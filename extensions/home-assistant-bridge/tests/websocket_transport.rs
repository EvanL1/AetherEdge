use std::sync::Arc;
use std::time::Duration;

use aether_home_assistant_bridge::{
    HomeAssistantConnectionConfig, HomeAssistantTransport, WebSocketHomeAssistantTransport,
};
use aether_ports::{
    PortError, PortErrorKind, PortResult, SecretMaterial, SecretRef, SecretResolver,
};
use async_trait::async_trait;
use futures::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::{WebSocketStream, accept_async, tungstenite::Message};

const ACCESS_TOKEN: &str = "test-token-that-must-never-be-rendered";

struct StaticResolver;

#[async_trait]
impl SecretResolver for StaticResolver {
    async fn resolve(&self, reference: &SecretRef) -> PortResult<SecretMaterial> {
        assert_eq!(reference.as_str(), "test:home-assistant");
        SecretMaterial::new(ACCESS_TOKEN)
    }
}

async fn read_json(socket: &mut WebSocketStream<TcpStream>) -> Value {
    let message = socket
        .next()
        .await
        .expect("client frame")
        .expect("valid client frame");
    serde_json::from_str(message.to_text().expect("text frame")).expect("client json")
}

async fn send_json(socket: &mut WebSocketStream<TcpStream>, value: Value) {
    socket
        .send(Message::Text(value.to_string().into()))
        .await
        .expect("server response");
}

async fn run_mock_home_assistant(listener: TcpListener) {
    let (stream, _) = listener.accept().await.expect("client connection");
    let mut socket = accept_async(stream).await.expect("websocket handshake");

    send_json(
        &mut socket,
        json!({"type": "auth_required", "ha_version": "2026.7.2"}),
    )
    .await;
    let auth = read_json(&mut socket).await;
    assert_eq!(auth["type"], "auth");
    assert_eq!(auth["access_token"], ACCESS_TOKEN);
    send_json(
        &mut socket,
        json!({"type": "auth_ok", "ha_version": "2026.7.2"}),
    )
    .await;

    let features = read_json(&mut socket).await;
    assert_eq!(features["type"], "supported_features");
    send_json(
        &mut socket,
        json!({"id": features["id"], "type": "result", "success": true, "result": null}),
    )
    .await;

    let subscription = read_json(&mut socket).await;
    assert_eq!(subscription["type"], "subscribe_events");
    assert!(
        subscription.get("event_type").is_none(),
        "registry changes require an all-events subscription"
    );
    let subscription_id = subscription["id"].clone();
    send_json(
        &mut socket,
        json!({"id": subscription_id, "type": "result", "success": true, "result": null}),
    )
    .await;

    loop {
        let command = read_json(&mut socket).await;
        let id = command["id"].clone();
        let result = match command["type"].as_str().expect("command type") {
            "config/area_registry/list" => json!([
                {"area_id": "kitchen", "name": "Kitchen", "floor_id": null, "labels": []}
            ]),
            "config/device_registry/list" => json!([
                {
                    "id": "device-42",
                    "name": "Kitchen lamp",
                    "name_by_user": null,
                    "area_id": "kitchen",
                    "manufacturer": "Example",
                    "model": "Lamp"
                }
            ]),
            "config/entity_registry/list" => json!([
                {
                    "id": "registry-17",
                    "entity_id": "light.kitchen",
                    "name": null,
                    "original_name": "Kitchen lamp",
                    "platform": "hue",
                    "device_id": "device-42",
                    "area_id": null,
                    "labels": []
                }
            ]),
            "get_states" => {
                let response = json!({
                    "id": id,
                    "type": "result",
                    "success": true,
                    "result": [{
                        "entity_id": "light.kitchen",
                        "state": "on",
                        "attributes": {
                            "brightness": 128,
                            "access_token": "must-be-filtered",
                            "media_title": "Private listening history"
                        },
                        "last_updated": "2026-07-17T10:00:00Z",
                        "context": {"id": "ctx-initial"}
                    }]
                });
                let event = json!({
                    "id": subscription_id,
                    "type": "event",
                    "event": {
                        "event_type": "state_changed",
                        "data": {
                            "entity_id": "light.kitchen",
                            "new_state": {
                                "entity_id": "light.kitchen",
                                "state": "off",
                                "attributes": {
                                    "brightness": 0,
                                    "access_token": "must-be-filtered",
                                    "media_title": "Private listening history"
                                },
                                "last_updated": "2026-07-17T10:00:01Z",
                                "context": {"id": "ctx-event"}
                            }
                        }
                    }
                });
                send_json(&mut socket, json!([response, event])).await;
                break;
            },
            other => panic!("unexpected command {other}"),
        };
        send_json(
            &mut socket,
            json!({"id": id, "type": "result", "success": true, "result": result}),
        )
        .await;
    }
}

