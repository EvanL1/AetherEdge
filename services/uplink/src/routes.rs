use std::sync::Arc;
use std::sync::atomic::Ordering;

use axum::{
    Router,
    extract::{Multipart, Path, State},
    http::StatusCode,
    response::Json,
    routing::{delete, get, post},
};
use serde_json::{Value, json};
use tracing::error;
#[cfg(feature = "swagger-ui")]
use utoipa::OpenApi;
#[cfg(feature = "swagger-ui")]
use utoipa_swagger_ui::{Config, SwaggerUi};

use crate::db_config;
#[cfg(feature = "swagger-ui")]
use crate::models::SystemMetrics;
use crate::models::{AlarmBroadcastRequest, CertUploadForm, NetConfig};
use crate::mqtt::do_inst_sync;
use crate::state::AppState;
use crate::uplink::enqueue_json;

// ============================================================================
// Router
// ============================================================================

pub fn build_router(state: Arc<AppState>) -> Router {
    let api = Router::new()
        .route("/", get(root))
        .route("/ping", get(ping))
        .route("/netApi/health", get(health))
        // Alarm
        .route("/netApi/alarm/broadcast", post(alarm_broadcast))
        .route("/netApi/alarm/config", get(alarm_config))
        // MQTT
        .route("/netApi/mqtt/config", get(mqtt_get_config).post(mqtt_update_config))
        .route("/netApi/mqtt/status", get(mqtt_status))
        .route("/netApi/mqtt/disconnect", post(mqtt_disconnect))
        .route("/netApi/mqtt/reconnect", post(mqtt_reconnect))
        // Certificate
        .route("/netApi/certificate/upload", post(cert_upload))
        .route("/netApi/certificate/info", get(cert_info))
        .route("/netApi/certificate/{cert_type}", delete(cert_delete))
        // Device sync
        .route("/netApi/inst-sync", post(inst_sync_push))
        // Admin API (shared endpoints from common lib)
        .route("/api/admin/logs/level", get(common::admin_api::get_log_level).post(common::admin_api::set_log_level))
        .route("/api/admin/logs/files", get(common::admin_api::list_log_files))
        .route("/api/admin/logs/view", get(common::admin_api::view_log_file))
        .with_state(state);

    #[cfg(feature = "swagger-ui")]
    let api = api.merge(
        SwaggerUi::new("/docs")
            .url("/openapi.json", ApiDoc::openapi())
            .config(
                Config::default()
                    .default_model_rendering("model")
                    .default_models_expand_depth(1),
            ),
    );

    api
}

// ============================================================================
// OpenAPI document (only consumed when swagger-ui feature is enabled)
// ============================================================================

#[cfg(feature = "swagger-ui")]
#[derive(OpenApi)]
#[openapi(
    paths(
        root,
        ping,
        health,
        alarm_broadcast,
        alarm_config,
        inst_sync_push,
        mqtt_get_config,
        mqtt_update_config,
        mqtt_status,
        mqtt_disconnect,
        mqtt_reconnect,
        cert_upload,
        cert_info,
        cert_delete,
        common::admin_api::get_log_level,
        common::admin_api::set_log_level,
        common::admin_api::list_log_files,
        common::admin_api::view_log_file,
    ),
    components(schemas(
        NetConfig,
        AlarmBroadcastRequest,
        CertUploadForm,
        SystemMetrics,
        crate::models::UplinkDataResponse<NetConfig>,
        crate::models::AlarmQueuedResponse,
        common::admin_api::SetLogLevelRequest,
        common::admin_api::LogLevelResponse,
    )),
    tags(
        (name = "Health",      description = "Health checks and service information"),
        (name = "Alarm",       description = "Alarm broadcast and configuration"),
        (name = "MQTT",        description = "MQTT connection configuration and control"),
        (name = "Certificate", description = "TLS certificate management"),
        (name = "admin",       description = "Host-local service administration"),
    ),
    info(
        title = "Aether Uplink Service API",
        version = env!("CARGO_PKG_VERSION"),
        description = "Internal loopback API for MQTT delivery, certificates, and cloud-to-edge coordination. Device actions are delegated through authenticated automation; direct I/O writes are rejected. Do not expose this service port remotely."
    )
)]
pub struct ApiDoc;

