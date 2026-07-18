#![cfg(feature = "integration-control")]

use std::sync::Arc;
use std::time::Duration;

use aether_domain::{GatewayIdentity, IntegrationId, IntegrationSnapshot};
use aether_home_assistant_bridge::{
    HomeAssistantBridge, HomeAssistantConnectionConfig, HomeAssistantTransport,
    WebSocketHomeAssistantTransport,
};
use aether_integration_control::{
    ActionReceiptStage, ActionTarget, AuditEvent, AuditRecord, CloudOfferVerifier, ControlClock,
    ControlDependencyError, ControlIdGenerator, ControlSession, ControllableEntityKind,
    IntegrationActionExecutor, IntegrationControlAudit, IntegrationControlConfig,
    IntegrationControlProcessor, IntegrationPowerAction, LocalAuthorityDecision,
    LocalControlAuthority, MemoryIntegrationControlLedger, ProcessDisposition,
    ProjectionTargetResolver, ProviderExecutionResult, ResolvedControlTarget,
};
use aether_ports::{
    DelegatedDeviceProvider, IntegrationProjectionQuery, PortResult, SecretMaterial, SecretRef,
    SecretResolver,
};
use async_trait::async_trait;
use futures::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::{WebSocketStream, accept_async, tungstenite::Message};

const ACCESS_TOKEN: &str = "edge-local-test-token";
const OFFER: &[u8] = include_bytes!(
    "../../../crates/aether-integration-control/tests/fixtures/integration-control/v1alpha1/action-offer.valid.json"
);

struct StaticResolver;

struct StaticProjection(IntegrationSnapshot);

struct AcceptingCloudVerifier;

struct AllowLocalAuthority;

struct RecordingAudit;

struct FixedClock;

struct FixedId;

#[async_trait]
impl SecretResolver for StaticResolver {
    async fn resolve(&self, _reference: &SecretRef) -> PortResult<SecretMaterial> {
        SecretMaterial::new(ACCESS_TOKEN)
    }
}

#[async_trait]
impl IntegrationProjectionQuery for StaticProjection {
    async fn snapshot(
        &self,
        _gateway_id: &GatewayIdentity,
        _integration_id: &IntegrationId,
    ) -> PortResult<Option<IntegrationSnapshot>> {
        Ok(Some(self.0.clone()))
    }
}

#[async_trait]
impl CloudOfferVerifier for AcceptingCloudVerifier {
    async fn verify(
        &self,
        _key_id: &str,
        _signature: &str,
        signing_bytes: &[u8],
    ) -> Result<bool, ControlDependencyError> {
        let value: Value =
            serde_json::from_slice(signing_bytes).expect("canonical signed projection");
        assert!(value.get("cloud_authentication").is_none());
        Ok(true)
    }
}

#[async_trait]
impl LocalControlAuthority for AllowLocalAuthority {
    async fn evaluate(
        &self,
        _request: &aether_integration_control::LocalAuthorityRequest<'_>,
    ) -> Result<LocalAuthorityDecision, ControlDependencyError> {
        Ok(LocalAuthorityDecision {
            commissioned: true,
            delegated: true,
            permission_granted: true,
            confirmation_valid: true,
        })
    }
}

#[async_trait]
impl IntegrationControlAudit for RecordingAudit {
    async fn record(&self, _event: &AuditEvent<'_>) -> Result<AuditRecord, ControlDependencyError> {
        AuditRecord::complete("audit-control-composed")
    }
}

impl ControlClock for FixedClock {
    fn now_ms(&self) -> u64 {
        1_784_217_600_100
    }
}

impl ControlIdGenerator for FixedId {
    fn next_receipt_id(&self) -> String {
        "77777777-7777-4777-8777-777777777777".to_string()
    }
}

async fn read_json(socket: &mut WebSocketStream<TcpStream>) -> Value {
    let message = socket
        .next()
        .await
        .expect("client frame")
        .expect("valid client frame");
    serde_json::from_str(message.to_text().expect("text frame")).expect("client JSON")
}

async fn send_json(socket: &mut WebSocketStream<TcpStream>, value: Value) {
    socket
        .send(Message::Text(value.to_string().into()))
        .await
        .expect("server response");
}