#[tokio::test]
async fn websocket_transport_authenticates_fetches_snapshot_and_decodes_coalesced_events() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
    let address = listener.local_addr().expect("listener address");
    let server = tokio::spawn(run_mock_home_assistant(listener));

    let config = HomeAssistantConnectionConfig::new(
        format!("http://{address}"),
        SecretRef::new("test:home-assistant").expect("secret reference"),
    )
    .expect("connection config")
    .with_request_timeout(Duration::from_secs(2))
    .expect("timeout");
    let rendered = format!("{config:?}");
    assert!(!rendered.contains(ACCESS_TOKEN));
    assert_eq!(
        config.websocket_url(),
        &format!("ws://{address}/api/websocket")
    );

    let transport = WebSocketHomeAssistantTransport::connect(config, Arc::new(StaticResolver))
        .await
        .expect("authenticated transport");
    let snapshot = transport.fetch_snapshot().await.expect("snapshot");
    assert_eq!(snapshot.areas[0].id, "kitchen");
    assert_eq!(snapshot.devices[0].id, "device-42");
    assert_eq!(snapshot.entities[0].id, "registry-17");
    assert_eq!(snapshot.states[0].state, "on");
    assert!(snapshot.states[0].attributes.contains_key("brightness"));
    assert!(!snapshot.states[0].attributes.contains_key("access_token"));
    assert!(!snapshot.states[0].attributes.contains_key("media_title"));

    let event = transport.next_state_changed().await.expect("state event");
    assert_eq!(event.new_state.state, "off");
    assert_eq!(event.new_state.context_id.as_deref(), Some("ctx-event"));
    assert!(!event.new_state.attributes.contains_key("access_token"));
    assert!(!event.new_state.attributes.contains_key("media_title"));

    server.await.expect("mock server");
}

#[test]
fn connection_config_rejects_credentials_query_fragments_and_non_origin_paths() {
    let secret = || SecretRef::new("test:home-assistant").expect("secret reference");
    for endpoint in [
        "http://user:password@localhost:8123",
        "http://localhost:8123?token=secret",
        "http://localhost:8123/#fragment",
        "http://localhost:8123/some/path",
        "ftp://localhost:8123",
    ] {
        assert!(
            HomeAssistantConnectionConfig::new(endpoint, secret()).is_err(),
            "{endpoint} must be rejected"
        );
    }
}

async fn accept_subscribed(
    listener: TcpListener,
) -> (WebSocketStream<TcpStream>, serde_json::Value) {
    let (stream, _) = listener.accept().await.expect("client connection");
    let mut socket = accept_async(stream).await.expect("websocket handshake");

    send_json(
        &mut socket,
        json!({"type": "auth_required", "ha_version": "2026.7.2"}),
    )
    .await;
    let auth = read_json(&mut socket).await;
    assert_eq!(auth["type"], "auth");
    assert_eq!(auth["access_token"], ACCESS_TOKEN);
    send_json(
        &mut socket,
        json!({"type": "auth_ok", "ha_version": "2026.7.2"}),
    )
    .await;

    let features = read_json(&mut socket).await;
    assert_eq!(features["type"], "supported_features");
    send_json(
        &mut socket,
        json!({"id": features["id"], "type": "result", "success": true, "result": null}),
    )
    .await;

    let subscription = read_json(&mut socket).await;
    assert_eq!(subscription["type"], "subscribe_events");
    assert!(
        subscription.get("event_type").is_none(),
        "registry changes require an all-events subscription"
    );
    let subscription_id = subscription["id"].clone();
    send_json(
        &mut socket,
        json!({"id": subscription_id, "type": "result", "success": true, "result": null}),
    )
    .await;

    (socket, subscription_id)
}