#[cfg(all(test, feature = "swagger-ui"))]
mod openapi_tests {
    use super::*;

    #[test]
    fn openapi_metadata_and_admin_routes_match_the_router() {
        let specification = serde_json::to_value(ApiDoc::openapi()).expect("serialize OpenAPI");
        assert_eq!(specification["info"]["title"], "Aether Uplink Service API");
        assert_eq!(specification["info"]["version"], env!("CARGO_PKG_VERSION"));
        for (path, method) in [
            ("/api/admin/logs/level", "get"),
            ("/api/admin/logs/level", "post"),
            ("/api/admin/logs/files", "get"),
            ("/api/admin/logs/view", "get"),
        ] {
            assert!(
                specification["paths"][path][method].is_object(),
                "missing {method} {path}"
            );
        }
        let operation_count = specification["paths"]
            .as_object()
            .expect("paths object")
            .values()
            .map(|item| {
                item.as_object()
                    .expect("path item")
                    .keys()
                    .filter(|method| {
                        matches!(
                            method.as_str(),
                            "get"
                                | "put"
                                | "post"
                                | "delete"
                                | "patch"
                                | "options"
                                | "head"
                                | "trace"
                        )
                    })
                    .count()
            })
            .sum::<usize>();
        assert_eq!(operation_count, 18, "Router/OpenAPI operation drift");
    }

    #[test]
    fn openapi_redacts_mqtt_secrets_and_documents_durable_queue_semantics() {
        let specification = serde_json::to_value(ApiDoc::openapi()).expect("serialize OpenAPI");

        let password_schema =
            &specification["components"]["schemas"]["NetConfig"]["properties"]["password"];
        assert!(
            password_schema.to_string().contains("\"writeOnly\":true"),
            "password schema must be write-only: {password_schema}"
        );
        let mqtt_get = &specification["paths"]["/netApi/mqtt/config"]["get"]["responses"]["200"]["content"]
            ["application/json"]["schema"];
        assert!(mqtt_get.to_string().contains("UplinkDataResponse"));

        let alarm = &specification["paths"]["/netApi/alarm/broadcast"]["post"]["responses"];
        assert!(alarm["200"].to_string().contains("AlarmQueuedResponse"));
        assert!(
            alarm["503"]["description"]
                .as_str()
                .expect("503 description")
                .contains("outbox")
        );
        assert!(
            specification["paths"]["/netApi/certificate/upload"]["post"]["responses"]["413"]
                .is_object()
        );
    }
}

// ============================================================================
// Root / ping
// ============================================================================

/// uplink service banner.
///
/// Returns service name, version, and status. Use this to confirm the uplink
/// process is online and running the expected version. Does not depend on the
/// MQTT connection — returns 200 even if the broker is unreachable. For MQTT
/// status see `/netApi/health` or `/netApi/mqtt/status`.
#[utoipa::path(get, path = "/", tag = "Health",
    responses((status = 200, description = "Basic service information")))]
async fn root() -> Json<Value> {
    Json(json!({
        "service": "aether-uplink",
        "version": env!("CARGO_PKG_VERSION"),
        "status": "running"
    }))
}

/// Minimal liveness probe — returns the string "pong".
///
/// Unlike `/`, the response body is a plain string with no JSON overhead,
/// suitable for high-frequency liveness probes and load-balancer health checks.
#[utoipa::path(get, path = "/ping", tag = "Health",
    responses((status = 200, description = "pong")))]
async fn ping() -> &'static str {
    "pong"
}

// ============================================================================
// Health
// ============================================================================

/// Health check: returns MQTT connection status and device identity.
///
/// Reflects the live MQTT broker connection state (not a cached value). Returns
/// `mqtt_connected` (bool), broker address, and device `client_id`. When the
/// process is alive but MQTT is not connected, responds 200 with
/// `connected: false` — allowing dashboards to distinguish a dead process from
/// a live process with a broken cloud link.
#[utoipa::path(get, path = "/netApi/health", tag = "Health",
    responses((status = 200, description = "MQTT connection status and device identity")))]
