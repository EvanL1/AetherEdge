use std::sync::Arc;
/// MQTT connection lifecycle and incoming-message dispatch.
///
/// Architecture:
/// - `run_mqtt_loop` runs forever, reconnecting whenever the connection drops
///   or `state.reconnect_signal` fires (triggered by config-change API).
/// - Incoming `Publish` events are dispatched to the appropriate handler
///   based on topic.
/// - A shared `Arc<Mutex<Option<AsyncClient>>>` in `AppState` is updated
///   every time a new connection is established, so other tasks can publish.
use std::sync::atomic::Ordering;

use anyhow::Context;
use bytes::Bytes;
use chrono::Utc;
use rumqttc::tokio_native_tls::native_tls::{Certificate, Identity, TlsConnector};
use rumqttc::{
    AsyncClient, Event, Incoming, LastWill, MqttOptions, NetworkOptions, QoS, TlsConfiguration,
    Transport,
};
use serde_json::json;
use tokio::time::{self, Duration};
use tokio_util::sync::CancellationToken;
use tracing::{Instrument, error, info, warn};

use crate::models::{
    CommandReply, InstSyncItem, InstSyncReply, ReadReply, ReadReplyProperty, ReadRequest,
    StatusPayload, WriteReply, WriteRequest,
};
use crate::state::AppState;
use crate::trace_context::{self, TraceParent};

// ── Public entry point ────────────────────────────────────────────────────────

pub async fn run_mqtt_loop(state: Arc<AppState>, shutdown: CancellationToken) {
    loop {
        let cfg = state.config.read().await.clone();
        let delay = Duration::from_secs(cfg.reconnect_delay_secs);

        match connect_and_run(Arc::clone(&state), shutdown.clone()).await {
            Ok(_) => {
                if shutdown.is_cancelled() {
                    info!("MQTT task shut down cleanly");
                    return;
                }
                info!("MQTT event loop exited");
            },
            Err(e) => {
                warn!("MQTT connection error: {}", e);
            },
        }

        if shutdown.is_cancelled() {
            return;
        }

        // If an explicit disconnect was requested, stay idle until a reconnect
        // signal arrives (do NOT auto-reconnect after the delay).
        if state
            .disconnect_requested
            .load(std::sync::atomic::Ordering::Relaxed)
        {
            info!("Disconnect requested – waiting for reconnect signal");
            state.mqtt_connected.store(false, Ordering::Relaxed);
            tokio::select! {
                _ = state.reconnect_signal.notified() => {}
                _ = shutdown.cancelled() => return,
            }
            // If still disconnected after wakeup, loop back and check again.
            continue;
        }

        // Normal auto-reconnect: wait for delay or an early trigger.
        tokio::select! {
            _ = time::sleep(delay) => {}
            _ = state.reconnect_signal.notified() => {
                info!("Reconnect signal received");
            }
            _ = shutdown.cancelled() => return,
        }
    }
}

// ── Inner connect + run ───────────────────────────────────────────────────────