fn connection_config(
    address: std::net::SocketAddr,
    timeout: Duration,
) -> HomeAssistantConnectionConfig {
    HomeAssistantConnectionConfig::new(
        format!("http://{address}"),
        SecretRef::new("test:home-assistant").expect("secret reference"),
    )
    .expect("connection config")
    .with_request_timeout(timeout)
    .expect("request timeout")
}

async fn connect_transport(
    address: std::net::SocketAddr,
    timeout: Duration,
) -> WebSocketHomeAssistantTransport {
    WebSocketHomeAssistantTransport::connect(
        connection_config(address, timeout),
        Arc::new(StaticResolver),
    )
    .await
    .expect("authenticated transport")
}

fn state_event_at(
    subscription_id: &Value,
    state: &str,
    observed_at: &str,
    context_id: &str,
) -> Value {
    json!({
        "id": subscription_id,
        "type": "event",
        "event": {
            "event_type": "state_changed",
            "data": {
                "entity_id": "light.kitchen",
                "new_state": {
                    "entity_id": "light.kitchen",
                    "state": state,
                    "attributes": {"brightness": 42},
                    "last_updated": observed_at,
                    "context": {"id": context_id}
                }
            }
        }
    })
}

fn state_event(subscription_id: &Value, state: &str) -> Value {
    state_event_at(subscription_id, state, "2026-07-17T10:00:01Z", "ctx-single")
}

#[tokio::test]
async fn auth_invalid_is_permanent_for_the_session_and_never_echoes_provider_text() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
    let address = listener.local_addr().expect("listener address");
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.expect("client connection");
        let mut socket = accept_async(stream).await.expect("websocket handshake");
        send_json(&mut socket, json!({"type": "auth_required"})).await;
        let auth = read_json(&mut socket).await;
        assert_eq!(auth["access_token"], ACCESS_TOKEN);
        send_json(
            &mut socket,
            json!({
                "type": "auth_invalid",
                "message": "provider-secret-marker-that-must-not-leak"
            }),
        )
        .await;
    });

    let error = match WebSocketHomeAssistantTransport::connect(
        connection_config(address, Duration::from_secs(2)),
        Arc::new(StaticResolver),
    )
    .await
    {
        Ok(_) => panic!("invalid credentials must fail"),
        Err(error) => error,
    };

    assert_eq!(error.kind(), PortErrorKind::Rejected);
    assert!(!error.to_string().contains(ACCESS_TOKEN));
    assert!(
        !error
            .to_string()
            .contains("provider-secret-marker-that-must-not-leak")
    );
    server.await.expect("mock server");
}

struct LeakingResolver;

#[async_trait]
impl SecretResolver for LeakingResolver {
    async fn resolve(&self, _reference: &SecretRef) -> PortResult<SecretMaterial> {
        Err(PortError::new(
            PortErrorKind::Permanent,
            format!("resolver accidentally rendered {ACCESS_TOKEN}"),
        ))
    }
}

#[tokio::test]
async fn resolver_failures_are_sanitized_at_the_transport_boundary() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
    let address = listener.local_addr().expect("listener address");
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.expect("client connection");
        let mut socket = accept_async(stream).await.expect("websocket handshake");
        send_json(&mut socket, json!({"type": "auth_required"})).await;
        let _closed = socket.next().await;
    });

    let error = match WebSocketHomeAssistantTransport::connect(
        connection_config(address, Duration::from_secs(2)),
        Arc::new(LeakingResolver),
    )
    .await
    {
        Ok(_) => panic!("resolver failure must fail authentication"),
        Err(error) => error,
    };

    assert_eq!(error.kind(), PortErrorKind::Permanent);
    assert!(!error.to_string().contains(ACCESS_TOKEN));
    server.await.expect("mock server");
}

#[tokio::test]
async fn single_object_events_are_decoded_without_coalescing() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
    let address = listener.local_addr().expect("listener address");
    let server = tokio::spawn(async move {
        let (mut socket, subscription_id) = accept_subscribed(listener).await;
        send_json(&mut socket, state_event(&subscription_id, "on")).await;
    });

    let transport = connect_transport(address, Duration::from_secs(2)).await;
    let event = transport.next_state_changed().await.expect("single event");

    assert_eq!(event.new_state.state, "on");
    server.await.expect("mock server");
}