async fn authenticate_and_subscribe(socket: &mut WebSocketStream<TcpStream>) -> Value {
    send_json(socket, json!({"type": "auth_required"})).await;
    let auth = read_json(socket).await;
    assert_eq!(auth, json!({"type": "auth", "access_token": ACCESS_TOKEN}));
    send_json(socket, json!({"type": "auth_ok"})).await;

    let features = read_json(socket).await;
    send_json(
        socket,
        json!({"id": features["id"], "type": "result", "success": true, "result": null}),
    )
    .await;
    let subscription = read_json(socket).await;
    send_json(
        socket,
        json!({"id": subscription["id"], "type": "result", "success": true, "result": null}),
    )
    .await;
    subscription["id"].clone()
}

#[tokio::test]
async fn fixed_power_action_returns_only_provider_acceptance_context() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
    let address = listener.local_addr().expect("address");
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.expect("client");
        let mut socket = accept_async(stream).await.expect("WebSocket");
        let _subscription_id = authenticate_and_subscribe(&mut socket).await;

        let command = read_json(&mut socket).await;
        assert_eq!(command["type"], "call_service");
        assert_eq!(command["domain"], "light");
        assert_eq!(command["service"], "turn_on");
        assert_eq!(command["target"], json!({"entity_id": "light.bedroom"}));
        assert!(command.get("service_data").is_none());
        assert!(command.get("url").is_none());
        assert!(command.get("token").is_none());
        send_json(
            &mut socket,
            json!({
                "id": command["id"],
                "type": "result",
                "success": true,
                "result": {
                    "context": {
                        "id": "ctx-ha-call-1",
                        "parent_id": null,
                        "user_id": "edge-user"
                    },
                    "response": null
                }
            }),
        )
        .await;
    });

    let config = HomeAssistantConnectionConfig::new(
        format!("http://{address}"),
        SecretRef::new("test:home-assistant").expect("secret reference"),
    )
    .expect("config")
    .with_request_timeout(Duration::from_secs(2))
    .expect("timeout");
    let transport = WebSocketHomeAssistantTransport::connect(config, Arc::new(StaticResolver))
        .await
        .expect("transport");
    let target = ActionTarget::new("home-assistant.home", 1, "entity-registry-light-bedroom")
        .expect("target");
    let resolved = ResolvedControlTarget::home_assistant(
        target,
        ControllableEntityKind::Light,
        "light.bedroom",
    )
    .expect("resolved target");
    let action = IntegrationPowerAction::for_resolved_target(
        "55555555-5555-4555-8555-555555555555",
        resolved,
        true,
    )
    .expect("closed action");

    let result = transport.execute(&action).await;
    let ProviderExecutionResult::Accepted(acceptance) = result else {
        panic!("provider must accept the fixed operation");
    };
    assert_eq!(acceptance.context_id(), "ctx-ha-call-1");
    assert!(!acceptance.physical_completed());
    server.await.expect("mock server");
}