async fn health(State(state): State<Arc<AppState>>) -> Json<Value> {
    let mqtt_ok = state.mqtt_connected.load(Ordering::Relaxed);
    Json(json!({
        "success": mqtt_ok,
        "message": if mqtt_ok { "MQTT 连接正常" } else { "MQTT 未连接" },
        "data": {
            "mqtt_connected": mqtt_ok,
            "product_sn":     state.device.product_sn,
            "device_sn":      state.device.device_sn,
        }
    }))
}

// ============================================================================
// Alarm
// ============================================================================

/// Forward an alarm JSON payload to the MQTT alarm topic.
///
/// The request body is an arbitrary JSON object; content is not validated and
/// is published as-is to the configured alarm topic (see `GET /netApi/alarm/config`
/// for the topic name). The cloud subscriber is responsible for parsing the
/// payload. Alarm events from upstream alarm travel this path to the cloud.
/// The payload is durably queued before the call returns, so temporary MQTT
/// disconnection does not discard the alarm. A full local queue returns 503.
#[utoipa::path(post, path = "/netApi/alarm/broadcast", tag = "Alarm",
    request_body = AlarmBroadcastRequest,
    responses(
        (status = 200, description = "Alarm durably queued in the local outbox", body = crate::models::AlarmQueuedResponse),
        (status = 503, description = "The local durable outbox is full or unavailable"),
    ))]
async fn alarm_broadcast(
    State(state): State<Arc<AppState>>,
    Json(AlarmBroadcastRequest(body)): Json<AlarmBroadcastRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    enqueue_json(&state, &state.topics.alarm, &body)
        .await
        .map(|id| {
            Json(json!({
                "success": true,
                "message": "Alarm queued",
                "outbox_id": id.get(),
            }))
        })
        .map_err(|e| {
            error!("Alarm broadcast failed: {}", e);
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({"success": false, "message": e.to_string()})),
            )
        })
}

/// Retrieve alarm cloud-forwarding configuration (topic name and MQTT status).
///
/// Returns the MQTT topic used for alarm broadcasts (e.g.
/// `aetherems/alarm/{device_id}`) and whether the MQTT connection is currently
/// online. Useful for the cloud-config UI to confirm where alarms are sent and
/// whether the link is healthy.
#[utoipa::path(get, path = "/netApi/alarm/config", tag = "Alarm",
    responses((status = 200, description = "Alarm topic name and MQTT connection status")))]
async fn alarm_config(State(state): State<Arc<AppState>>) -> Json<Value> {
    Json(json!({
        "success": true,
        "message": "OK",
        "data": {
            "alarm_topic":    state.topics.alarm,
            "mqtt_connected": state.mqtt_connected.load(Ordering::Relaxed),
        }
    }))
}

// ============================================================================
// MQTT config
// ============================================================================

/// Retrieve current MQTT connection and forwarding configuration.
///
/// Read-only. Returns broker, client, TLS, reconnect, and forwarding settings.
/// The MQTT password is write-only and is never returned. On update, omit it to
/// preserve the stored password or send an empty string to clear it. Certificate material
/// is managed through `/netApi/certificate/*`; topics are derived from device
/// identity. To update connection settings use `POST /netApi/mqtt/config`.
#[utoipa::path(get, path = "/netApi/mqtt/config", tag = "MQTT",
    responses((status = 200, description = "Current MQTT configuration with password omitted", body = crate::models::UplinkDataResponse<NetConfig>)))]
async fn mqtt_get_config(State(state): State<Arc<AppState>>) -> Json<Value> {
    let cfg = state.config.read().await.clone();
    Json(json!({ "success": true, "message": "OK", "data": cfg }))
}

/// Update MQTT configuration and immediately trigger a reconnect — no service restart needed.
///
/// Persists the new configuration, then disconnects the current MQTT session and
/// reconnects with the new parameters. There will be a brief MQTT unavailability
/// window of a few seconds; events already accepted into the local outbox remain
/// queued. Use this endpoint to change broker, authentication, TLS enablement,
/// reconnect, and forwarding settings. Certificate files use the dedicated
/// certificate endpoints. If the new parameters are invalid and the connection
/// fails, uplink remains disconnected until a correct configuration is submitted.
#[utoipa::path(post, path = "/netApi/mqtt/config", tag = "MQTT",
    request_body = NetConfig,
    responses(
        (status = 200, description = "Configuration saved; reconnecting"),
        (status = 500, description = "Failed to save configuration"),
    ))]