#[tokio::test]
async fn registry_change_requires_snapshot_before_incremental_events_resume() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
    let address = listener.local_addr().expect("listener address");
    let server = tokio::spawn(async move {
        let (mut socket, subscription_id) = accept_subscribed(listener).await;
        send_json(
            &mut socket,
            json!({
                "id": subscription_id,
                "type": "event",
                "event": {
                    "event_type": "entity_registry_updated",
                    "data": {"action": "update", "entity_id": "light.kitchen"}
                }
            }),
        )
        .await;
        send_json(&mut socket, state_event(&subscription_id, "off")).await;

        for expected_type in [
            "config/area_registry/list",
            "config/device_registry/list",
            "config/entity_registry/list",
            "get_states",
        ] {
            let command = read_json(&mut socket).await;
            assert_eq!(command["type"], expected_type);
            send_json(
                &mut socket,
                json!({
                    "id": command["id"],
                    "type": "result",
                    "success": true,
                    "result": []
                }),
            )
            .await;
        }
    });

    let transport = connect_transport(address, Duration::from_secs(2)).await;
    let changed = transport
        .next_state_changed()
        .await
        .expect_err("registry change must invalidate incrementals");
    assert_eq!(changed.kind(), PortErrorKind::Conflict);

    let still_stale = transport
        .next_state_changed()
        .await
        .expect_err("snapshot must be mandatory after registry change");
    assert_eq!(still_stale.kind(), PortErrorKind::Conflict);

    let snapshot = transport.fetch_snapshot().await.expect("complete resync");
    assert!(snapshot.areas.is_empty());
    let event = transport
        .next_state_changed()
        .await
        .expect("incrementals resume after snapshot");
    assert_eq!(event.new_state.state, "off");
    server.await.expect("mock server");
}

#[tokio::test]
async fn snapshot_fences_older_state_events_buffered_on_both_sides_of_its_result() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
    let address = listener.local_addr().expect("listener address");
    let server = tokio::spawn(async move {
        let (mut socket, subscription_id) = accept_subscribed(listener).await;

        for expected_type in [
            "config/area_registry/list",
            "config/device_registry/list",
            "config/entity_registry/list",
        ] {
            let command = read_json(&mut socket).await;
            assert_eq!(command["type"], expected_type);
            send_json(
                &mut socket,
                json!({
                    "id": command["id"],
                    "type": "result",
                    "success": true,
                    "result": []
                }),
            )
            .await;
        }

        let command = read_json(&mut socket).await;
        assert_eq!(command["type"], "get_states");
        let snapshot_result = json!({
            "id": command["id"],
            "type": "result",
            "success": true,
            "result": [{
                "entity_id": "light.kitchen",
                "state": "snapshot-current",
                "attributes": {"brightness": 42},
                "last_updated": "2026-07-17T10:00:02Z",
                "context": {"id": "ctx-snapshot"}
            }]
        });
        send_json(
            &mut socket,
            json!([
                state_event_at(
                    &subscription_id,
                    "older-before-result",
                    "2026-07-17T10:00:00Z",
                    "ctx-older-before"
                ),
                snapshot_result,
                state_event_at(
                    &subscription_id,
                    "older-after-result",
                    "2026-07-17T10:00:01Z",
                    "ctx-older-after"
                )
            ]),
        )
        .await;
        send_json(
            &mut socket,
            state_event_at(
                &subscription_id,
                "newer-than-snapshot",
                "2026-07-17T10:00:03Z",
                "ctx-newer",
            ),
        )
        .await;
    });

    let transport = connect_transport(address, Duration::from_secs(2)).await;
    let snapshot = transport.fetch_snapshot().await.expect("snapshot");
    assert_eq!(snapshot.states[0].state, "snapshot-current");

    let event = transport
        .next_state_changed()
        .await
        .expect("only an event newer than the snapshot may be replayed");
    assert_eq!(event.new_state.state, "newer-than-snapshot");
    assert_eq!(event.new_state.context_id.as_deref(), Some("ctx-newer"));
    server.await.expect("mock server");
}