async fn connect_and_run(state: Arc<AppState>, shutdown: CancellationToken) -> anyhow::Result<()> {
    let cfg = state.config.read().await.clone();

    // Resolve client ID
    let client_id = if cfg.client_id == "auto" {
        state.device.device_sn.clone()
    } else {
        cfg.client_id.clone()
    };

    let mut options = MqttOptions::new(&client_id, &cfg.broker_host, cfg.broker_port);
    options.set_keep_alive(Duration::from_secs(cfg.broker_keepalive_secs));
    options.set_clean_session(true);

    // MQTT username/password auth (only set when both are non-empty)
    if let (Some(user), Some(pass)) = (&cfg.username, &cfg.password)
        && !user.is_empty()
    {
        options.set_credentials(user, pass);
    }

    // Last Will Testament: broker publishes this automatically on unexpected disconnect.
    let lwt_payload = serde_json::to_string(&StatusPayload {
        msg_type: "offline".to_string(),
        gateway: state.device.device_sn.clone(),
        timestamp: Utc::now().timestamp(),
        reason: Some("unexpected".to_string()),
    })
    .unwrap_or_default();
    options.set_last_will(LastWill::new(
        &state.topics.status,
        lwt_payload.into_bytes(),
        QoS::AtLeastOnce,
        true,
    ));

    // TLS – cert_dir is fixed at startup from EnvConfig (not API-editable).
    // If ssl_enabled but cert loading fails, abort the connection attempt rather
    // than silently downgrading to plaintext.
    if cfg.ssl_enabled {
        let tls = build_tls(&state.env.cert_dir)?;
        options.set_transport(Transport::tls_with_config(tls));
    }

    let (client, mut event_loop) = AsyncClient::new(options, 64);
    // ARM64 设备无硬件加速时 rustls RSA 握手可能超过默认 5s，调大连接超时避免误报
    // NetworkOptions 是 rumqttc 0.24 设置连接超时的正确 API（不在 MqttOptions 上）
    let mut network_options = NetworkOptions::new();
    network_options.set_connection_timeout(30);
    event_loop.set_network_options(network_options);
    *state.mqtt_client.lock().await = Some(client.clone());

    info!("MQTT connecting to {}:{}", cfg.broker_host, cfg.broker_port);

    loop {
        tokio::select! {
            event_result = event_loop.poll() => {
                match event_result {
                    Ok(Event::Incoming(Incoming::ConnAck(ack))) => {
                        info!("MQTT connected (return_code={:?})", ack.code);
                        state.mqtt_connected.store(true, Ordering::Relaxed);
                        on_connected(&state, &client).await;
                    }
                    Ok(Event::Incoming(Incoming::Publish(p))) => {
                        let topic = p.topic.clone();
                        let payload = p.payload.clone();
                        let s = Arc::clone(&state);
                        tokio::spawn(async move {
                            dispatch_message(s, &topic, payload).await;
                        });
                    }
                    Ok(Event::Incoming(Incoming::Disconnect)) => {
                        state.mqtt_connected.store(false, Ordering::Relaxed);
                        info!("MQTT disconnected by broker");
                        return Ok(());
                    }
                    Ok(_) => {}
                    Err(e) => {
                        state.mqtt_connected.store(false, Ordering::Relaxed);
                        return Err(anyhow::anyhow!("MQTT poll error: {}", e));
                    }
                }
            }

            _ = state.reconnect_signal.notified() => {
                info!("Config changed, reconnecting MQTT");
                // Send graceful offline before disconnecting
                let _ = publish_status(&client, &state, "offline", Some("config_reload")).await;
                state.mqtt_connected.store(false, Ordering::Relaxed);
                return Ok(());
            }

            _ = shutdown.cancelled() => {
                let _ = publish_status(&client, &state, "offline", Some("graceful_shutdown")).await;
                let _ = client.disconnect().await;
                state.mqtt_connected.store(false, Ordering::Relaxed);
                info!("MQTT disconnected (graceful shutdown)");
                return Ok(());
            }
        }
    }
}

// ── Connection established ────────────────────────────────────────────────────

async fn on_connected(state: &AppState, client: &AsyncClient) {
    // Re-subscribe to all command topics
    for (topic, qos) in state.topics.subscriptions() {
        if let Err(e) = client.subscribe(topic, qos).await {
            error!("Subscribe '{}' failed: {}", topic, e);
        }
    }

    // Send online status
    let _ = publish_status(client, state, "online", None).await;
}

// ── Status message helper ─────────────────────────────────────────────────────

pub async fn publish_status(
    client: &AsyncClient,
    state: &AppState,
    msg_type: &str,
    reason: Option<&str>,
) -> anyhow::Result<()> {
    let payload = StatusPayload {
        msg_type: msg_type.to_string(),
        gateway: state.device.device_sn.clone(),
        timestamp: Utc::now().timestamp(),
        reason: reason.map(|s| s.to_string()),
    };
    let json = serde_json::to_string(&payload)?;
    client
        .publish(&state.topics.status, QoS::AtLeastOnce, true, json)
        .await?;
    Ok(())
}

/// Publish any JSON value to a topic.
pub async fn publish_json(
    state: &AppState,
    topic: &str,
    value: &impl serde::Serialize,
) -> anyhow::Result<()> {
    let guard = state.mqtt_client.lock().await;
    let client = guard
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("MQTT not connected"))?;
    let json = serde_json::to_string(value)?;
    client.publish(topic, QoS::AtLeastOnce, false, json).await?;
    Ok(())
}

// ── TLS configuration ─────────────────────────────────────────────────────────