async fn mqtt_update_config(
    State(state): State<Arc<AppState>>,
    Json(mut new_cfg): Json<NetConfig>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let current_cfg = state.config.read().await.clone();
    new_cfg.preserve_write_only_secrets_from(&current_cfg);
    // Without this the in-memory copy bypasses load_config() and chunks(0) can panic.
    new_cfg.normalize();
    if let Err(e) = db_config::save_config(&state.sqlite, &new_cfg).await {
        error!("Save config failed: {}", e);
        return Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"success": false, "message": e.to_string()})),
        ));
    }
    *state.config.write().await = new_cfg;
    state.reconnect_signal.notify_one();
    Ok(Json(
        json!({"success": true, "message": "Config updated, reconnecting"}),
    ))
}

/// Real-time MQTT connection status (polling endpoint).
///
/// Returns connected/disconnected state, broker address, TLS flag, and device
/// identity. Intended for the cloud-status indicator on the operations dashboard.
/// More detailed than `/netApi/health` but with the same update frequency (no
/// background cache).
#[utoipa::path(get, path = "/netApi/mqtt/status", tag = "MQTT",
    responses((status = 200, description = "Current MQTT connection state, broker address, and device identity")))]
async fn mqtt_status(State(state): State<Arc<AppState>>) -> Json<Value> {
    let cfg = state.config.read().await;
    Json(json!({
        "success": true,
        "message": "OK",
        "data": {
            "connected":  state.mqtt_connected.load(Ordering::Relaxed),
            "broker":     format!("{}:{}", cfg.broker_host, cfg.broker_port),
            "ssl":        cfg.ssl_enabled,
            "product_sn": state.device.product_sn,
            "device_sn":  state.device.device_sn,
        }
    }))
}

/// Manually disconnect MQTT and suspend automatic reconnection.
///
/// Counterpart to `POST /netApi/mqtt/reconnect`. Closes the current MQTT
/// session and sets a "reconnect inhibit" flag — uplink will not attempt to
/// reconnect even if the broker is reachable, until `reconnect` is explicitly
/// called. Intended for maintenance windows such as broker upgrades or
/// temporarily suppressing cloud alarm forwarding.
#[utoipa::path(post, path = "/netApi/mqtt/disconnect", tag = "MQTT",
    responses((status = 200, description = "MQTT disconnected; auto-reconnect suspended until reconnect is called")))]
async fn mqtt_disconnect(State(state): State<Arc<AppState>>) -> Json<Value> {
    // Mark disconnection intent first, then wake the mqtt loop so it stops reconnecting.
    state
        .disconnect_requested
        .store(true, std::sync::atomic::Ordering::Relaxed);
    // Drop the current client to force the event loop to exit.
    *state.mqtt_client.lock().await = None;
    state
        .mqtt_connected
        .store(false, std::sync::atomic::Ordering::Relaxed);
    state.reconnect_signal.notify_one();
    Json(json!({"success": true, "message": "MQTT disconnected, auto-reconnect paused"}))
}

/// Trigger MQTT reconnection and resume automatic reconnection.
///
/// Counterpart to `POST /netApi/mqtt/disconnect`. Clears the reconnect-inhibit
/// flag and immediately schedules a connection attempt. A 200 response does not
/// mean the connection succeeded — reconnection runs asynchronously in the
/// background; poll `GET /netApi/mqtt/status` to confirm. Call this after a
/// maintenance window to restore cloud link.
#[utoipa::path(post, path = "/netApi/mqtt/reconnect", tag = "MQTT",
    responses((status = 200, description = "Reconnect command issued; executing in background")))]
async fn mqtt_reconnect(State(state): State<Arc<AppState>>) -> Json<Value> {
    // Clear the disconnect flag, then wake the mqtt loop to reconnect.
    state
        .disconnect_requested
        .store(false, std::sync::atomic::Ordering::Relaxed);
    state.reconnect_signal.notify_one();
    Json(json!({"success": true, "message": "Reconnect command sent, executing in background"}))
}