#[tokio::test]
async fn snapshot_fence_rejects_an_older_state_event_delivered_after_the_result_frame() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
    let address = listener.local_addr().expect("listener address");
    let server = tokio::spawn(async move {
        let (mut socket, subscription_id) = accept_subscribed(listener).await;

        for expected_type in [
            "config/area_registry/list",
            "config/device_registry/list",
            "config/entity_registry/list",
        ] {
            let command = read_json(&mut socket).await;
            assert_eq!(command["type"], expected_type);
            send_json(
                &mut socket,
                json!({
                    "id": command["id"],
                    "type": "result",
                    "success": true,
                    "result": []
                }),
            )
            .await;
        }

        let command = read_json(&mut socket).await;
        assert_eq!(command["type"], "get_states");
        send_json(
            &mut socket,
            json!({
                "id": command["id"],
                "type": "result",
                "success": true,
                "result": [{
                    "entity_id": "light.kitchen",
                    "state": "snapshot-current",
                    "attributes": {"brightness": 42},
                    "last_updated": "2026-07-17T10:00:02Z",
                    "context": {"id": "ctx-snapshot"}
                }]
            }),
        )
        .await;

        tokio::time::sleep(Duration::from_millis(20)).await;
        send_json(
            &mut socket,
            state_event_at(
                &subscription_id,
                "delayed-but-older",
                "2026-07-17T10:00:01Z",
                "ctx-delayed-old",
            ),
        )
        .await;
        send_json(
            &mut socket,
            state_event_at(
                &subscription_id,
                "newer-than-snapshot",
                "2026-07-17T10:00:03Z",
                "ctx-newer",
            ),
        )
        .await;
    });

    let transport = connect_transport(address, Duration::from_secs(2)).await;
    let snapshot = transport.fetch_snapshot().await.expect("snapshot");
    assert_eq!(snapshot.states[0].state, "snapshot-current");

    let event = transport
        .next_state_changed()
        .await
        .expect("a delayed old event cannot cross the snapshot fence");
    assert_eq!(event.new_state.state, "newer-than-snapshot");
    server.await.expect("mock server");
}

#[tokio::test]
async fn stream_disconnect_requires_resynchronization() {
    let disconnect_listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
    let disconnect_address = disconnect_listener.local_addr().expect("listener address");
    let disconnect_server = tokio::spawn(async move {
        let (mut socket, _) = accept_subscribed(disconnect_listener).await;
        socket.close(None).await.expect("close socket");
    });
    let disconnected = connect_transport(disconnect_address, Duration::from_secs(2)).await;
    let error = disconnected
        .next_state_changed()
        .await
        .expect_err("disconnect must fail");
    assert_eq!(error.kind(), PortErrorKind::Conflict);
    disconnect_server.await.expect("disconnect server");
}

#[tokio::test]
async fn idle_period_longer_than_request_timeout_keeps_the_subscription() {
    let idle_listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
    let idle_address = idle_listener.local_addr().expect("listener address");
    let idle_server = tokio::spawn(async move {
        let (mut socket, subscription_id) = accept_subscribed(idle_listener).await;
        tokio::time::sleep(Duration::from_millis(150)).await;
        send_json(&mut socket, state_event(&subscription_id, "after-idle")).await;
    });
    let idle = connect_transport(idle_address, Duration::from_millis(100)).await;
    let idle_timeout = idle
        .next_state_changed()
        .await
        .expect_err("a bounded state wait reports normal idle time");
    assert_eq!(idle_timeout.kind(), PortErrorKind::Timeout);
    let event = idle
        .next_state_changed()
        .await
        .expect("an idle timeout must leave the same subscription usable");
    assert_eq!(event.new_state.state, "after-idle");
    idle_server.await.expect("idle server");
}

#[tokio::test]
async fn concurrent_state_waiters_consume_distinct_events_in_fifo_order() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
    let address = listener.local_addr().expect("listener address");
    let server = tokio::spawn(async move {
        let (mut socket, subscription_id) = accept_subscribed(listener).await;
        tokio::time::sleep(Duration::from_millis(50)).await;
        send_json(
            &mut socket,
            state_event_at(
                &subscription_id,
                "first",
                "2026-07-17T10:00:01Z",
                "ctx-first-waiter",
            ),
        )
        .await;
        send_json(
            &mut socket,
            state_event_at(
                &subscription_id,
                "second",
                "2026-07-17T10:00:02Z",
                "ctx-second-waiter",
            ),
        )
        .await;
    });

    let transport = connect_transport(address, Duration::from_secs(2)).await;
    let first_transport = transport.clone();
    let first = tokio::spawn(async move { first_transport.next_state_changed().await });
    tokio::time::sleep(Duration::from_millis(10)).await;
    let second_transport = transport.clone();
    let second = tokio::spawn(async move { second_transport.next_state_changed().await });

    let first_event = first
        .await
        .expect("first waiter task")
        .expect("first state event");
    let second_event = second
        .await
        .expect("second waiter task")
        .expect("second state event");
    assert_eq!(
        first_event.new_state.context_id.as_deref(),
        Some("ctx-first-waiter")
    );
    assert_eq!(
        second_event.new_state.context_id.as_deref(),
        Some("ctx-second-waiter")
    );
    server.await.expect("mock server");
}