fn build_tls(cert_dir: &str) -> anyhow::Result<TlsConfiguration> {
    let ca = std::fs::read(format!("{}/AmazonRootCA1.pem", cert_dir))
        .or_else(|_| std::fs::read(format!("{}/ca.pem", cert_dir)))
        .map_err(|e| anyhow::anyhow!("CA cert not found in {}: {}", cert_dir, e))?;

    let client_cert = std::fs::read(format!("{}/certificate.pem.crt", cert_dir))
        .or_else(|_| std::fs::read(format!("{}/client.crt", cert_dir)))
        .map_err(|e| anyhow::anyhow!("Client cert not found: {}", e))?;

    let client_key = std::fs::read(format!("{}/private.pem.key", cert_dir))
        .or_else(|_| std::fs::read(format!("{}/client.key", cert_dir)))
        .map_err(|e| anyhow::anyhow!("Client key not found: {}", e))?;

    let ca = Certificate::from_pem(&ca).context("parse MQTT CA certificate")?;
    let identity = Identity::from_pkcs8(&client_cert, &client_key)
        .context("parse MQTT client identity (the private key must be unencrypted PKCS#8 PEM)")?;

    let mut connector = TlsConnector::builder();
    connector.add_root_certificate(ca);
    connector.identity(identity);

    Ok(TlsConfiguration::NativeConnector(
        connector.build().context("build MQTT TLS connector")?,
    ))
}

// ── Incoming message dispatch ─────────────────────────────────────────────────

async fn dispatch_message(state: Arc<AppState>, topic: &str, payload: Bytes) {
    let t = &state.topics;

    if topic == t.read {
        handle_read(state, payload).await;
    } else if topic == t.write {
        handle_write(state, payload).await;
    } else if topic == t.call_data {
        handle_call_data(state, payload).await;
    } else if topic == t.call_alarm {
        handle_call_alarm(state, payload).await;
    } else if topic == t.inst_sync {
        handle_inst_sync(state, payload).await;
    }
}

// ── Command handlers ──────────────────────────────────────────────────────────

/// Correlation identifiers from a body parsed as raw JSON.
///
/// Used by the handlers that have no request struct (`call-data`, `call-alarm`,
/// `inst-sync`) and, on the typed paths, to salvage the ids from a body that
/// failed *schema* validation — a wrong type or a missing field. That request is
/// exactly the one whose error reply the caller most needs to correlate. A body
/// that is not JSON at all yields nothing, which is all that can be done.
fn correlation_from(payload: &Bytes) -> (Option<String>, Option<TraceParent>) {
    let Ok(body) = serde_json::from_slice::<serde_json::Value>(payload) else {
        return (None, None);
    };
    let msg_id = body
        .get("msgId")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string);
    (msg_id, trace_context::from_json(&body))
}

/// Builds the reply for a body that failed typed deserialization.
///
/// An absent `traceparent` is omitted rather than emitted as `null`: a cloud
/// that predates trace context must keep seeing the reply shape it already
/// parses, and `json!` would otherwise insert the key unconditionally.
fn parse_error_reply(payload: &Bytes, error: &serde_json::Error) -> serde_json::Value {
    let (msg_id, traceparent) = correlation_from(payload);
    let mut reply = json!({
        "result": "fail",
        "error": "json_parse_error",
        "message": format!("JSON parse error: {error}"),
        "msgId": msg_id.unwrap_or_else(|| "unknown".to_string()),
        "timestamp": Utc::now().timestamp(),
    });
    if let Some(traceparent) = traceparent {
        reply["traceparent"] = json!(traceparent);
    }
    reply
}