#[tokio::test]
async fn power_control_preempts_an_idle_state_wait_without_losing_the_waiter() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
    let address = listener.local_addr().expect("address");
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.expect("client");
        let mut socket = accept_async(stream).await.expect("WebSocket");
        let subscription_id = authenticate_and_subscribe(&mut socket).await;

        let command = tokio::time::timeout(Duration::from_millis(500), read_json(&mut socket))
            .await
            .expect("power control must not wait for the idle state deadline");
        assert_eq!(command["type"], "call_service");
        assert_eq!(command["domain"], "light");
        assert_eq!(command["service"], "turn_on");
        assert_eq!(command["target"], json!({"entity_id": "light.bedroom"}));
        send_json(
            &mut socket,
            json!({
                "id": command["id"],
                "type": "result",
                "success": true,
                "result": {"context": {"id": "ctx-ha-priority-control"}}
            }),
        )
        .await;
        send_json(
            &mut socket,
            json!({
                "id": subscription_id,
                "type": "event",
                "event": {
                    "event_type": "state_changed",
                    "data": {
                        "entity_id": "light.bedroom",
                        "new_state": {
                            "entity_id": "light.bedroom",
                            "state": "on",
                            "attributes": {},
                            "last_updated": "2026-07-17T10:00:01Z",
                            "context": {"id": "ctx-ha-state-after-control"}
                        }
                    }
                }
            }),
        )
        .await;
    });

    let config = HomeAssistantConnectionConfig::new(
        format!("http://{address}"),
        SecretRef::new("test:home-assistant").expect("secret reference"),
    )
    .expect("config")
    .with_request_timeout(Duration::from_secs(2))
    .expect("timeout");
    let transport = WebSocketHomeAssistantTransport::connect(config, Arc::new(StaticResolver))
        .await
        .expect("transport");
    let waiting_transport = transport.clone();
    let state_waiter = tokio::spawn(async move { waiting_transport.next_state_changed().await });
    tokio::time::sleep(Duration::from_millis(20)).await;

    let target = ActionTarget::new("home-assistant.home", 1, "entity-registry-light-bedroom")
        .expect("target");
    let resolved = ResolvedControlTarget::home_assistant(
        target,
        ControllableEntityKind::Light,
        "light.bedroom",
    )
    .expect("resolved target");
    let action = IntegrationPowerAction::for_resolved_target(
        "55555555-5555-4555-8555-555555555555",
        resolved,
        true,
    )
    .expect("closed action");

    let result = tokio::time::timeout(Duration::from_millis(800), transport.execute(&action))
        .await
        .expect("power control was blocked by an idle state wait");
    let ProviderExecutionResult::Accepted(acceptance) = result else {
        panic!("provider must accept the preempting fixed operation");
    };
    assert_eq!(acceptance.context_id(), "ctx-ha-priority-control");

    let event = tokio::time::timeout(Duration::from_secs(1), state_waiter)
        .await
        .expect("state waiter must resume after control")
        .expect("state waiter task")
        .expect("state event");
    assert_eq!(
        event.new_state.context_id.as_deref(),
        Some("ctx-ha-state-after-control")
    );
    server.await.expect("mock server");
}

#[tokio::test]
async fn power_control_preemption_does_not_restart_the_state_wait_deadline() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
    let address = listener.local_addr().expect("address");
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.expect("client");
        let mut socket = accept_async(stream).await.expect("WebSocket");
        let _subscription_id = authenticate_and_subscribe(&mut socket).await;

        let command = read_json(&mut socket).await;
        assert_eq!(command["type"], "call_service");
        send_json(
            &mut socket,
            json!({
                "id": command["id"],
                "type": "result",
                "success": true,
                "result": {"context": {"id": "ctx-ha-deadline-control"}}
            }),
        )
        .await;
        tokio::time::sleep(Duration::from_millis(400)).await;
    });

    let config = HomeAssistantConnectionConfig::new(
        format!("http://{address}"),
        SecretRef::new("test:home-assistant").expect("secret reference"),
    )
    .expect("config")
    .with_request_timeout(Duration::from_millis(200))
    .expect("timeout");
    let transport = WebSocketHomeAssistantTransport::connect(config, Arc::new(StaticResolver))
        .await
        .expect("transport");
    let waiting_transport = transport.clone();
    let state_waiter = tokio::spawn(async move { waiting_transport.next_state_changed().await });
    tokio::time::sleep(Duration::from_millis(140)).await;

    let target = ActionTarget::new("home-assistant.home", 1, "entity-registry-light-bedroom")
        .expect("target");
    let resolved = ResolvedControlTarget::home_assistant(
        target,
        ControllableEntityKind::Light,
        "light.bedroom",
    )
    .expect("resolved target");
    let action = IntegrationPowerAction::for_resolved_target(
        "55555555-5555-4555-8555-555555555555",
        resolved,
        true,
    )
    .expect("closed action");
    let result = transport.execute(&action).await;
    assert!(matches!(result, ProviderExecutionResult::Accepted(_)));

    let error = tokio::time::timeout(Duration::from_millis(140), state_waiter)
        .await
        .expect("control preemption must preserve the original state deadline")
        .expect("state waiter task")
        .expect_err("idle state wait must time out");
    assert_eq!(error.kind(), aether_ports::PortErrorKind::Timeout);
    server.await.expect("mock server");
}