#[tokio::test]
async fn oversized_coalesced_batches_and_invalid_json_are_rejected_without_echo() {
    let oversized_listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
    let oversized_address = oversized_listener.local_addr().expect("listener address");
    let oversized_server = tokio::spawn(async move {
        let (mut socket, _) = accept_subscribed(oversized_listener).await;
        send_json(&mut socket, Value::Array(vec![json!({}); 50_001])).await;
    });
    let oversized = connect_transport(oversized_address, Duration::from_secs(2)).await;
    let error = oversized
        .next_state_changed()
        .await
        .expect_err("oversized coalesced batch must fail");
    assert_eq!(error.kind(), PortErrorKind::InvalidData);
    oversized_server.await.expect("oversized server");

    let invalid_listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
    let invalid_address = invalid_listener.local_addr().expect("listener address");
    let invalid_server = tokio::spawn(async move {
        let (mut socket, _) = accept_subscribed(invalid_listener).await;
        socket
            .send(Message::Text(
                "invalid-json-secret-marker-that-must-not-leak".into(),
            ))
            .await
            .expect("invalid server frame");
    });
    let invalid = connect_transport(invalid_address, Duration::from_secs(2)).await;
    let error = invalid
        .next_state_changed()
        .await
        .expect_err("invalid JSON must fail");
    assert_eq!(error.kind(), PortErrorKind::InvalidData);
    assert!(
        !error
            .to_string()
            .contains("invalid-json-secret-marker-that-must-not-leak")
    );
    invalid_server.await.expect("invalid server");
}

#[tokio::test]
async fn frames_over_the_transport_byte_limit_are_invalid_external_data() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
    let address = listener.local_addr().expect("listener address");
    let server = tokio::spawn(async move {
        let (mut socket, subscription_id) = accept_subscribed(listener).await;
        let oversized = json!({
            "id": subscription_id,
            "type": "event",
            "padding": "x".repeat(4 * 1024 * 1024 + 1)
        })
        .to_string();
        let _client_may_close_after_header = socket.send(Message::Text(oversized.into())).await;
    });

    let transport = connect_transport(address, Duration::from_secs(2)).await;
    let error = transport
        .next_state_changed()
        .await
        .expect_err("oversized frame must be rejected");
    assert_eq!(error.kind(), PortErrorKind::InvalidData);
    server.await.expect("mock server");
}

#[tokio::test]
async fn event_for_an_unknown_subscription_is_rejected() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
    let address = listener.local_addr().expect("listener address");
    let server = tokio::spawn(async move {
        let (mut socket, subscription_id) = accept_subscribed(listener).await;
        let unexpected_id = subscription_id.as_u64().expect("subscription id") + 1;
        send_json(&mut socket, state_event(&Value::from(unexpected_id), "on")).await;
    });

    let transport = connect_transport(address, Duration::from_secs(2)).await;
    let error = transport
        .next_state_changed()
        .await
        .expect_err("unknown subscription must be rejected");
    assert_eq!(error.kind(), PortErrorKind::InvalidData);
    server.await.expect("mock server");
}

#[tokio::test]
async fn non_event_messages_are_rejected_while_waiting_for_state() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
    let address = listener.local_addr().expect("listener address");
    let server = tokio::spawn(async move {
        let (mut socket, _) = accept_subscribed(listener).await;
        send_json(
            &mut socket,
            json!({"id": 9_999, "type": "result", "success": true, "result": null}),
        )
        .await;
    });

    let transport = connect_transport(address, Duration::from_secs(2)).await;
    let error = transport
        .next_state_changed()
        .await
        .expect_err("an unsolicited result must be rejected");
    assert_eq!(error.kind(), PortErrorKind::InvalidData);
    server.await.expect("mock server");
}