// ============================================================================
// Certificate management
// ============================================================================

/// Upload a single TLS certificate file (multipart/form-data).
///
/// Upload one certificate per request; use `cert_type` to specify the type.
/// The original filename is ignored — files are saved under fixed names in
/// `cert_dir`:
///
/// | cert_type     | Saved filename         |
/// |---------------|------------------------|
/// | `ca_cert`     | `AmazonRootCA1.pem`    |
/// | `client_cert` | `certificate.pem.crt`  |
/// | `client_key`  | `private.pem.key`      |
#[utoipa::path(post, path = "/netApi/certificate/upload", tag = "Certificate",
    request_body(
        content_type = "multipart/form-data",
        content = inline(CertUploadForm),
    ),
    responses(
        (status = 200, description = "Certificate uploaded successfully"),
        (status = 400, description = "Invalid request — unknown cert_type, empty file, or unsupported format"),
        (status = 413, description = "Multipart request exceeds the service body limit"),
        (status = 500, description = "Certificate directory not writable or file write failed"),
    ))]
async fn cert_upload(
    State(state): State<Arc<AppState>>,
    mut multipart: Multipart,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    const MAX_SIZE: usize = 1024 * 1024; // 1 MB
    const ALLOWED_EXT: &[&str] = &[".pem", ".crt", ".key", ".cer", ".p12", ".pfx"];

    let cert_dir = state.env.cert_dir.clone();

    // Collect all multipart fields first.
    let mut cert_type_val: Option<String> = None;
    let mut file_data: Option<(String, Vec<u8>)> = None; // (original_filename, bytes)

    while let Some(field) = multipart.next_field().await.unwrap_or(None) {
        let name = field.name().unwrap_or("").to_string();
        match name.as_str() {
            "cert_type" => {
                let text = field.text().await.map_err(|e| {
                    (
                        StatusCode::BAD_REQUEST,
                        Json(json!({"success": false, "message": e.to_string()})),
                    )
                })?;
                cert_type_val = Some(text);
            },
            "file" => {
                let orig_name = field.file_name().unwrap_or("").to_string();
                let data = field.bytes().await.map_err(|e| {
                    (
                        StatusCode::BAD_REQUEST,
                        Json(json!({"success": false, "message": e.to_string()})),
                    )
                })?;
                file_data = Some((orig_name, data.to_vec()));
            },
            _ => {},
        }
    }

    // Validate cert_type.
    let cert_type = cert_type_val.ok_or_else(|| (
        StatusCode::BAD_REQUEST,
        Json(json!({"success": false, "message": "Missing cert_type field. Valid values: ca_cert | client_cert | client_key"})),
    ))?;

    let save_name = match cert_type.as_str() {
        "ca_cert" => "AmazonRootCA1.pem",
        "client_cert" => "certificate.pem.crt",
        "client_key" => "private.pem.key",
        _ => {
            return Err((
                StatusCode::BAD_REQUEST,
                Json(
                    json!({"success": false, "message": format!("Unsupported cert_type: '{}'. Valid: ca_cert | client_cert | client_key", cert_type)}),
                ),
            ));
        },
    };

    // Validate file.
    let (orig_name, data) = file_data.ok_or_else(|| {
        (
            StatusCode::BAD_REQUEST,
            Json(json!({"success": false, "message": "Missing file field"})),
        )
    })?;

    if orig_name.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!({"success": false, "message": "Filename cannot be empty"})),
        ));
    }

    let ext = std::path::Path::new(&orig_name)
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| format!(".{}", e.to_lowercase()))
        .unwrap_or_default();
    if !ALLOWED_EXT.contains(&ext.as_str()) {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!({
                "success": false,
                "message": format!("Unsupported file format '{}'. Supported: {}", ext, ALLOWED_EXT.join(", "))
            })),
        ));
    }

    if data.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!({"success": false, "message": "File content is empty"})),
        ));
    }
    if data.len() > MAX_SIZE {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!({
                "success": false,
                "message": format!("File exceeds 1MB limit (current: {} bytes)", data.len())
            })),
        ));
    }

    // Ensure directory exists.
    std::fs::create_dir_all(&cert_dir).map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({
                "success": false,
                "message": format!("Cannot create cert directory '{}': {} (check path permissions)", cert_dir, e)
            })),
        )
    })?;

    // Save file.
    let dest = format!("{}/{}", cert_dir, save_name);
    std::fs::write(&dest, &data).map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"success": false, "message": format!("Failed to write file: {}", e)})),
        )
    })?;

    Ok(Json(json!({
        "success": true,
        "message": "Certificate uploaded",
        "data": {
            "cert_type": cert_type,
            "saved_as":  save_name,
            "path":      dest,
        }
    })))
}