async fn handle_read(state: Arc<AppState>, payload: Bytes) {
    let req: ReadRequest = match serde_json::from_slice(&payload) {
        Ok(r) => r,
        Err(e) => {
            warn!("Bad read request: {}", e);
            let err_reply = parse_error_reply(&payload, &e);
            let _ = publish_json(&state, &state.topics.read_reply, &err_reply).await;
            return;
        },
    };

    // Pin the same immutable catalogue + committed SHM epoch for the complete read.
    let logical_device = req.device.replace('_', " ");
    let logical_key = format!("{}:{}:{}", req.source, logical_device, req.data_type);
    let generation = state.live_topology.load();
    let values = match generation.read_group(&logical_key, req.field.as_deref()) {
        Ok(Some(values)) if !values.is_empty() => values,
        Ok(_) => {
            warn!(
                "Read: logical point '{}' not found or unavailable",
                logical_key
            );
            return;
        },
        Err(error) => {
            error!(
                retryable = error.is_retryable(),
                "SHM cloud read failed for '{logical_key}': {error}"
            );
            return;
        },
    };
    let value = serde_json::Value::Object(values.into_iter().collect());

    let reply = ReadReply {
        timestamp: Utc::now().timestamp(),
        property: vec![ReadReplyProperty {
            source: req.source,
            device: req.device,
            data_type: req.data_type,
            value,
        }],
        msg_id: req.msg_id,
        traceparent: req.traceparent,
    };

    if let Err(e) = publish_json(&state, &state.topics.read_reply, &reply).await {
        error!("Failed to publish read-reply: {}", e);
    }
}

async fn handle_write(state: Arc<AppState>, payload: Bytes) {
    let req: WriteRequest = match serde_json::from_slice(&payload) {
        Ok(r) => r,
        Err(e) => {
            warn!("Bad write request: {}", e);
            let err_reply = parse_error_reply(&payload, &e);
            let _ = publish_json(&state, &state.topics.write_reply, &err_reply).await;
            return;
        },
    };

    let WriteRequest {
        source,
        device,
        data_type,
        field,
        value,
        msg_id,
        traceparent,
    } = req;

    let result_str = match numeric_command_value(&value).and_then(|value| {
        resolve_command_target(&source, &device, &data_type).map(|target| (target, value))
    }) {
        Ok((target, value)) => {
            // The command now fans out across loopback services to a device. This
            // is the hop whose latency the cloud cannot otherwise attribute, so it
            // is the one span worth naming.
            let span = tracing::info_span!(
                "cloud_command",
                traceparent = traceparent.as_ref().map_or("-", TraceParent::as_str)
            );
            match dispatch_cloud_command(
                &state,
                target,
                &field,
                value,
                msg_id.as_deref(),
                traceparent.as_ref(),
            )
            .instrument(span)
            .await
            {
                Ok(()) => "success",
                Err(error) => {
                    error!("Cloud command dispatch failed: {error}");
                    "fail"
                },
            }
        },
        Err(error) => {
            warn!("Rejected cloud write: {error}");
            "fail"
        },
    };

    let reply = WriteReply {
        result: result_str.to_string(),
        msg_id,
        traceparent,
    };

    if let Err(e) = publish_json(&state, &state.topics.write_reply, &reply).await {
        error!("Failed to publish write-reply: {}", e);
    }
}

async fn handle_call_data(state: Arc<AppState>, payload: Bytes) {
    let (msg_id, traceparent) = correlation_from(&payload);

    // Reply first, then trigger the upload so the cloud gets an ACK immediately.
    let reply = CommandReply {
        result: "success".to_string(),
        message: "数据总召已启动".to_string(),
        timestamp: Utc::now().timestamp(),
        msg_id,
        error: None,
        traceparent,
    };
    if let Err(e) = publish_json(&state, &state.topics.call_data_reply, &reply).await {
        error!("Failed to publish call-data-reply: {}", e);
    }

    crate::forwarder::upload_once(Arc::clone(&state)).await;
}

async fn handle_call_alarm(state: Arc<AppState>, payload: Bytes) {
    let (msg_id, traceparent) = correlation_from(&payload);

    let alarm_url = state.config.read().await.alarm_url.clone();
    let url = format!("{}/alarmApi/call-data", alarm_url);

    // POST to alarm with msgId + timestamp in body (matches Python uplink).
    let post_body = json!({
        "msgId": msg_id.as_deref().unwrap_or(""),
        "timestamp": Utc::now().timestamp()
    });

    let (result, message) = match state.http_client.post(&url).json(&post_body).send().await {
        Ok(resp) if resp.status().is_success() => {
            ("success".to_string(), "告警数据请求成功".to_string())
        },
        Ok(resp) => {
            let msg = format!("告警API返回状态码: {}", resp.status());
            warn!("call-alarm: {}", msg);
            ("warning".to_string(), msg)
        },
        Err(e) => {
            let msg = format!("告警API调用失败: {}", e);
            warn!("call-alarm: {}", msg);
            ("fail".to_string(), msg)
        },
    };

    let reply = CommandReply {
        result,
        message,
        timestamp: Utc::now().timestamp(),
        msg_id,
        error: None,
        traceparent,
    };
    if let Err(e) = publish_json(&state, &state.topics.call_alarm_reply, &reply).await {
        error!("Failed to publish call-alarm-reply: {}", e);
    }
}