#[tokio::test]
async fn unrelated_event_traffic_cannot_extend_the_state_wait_deadline() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
    let address = listener.local_addr().expect("listener address");
    let server = tokio::spawn(async move {
        let (mut socket, subscription_id) = accept_subscribed(listener).await;
        for sequence in 0..30 {
            send_json(
                &mut socket,
                json!({
                    "id": subscription_id,
                    "type": "event",
                    "event": {
                        "event_type": "logbook_entry",
                        "data": {"sequence": sequence}
                    }
                }),
            )
            .await;
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    });

    let transport = connect_transport(address, Duration::from_millis(100)).await;
    let error = transport
        .next_state_changed()
        .await
        .expect_err("unrelated events must not defeat the caller deadline");
    assert_eq!(error.kind(), PortErrorKind::Timeout);
    server.await.expect("mock server");
}

#[tokio::test]
async fn shared_shutdown_interrupts_waiters_with_a_static_typed_error() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
    let address = listener.local_addr().expect("listener address");
    let server = tokio::spawn(async move {
        let (mut socket, _) = accept_subscribed(listener).await;
        match tokio::time::timeout(Duration::from_secs(1), socket.next()).await {
            Ok(None | Some(Err(_)) | Some(Ok(Message::Close(_)))) => {},
            Ok(Some(Ok(message))) => panic!("unexpected client frame during shutdown: {message:?}"),
            Err(_) => panic!("transport actor remained connected after shutdown"),
        }
    });

    let transport = connect_transport(address, Duration::from_secs(10)).await;
    let waiting_transport = transport.clone();
    let waiter = tokio::spawn(async move { waiting_transport.next_state_changed().await });
    tokio::task::yield_now().await;

    tokio::time::timeout(Duration::from_secs(1), transport.shutdown())
        .await
        .expect("shutdown must be bounded")
        .expect("shutdown must complete");

    let error = waiter
        .await
        .expect("waiter task")
        .expect_err("shared shutdown must interrupt the waiter");
    assert_eq!(error.kind(), PortErrorKind::Unavailable);
    assert_eq!(
        error.message(),
        "Home Assistant WebSocket transport is shut down"
    );
    assert!(!error.to_string().contains(ACCESS_TOKEN));

    let after_shutdown = transport
        .next_state_changed()
        .await
        .expect_err("all shared handles must remain shut down");
    assert_eq!(after_shutdown, error);
    server.await.expect("mock server");
}

#[tokio::test]
async fn dropping_a_non_final_clone_keeps_the_shared_actor_running() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
    let address = listener.local_addr().expect("listener address");
    let server = tokio::spawn(async move {
        let (mut socket, subscription_id) = accept_subscribed(listener).await;
        send_json(&mut socket, state_event(&subscription_id, "on")).await;
    });

    let transport = connect_transport(address, Duration::from_secs(2)).await;
    let surviving_clone = transport.clone();
    drop(transport);

    let event = surviving_clone
        .next_state_changed()
        .await
        .expect("a non-final clone must not stop the actor");
    assert_eq!(event.new_state.state, "on");

    surviving_clone.shutdown().await.expect("bounded shutdown");
    server.await.expect("mock server");
}

#[tokio::test]
async fn dropping_the_last_handle_aborts_an_orphaned_actor_request() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
    let address = listener.local_addr().expect("listener address");
    let server = tokio::spawn(async move {
        let (mut socket, _) = accept_subscribed(listener).await;
        match tokio::time::timeout(Duration::from_secs(1), socket.next()).await {
            Ok(None | Some(Err(_)) | Some(Ok(Message::Close(_)))) => {},
            Ok(Some(Ok(message))) => panic!("unexpected client frame after last drop: {message:?}"),
            Err(_) => panic!("last handle drop left the transport actor running"),
        }
    });

    let transport = connect_transport(address, Duration::from_secs(10)).await;
    let pending =
        tokio::time::timeout(Duration::from_millis(100), transport.next_state_changed()).await;
    assert!(
        pending.is_err(),
        "the caller must cancel while the actor is still reading"
    );

    drop(transport);

    server.await.expect("mock server");
}