#[tokio::test]
async fn mapped_is_on_offer_crosses_the_processor_and_only_calls_the_fixed_power_service() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
    let address = listener.local_addr().expect("address");
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.expect("client");
        let mut socket = accept_async(stream).await.expect("WebSocket");
        let _subscription_id = authenticate_and_subscribe(&mut socket).await;

        for expected_type in [
            "config/area_registry/list",
            "config/device_registry/list",
            "config/entity_registry/list",
            "get_states",
        ] {
            let command = read_json(&mut socket).await;
            assert_eq!(command["type"], expected_type);
            let result = match expected_type {
                "config/area_registry/list" | "config/device_registry/list" => json!([]),
                "config/entity_registry/list" => json!([{
                    "id": "entity-registry-light-bedroom",
                    "entity_id": "light.bedroom",
                    "name": null,
                    "original_name": "Bedroom light",
                    "platform": "test",
                    "device_id": null,
                    "area_id": null,
                    "labels": []
                }]),
                "get_states" => json!([{
                    "entity_id": "light.bedroom",
                    "state": "on",
                    "attributes": {},
                    "last_updated": "2026-07-17T10:00:00Z",
                    "context": {"id": "ctx-ha-state-1"}
                }]),
                _ => unreachable!("expected command list is closed"),
            };
            send_json(
                &mut socket,
                json!({
                    "id": command["id"],
                    "type": "result",
                    "success": true,
                    "result": result
                }),
            )
            .await;
        }

        let command = read_json(&mut socket).await;
        assert_eq!(
            command,
            json!({
                "id": command["id"],
                "type": "call_service",
                "domain": "light",
                "service": "turn_on",
                "target": {"entity_id": "light.bedroom"}
            })
        );
        send_json(
            &mut socket,
            json!({
                "id": command["id"],
                "type": "result",
                "success": true,
                "result": {
                    "context": {
                        "id": "ctx-ha-call-2",
                        "parent_id": null,
                        "user_id": "edge-user"
                    },
                    "response": null
                }
            }),
        )
        .await;
    });

    let config = HomeAssistantConnectionConfig::new(
        format!("http://{address}"),
        SecretRef::new("test:home-assistant").expect("secret reference"),
    )
    .expect("config")
    .with_request_timeout(Duration::from_secs(2))
    .expect("timeout");
    let transport = WebSocketHomeAssistantTransport::connect(config, Arc::new(StaticResolver))
        .await
        .expect("transport");
    let gateway_id = GatewayIdentity::new("33333333-3333-4333-8333-333333333333").expect("gateway");
    let integration_id = IntegrationId::new("home-assistant.home").expect("integration");
    let bridge = HomeAssistantBridge::new(
        gateway_id.clone(),
        integration_id.clone(),
        transport.clone(),
    );
    let snapshot = bridge.snapshot().await.expect("mapped snapshot");
    let entity = &snapshot.topology().entities()[0];
    assert!(
        entity
            .points()
            .iter()
            .any(|point| point.key().as_str() == "is_on")
    );
    assert!(
        entity
            .points()
            .iter()
            .all(|point| point.key().as_str() != "state")
    );

    assert_eq!(snapshot.topology().generation().get(), 1);
    let ledger = Arc::new(MemoryIntegrationControlLedger::new());
    let executor: Arc<dyn IntegrationActionExecutor> = Arc::new(transport.clone());
    let processor = IntegrationControlProcessor::new(
        IntegrationControlConfig::enabled(Duration::from_secs(2)).expect("enabled control"),
        ControlSession::new(
            gateway_id.as_str(),
            "44444444-4444-4444-8444-444444444444",
            7,
            3,
        )
        .expect("control session"),
        Arc::new(AcceptingCloudVerifier),
        Arc::new(ProjectionTargetResolver::new(Arc::new(StaticProjection(
            snapshot,
        )))),
        Arc::new(AllowLocalAuthority),
        Arc::new(RecordingAudit),
        Arc::clone(&ledger) as Arc<dyn aether_integration_control::IntegrationControlLedger>,
        executor,
        Arc::new(FixedClock),
        Arc::new(FixedId),
    );

    let processed = processor
        .process(OFFER)
        .await
        .expect("governed control offer");
    assert_eq!(processed.disposition(), ProcessDisposition::Executed);
    assert_eq!(
        processed.receipt().stage(),
        ActionReceiptStage::ProviderAccepted
    );
    assert!(!processed.receipt().physical_completed());
    assert!(!processed.receipt().job_succeeded());
    assert_eq!(ledger.pending_receipt_count().await, 1);

    let replayed = processor
        .process(OFFER)
        .await
        .expect("same job replays receipt");
    assert_eq!(replayed.disposition(), ProcessDisposition::Replayed);
    assert_eq!(
        replayed.receipt().receipt_id(),
        processed.receipt().receipt_id()
    );
    server.await.expect("mock server");
}