async fn handle_inst_sync(state: Arc<AppState>, payload: Bytes) {
    let (msg_id, traceparent) = correlation_from(&payload);

    if let Err(e) = do_inst_sync(Arc::clone(&state), msg_id, traceparent).await {
        error!("inst-sync failed: {}", e);
    }
}

/// Fetch instance list from automation and publish an `inst-sync-reply`.
/// `msg_id` is echoed back verbatim; pass the ms-timestamp string for
/// HTTP-triggered calls.
pub async fn do_inst_sync(
    state: Arc<AppState>,
    msg_id: Option<String>,
    traceparent: Option<TraceParent>,
) -> anyhow::Result<()> {
    let automation_url = state.config.read().await.automation_url.clone();
    let url = format!("{}/api/instances?page_size=100", automation_url);

    let list = match state.http_client.get(&url).send().await {
        Ok(resp) if resp.status().is_success() => match resp.json::<serde_json::Value>().await {
            Ok(body) => {
                let raw_list = body
                    .get("data")
                    .and_then(|d| d.get("list"))
                    .and_then(|l| l.as_array())
                    .cloned()
                    .unwrap_or_default();

                raw_list
                    .into_iter()
                    .filter_map(|item| {
                        let instance_id = item.get("instance_id")?.as_i64()?;
                        let instance_name = item.get("instance_name")?.as_str()?.to_string();
                        let product_name = item.get("product_name")?.as_str()?.to_string();
                        Some(InstSyncItem {
                            instance_id,
                            instance_name,
                            product_name,
                        })
                    })
                    .collect::<Vec<_>>()
            },
            Err(e) => {
                return Err(anyhow::anyhow!("parse automation response: {}", e));
            },
        },
        Ok(resp) => {
            return Err(anyhow::anyhow!(
                "automation returned status {}",
                resp.status()
            ));
        },
        Err(e) => {
            return Err(anyhow::anyhow!("HTTP request to automation: {}", e));
        },
    };

    let reply = InstSyncReply {
        msg_id,
        timestamp: Utc::now().timestamp(),
        list,
        traceparent,
    };

    publish_json(&state, &state.topics.inst_sync_reply, &reply).await
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn resolve_command_target(source: &str, device: &str, data_type: &str) -> anyhow::Result<u32> {
    let device = device.replace('_', " ");
    let id = device
        .parse::<u32>()
        .with_context(|| format!("cloud command device must be a numeric id, got '{device}'"))?;
    if source.eq_ignore_ascii_case("inst") && data_type.eq_ignore_ascii_case("A") {
        return Ok(id);
    }
    anyhow::bail!("unsupported command plane {source}:{data_type}; only inst:A is writable")
}

fn numeric_command_value(value: &serde_json::Value) -> anyhow::Result<f64> {
    let value = value
        .as_f64()
        .or_else(|| value.as_str().and_then(|value| value.parse().ok()))
        .ok_or_else(|| anyhow::anyhow!("command value must be numeric"))?;
    if !value.is_finite() {
        anyhow::bail!("command value must be finite");
    }
    Ok(value)
}

async fn dispatch_cloud_command(
    state: &AppState,
    instance_id: u32,
    field: &str,
    value: f64,
    message_id: Option<&str>,
    traceparent: Option<&TraceParent>,
) -> anyhow::Result<()> {
    let config = state.config.read().await.clone();
    let (url, body) = command_request(&config, instance_id, field, value);
    let control_token = state.env.control_token.as_deref().ok_or_else(|| {
        anyhow::anyhow!(
            "AETHER_UPLINK_CONTROL_TOKEN is missing or weaker than 32 bytes; cloud device control is disabled"
        )
    })?;
    let mut request = state
        .http_client
        .post(url)
        .header("authorization", format!("AetherService {control_token}"))
        .json(&body);
    if let Some(message_id) = message_id {
        request = request.header("Idempotency-Key", message_id);
    }
    if let Some(traceparent) = traceparent {
        // Safe as a header value only because `TraceParent` is parsed, not
        // passed through: the grammar admits no CR/LF (see `trace_context`).
        request = request.header("traceparent", traceparent.as_str());
    }
    let response = request.send().await?;
    if !response.status().is_success() {
        anyhow::bail!("command service returned HTTP {}", response.status());
    }
    Ok(())
}

fn command_request(
    config: &crate::models::NetConfig,
    instance_id: u32,
    field: &str,
    value: f64,
) -> (String, serde_json::Value) {
    (
        format!(
            "{}/api/instances/{instance_id}/action",
            config.automation_url.trim_end_matches('/')
        ),
        json!({"point_id": field, "value": value, "confirmed": true}),
    )
}

#[cfg(test)]
mod tests {
    use super::{build_tls, command_request, parse_error_reply, resolve_command_target};
    use bytes::Bytes;

    const TP: &str = "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01";

    fn parse_failure() -> serde_json::Error {
        serde_json::from_str::<super::ReadRequest>("{").expect_err("body is not valid JSON")
    }

    /// A cloud that predates trace context must keep receiving the exact error
    /// reply shape it already parses. `json!` inserts keys unconditionally, so an
    /// absent `traceparent` would otherwise arrive as a new `null` key — the same
    /// break the typed replies guard against with `skip_serializing_if`.
    #[test]
    fn a_parse_error_reply_gains_no_null_traceparent_key() {
        let payload = Bytes::from_static(br#"{"msgId":"m-1","source":}"#);
        let reply = parse_error_reply(&payload, &parse_failure());

        assert!(
            reply.get("traceparent").is_none(),
            "no null key emitted, got {reply}"
        );
    }

    /// The realistic failure: the body is valid JSON but does not satisfy the
    /// request schema — a wrong type, a missing field. That request is precisely
    /// the one whose error reply the caller most needs to match up, so the
    /// correlation ids are salvaged from the raw body rather than thrown away.
    #[test]
    fn a_schema_invalid_body_still_yields_its_correlation_ids() {
        let payload = Bytes::from(format!(
            r#"{{"msgId":"m-1","traceparent":"{TP}","source":42}}"#
        ));
        let reply = parse_error_reply(&payload, &parse_failure());

        assert_eq!(reply["msgId"], "m-1");
        assert_eq!(reply["traceparent"], TP);
        assert_eq!(reply["result"], "fail");
    }

    /// A body that is not JSON at all yields nothing to salvage — there is no
    /// object to read the ids out of. The reply must still be well formed.
    #[test]
    fn a_syntactically_broken_body_still_produces_a_well_formed_error_reply() {
        let payload = Bytes::from_static(b"not json at all");
        let reply = parse_error_reply(&payload, &parse_failure());

        assert_eq!(reply["msgId"], "unknown");
        assert_eq!(reply["result"], "fail");
        assert!(reply.get("traceparent").is_none());
    }
    use crate::models::NetConfig;

    #[test]
    fn tls_material_is_parsed_before_a_connection_is_attempted() {
        let directory = tempfile::tempdir().expect("temporary certificate directory");
        std::fs::write(directory.path().join("ca.pem"), "not a certificate")
            .expect("write CA fixture");
        std::fs::write(directory.path().join("client.crt"), "not a certificate")
            .expect("write client certificate fixture");
        std::fs::write(directory.path().join("client.key"), "not a private key")
            .expect("write client key fixture");

        assert!(build_tls(directory.path().to_str().expect("UTF-8 path")).is_err());
    }

    #[test]
    fn cloud_writes_resolve_only_to_explicit_command_planes() {
        assert_eq!(
            resolve_command_target("inst", "12", "A").expect("instance action"),
            12
        );
        assert!(resolve_command_target("inst", "12", "M").is_err());
        assert!(resolve_command_target("io", "10", "T").is_err());
    }

    #[test]
    fn cloud_commands_use_existing_safety_checked_service_apis() {
        let config = NetConfig::default();
        let (instance_url, instance_body) = command_request(&config, 12, "5", 42.5);

        assert_eq!(
            instance_url,
            "http://localhost:6002/api/instances/12/action"
        );
        assert_eq!(
            instance_body,
            serde_json::json!({"point_id": "5", "value": 42.5, "confirmed": true})
        );
        assert!(resolve_command_target("io", "10", "C").is_err());
    }
}