/// List certificate directory status: path and per-file existence flags.
///
/// Checks whether the CA certificate, client certificate, and private key are
/// present in the configured certificate directory. Certificate contents and
/// fingerprints are never returned (to avoid private-key exposure) — only
/// `exists: true/false` per file. Use this on the cloud-config pre-flight page
/// to confirm all required certificates have been uploaded.
#[utoipa::path(get, path = "/netApi/certificate/info", tag = "Certificate",
    responses((status = 200, description = "Certificate directory path and per-file existence status")))]
async fn cert_info(State(state): State<Arc<AppState>>) -> Json<Value> {
    let cert_dir = state.env.cert_dir.clone();
    let files = [
        "AmazonRootCA1.pem",
        "certificate.pem.crt",
        "private.pem.key",
    ];
    let info: Vec<Value> = files
        .iter()
        .map(|f| {
            let exists = std::path::Path::new(&format!("{}/{}", cert_dir, f)).exists();
            json!({ "file": f, "exists": exists })
        })
        .collect();
    Json(json!({
        "success": true,
        "message": "OK",
        "data": {
            "cert_dir": cert_dir,
            "files":    info,
        }
    }))
}

/// Delete a certificate file by type.
///
/// `cert_type` must be one of: `ca_cert` / `client_cert` / `client_key`.
#[utoipa::path(delete, path = "/netApi/certificate/{cert_type}", tag = "Certificate",
    params(
        ("cert_type" = String, Path, description = "Certificate type: ca_cert | client_cert | client_key")
    ),
    responses(
        (status = 200, description = "Deleted successfully (also returned when the file did not exist)"),
        (status = 400, description = "Unknown cert_type"),
        (status = 500, description = "Delete failed"),
    ))]
async fn cert_delete(
    State(state): State<Arc<AppState>>,
    Path(cert_type): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let cert_dir = state.env.cert_dir.clone();
    let filename = match cert_type.as_str() {
        "ca_cert" => "AmazonRootCA1.pem",
        "client_cert" => "certificate.pem.crt",
        "client_key" => "private.pem.key",
        _ => {
            return Err((
                StatusCode::BAD_REQUEST,
                Json(
                    json!({"success": false, "message": "Unknown cert_type. Valid: ca_cert | client_cert | client_key"}),
                ),
            ));
        },
    };

    match std::fs::remove_file(format!("{}/{}", cert_dir, filename)) {
        Ok(_) => Ok(Json(json!({
            "success": true,
            "message": "Deleted successfully",
            "data": { "deleted": filename }
        }))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Json(
            json!({"success": true, "message": "File does not exist, nothing to delete"}),
        )),
        Err(e) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"success": false, "message": e.to_string()})),
        )),
    }
}

// ============================================================================
// Device sync
// ============================================================================

/// 主动向平台发送设备列表同步消息（inst-sync-reply）。
///
/// msgId 自动设为当前毫秒级时间戳，数据从 automation 实时拉取。
#[utoipa::path(post, path = "/netApi/inst-sync", tag = "MQTT",
    responses(
        (status = 200, description = "已发布 inst-sync-reply"),
        (status = 503, description = "MQTT 未连接或 automation 不可达"),
    ))]
async fn inst_sync_push(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let msg_id = chrono::Utc::now().timestamp_millis().to_string();

    do_inst_sync(Arc::clone(&state), Some(msg_id.clone()))
        .await
        .map(|_| {
            Json(json!({
                "success": true,
                "message": "inst-sync-reply published",
                "data": { "msgId": msg_id }
            }))
        })
        .map_err(|e| {
            error!("inst-sync-push failed: {}", e);
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({"success": false, "message": e.to_string()})),
            )
        })
}
