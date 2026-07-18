use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use aether_cloudlink::{GatewaySessionAuthenticator, UplinkAuthentication};
use aether_domain::{
    EntityId, EntityPointDescriptor, EntityRecord, ExternalAlias, IntegrationPointKey,
    IntegrationPointKind, IntegrationSnapshot, IntegrationTopologySnapshot, ObservedValueType,
    SnapshotDigest, TimestampMs, TopologyGeneration,
};
use aether_home_assistant_bridge::{
    HomeAssistantConnectionConfig, WebSocketHomeAssistantTransport,
};
use aether_integration_control::IntegrationControlCodec;
use aether_ports::{
    IntegrationProjectionSink, PortResult, SecretMaterial, SecretRef, SecretResolver,
};
use ed25519_dalek::{Signer as _, SigningKey, Verifier as _};
use futures::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::{WebSocketStream, accept_async, tungstenite::Message};

use super::*;
use crate::home_assistant::{HomeAssistantRuntimeConfig, InMemoryIntegrationProjection};

const GATEWAY_ID: &str = "33333333-3333-4333-8333-333333333333";
const SESSION_ID: &str = "44444444-4444-4444-8444-444444444444";
const RESUMED_SESSION_ID: &str = "55555555-5555-4555-8555-555555555555";
const INTEGRATION_ID: &str = "home-assistant.home";
const ENTITY_ID: &str = "entity-registry-light-bedroom";
const CLOUD_KEY_ENV: &str = "AETHER_TEST_CONTROL_CLOUD_PUBLIC_KEY";
const EDGE_KEY_ENV: &str = "AETHER_TEST_CONTROL_EDGE_SIGNING_KEY";
const HOME_ASSISTANT_TOKEN: &str = "edge-local-control-test-token";
const OFFER: &[u8] = include_bytes!(
    "../../../crates/aether-integration-control/tests/fixtures/integration-control/v1alpha1/action-offer.valid.json"
);

struct CountingExecutor {
    inner: Arc<ActiveHomeAssistantExecutor>,
    calls: Arc<AtomicUsize>,
}

#[async_trait]
impl IntegrationActionExecutor for CountingExecutor {
    async fn execute(
        &self,
        _action: &aether_integration_control::IntegrationPowerAction,
    ) -> ProviderExecutionResult {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.inner.execute(_action).await
    }
}

struct StaticSecretResolver;

#[async_trait]
impl SecretResolver for StaticSecretResolver {
    async fn resolve(&self, _reference: &SecretRef) -> PortResult<SecretMaterial> {
        SecretMaterial::new(HOME_ASSISTANT_TOKEN)
    }
}

#[derive(Default)]
struct RecordingPublisher {
    payloads: std::sync::Mutex<Vec<Vec<u8>>>,
}

impl RecordingPublisher {
    fn payloads(&self) -> Vec<Vec<u8>> {
        self.payloads.lock().expect("publisher lock").clone()
    }
}

#[async_trait]
impl IntegrationControlReceiptPublisher for RecordingPublisher {
    async fn publish_receipt(&self, payload: Vec<u8>) -> PortResult<()> {
        self.payloads.lock().expect("publisher lock").push(payload);
        Ok(())
    }
}

fn current_timestamp_ms() -> PortResult<u64> {
    let duration = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("test clock after Unix epoch");
    Ok(u64::try_from(duration.as_millis()).expect("test timestamp range"))
}

#[tokio::test]
async fn persistent_runtime_executes_once_resends_until_exact_ack_and_replays_without_execution() {
    let root = tempfile::tempdir().expect("temporary control root");
    let cloud_signing_key = SigningKey::from_bytes(&[7_u8; 32]);
    let edge_signing_key = SigningKey::from_bytes(&[9_u8; 32]);
    set_key_environment(&cloud_signing_key, &edge_signing_key);
    write_policy(root.path());
    let runtime_config = runtime_config(root.path());
    let enabled = runtime_config.enabled().expect("Home Assistant enabled");
    let control_config = enabled
        .integration_control()
        .expect("control explicitly enabled")
        .clone();
    let gateway = enabled.gateway_id().clone();
    let integration = enabled.integration_id().clone();
    let projection = projection().await;
    let session = SessionBinding::new(GATEWAY_ID, SESSION_ID, 7, 3).expect("session");
    let resumed_session =
        SessionBinding::new(GATEWAY_ID, RESUMED_SESSION_ID, 8, 3).expect("resumed session");
    let uplink_authentication = gateway_authentication(&edge_signing_key);
    let offer = signed_offer(&cloud_signing_key);
    let provider_calls = Arc::new(AtomicUsize::new(0));
    let (home_assistant_transport, home_assistant_server) = connected_home_assistant().await;

    let mut prepared =
        PreparedIntegrationControl::prepare(&control_config, &gateway, &integration, "aether-test")
            .expect("prepared control");
    prepared
        .active_executor
        .install(home_assistant_transport)
        .await;
    let executor = Arc::new(CountingExecutor {
        inner: prepared.active_executor.clone(),
        calls: provider_calls.clone(),
    });
    prepared.executor = executor.clone();
    let processor = prepared
        .processor(&session, projection.clone())
        .expect("session processor");
    prepared
        .recover_once(&processor)
        .await
        .expect("startup recovery");
    let publisher = RecordingPublisher::default();

    prepared
        .process_offer(
            &processor,
            &session,
            &uplink_authentication,
            &publisher,
            &offer,
        )
        .await
        .expect("first governed offer");
    home_assistant_server
        .await
        .expect("mock Home Assistant server");
    assert_eq!(provider_calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        prepared
            .ledger
            .pending_receipts(16)
            .await
            .expect("pending")
            .len(),
        1,
        "transport publication must not remove a business receipt"
    );
    let first = receipt_json(&publisher.payloads()[0]);
    assert_eq!(first["payload"]["stage"], "provider-accepted");
    assert!(
        first["payload"]["evidence_digest"].is_string(),
        "provider acceptance must carry a bounded evidence digest"
    );
    assert!(
        first["payload"].get("failure_code").is_none(),
        "provider acceptance must not carry a failure code"
    );
    assert_edge_signature(&first, &edge_signing_key);

    prepared
        .process_offer(
            &processor,
            &session,
            &uplink_authentication,
            &publisher,
            &offer,
        )
        .await
        .expect("same job replay");
    assert_eq!(
        provider_calls.load(Ordering::SeqCst),
        1,
        "same job and digest must not cross the provider boundary twice"
    );
    let replay = receipt_json(&publisher.payloads()[1]);
    assert_same_delivery(&first, &replay);
    assert_edge_signature(&replay, &edge_signing_key);
    // Do not hide the frozen profile's unresolved cross-session replay
    // identity/signing-digest conflict behind a process-local byte cache.

    drop(processor);
    drop(prepared);

    let mut restarted =
        PreparedIntegrationControl::prepare(&control_config, &gateway, &integration, "aether-test")
            .expect("restarted control");
    restarted.executor = Arc::new(CountingExecutor {
        inner: restarted.active_executor.clone(),
        calls: provider_calls.clone(),
    });
    let restarted_publisher = RecordingPublisher::default();
    restarted
        .flush_receipts(
            &resumed_session,
            &uplink_authentication,
            &restarted_publisher,
        )
        .await
        .expect("restart resend");
    let resent = receipt_json(&restarted_publisher.payloads()[0]);
    assert_same_delivery(&first, &resent);
    assert_eq!(
        first["sent_at_ms"], resent["sent_at_ms"],
        "the receipt spool must preserve the original immutable send time"
    );
    assert_ne!(
        first["message_authentication"]["signature"], resent["message_authentication"]["signature"],
        "a newer session re-signs the same immutable receipt fact"
    );
    assert_edge_signature(&resent, &edge_signing_key);

    let wrong_session_ack = durable_ack(&first, "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa", 6);
    assert!(
        restarted
            .acknowledge(&resumed_session, &wrong_session_ack)
            .await
            .is_err(),
        "old or foreign sessions cannot acknowledge a receipt"
    );
    assert_eq!(
        restarted
            .ledger
            .pending_receipts(16)
            .await
            .expect("still pending")
            .len(),
        1
    );

    restarted
        .acknowledge(
            &resumed_session,
            &durable_ack(&resent, RESUMED_SESSION_ID, 8),
        )
        .await
        .expect("current authenticated ACK");
    assert!(
        restarted
            .ledger
            .pending_receipts(16)
            .await
            .expect("acknowledged")
            .is_empty()
    );

    let restarted_processor = restarted
        .processor(&resumed_session, projection)
        .expect("restarted processor");
    let resumed_offer = resign_offer_for_session(&offer, &cloud_signing_key, RESUMED_SESSION_ID, 8);
    restarted
        .process_offer(
            &restarted_processor,
            &resumed_session,
            &uplink_authentication,
            &restarted_publisher,
            &resumed_offer,
        )
        .await
        .expect("post-ACK same job replay");
    assert_eq!(provider_calls.load(Ordering::SeqCst), 1);
    let after_ack_replay = receipt_json(&restarted_publisher.payloads()[1]);
    assert_eq!(
        after_ack_replay["delivery"]["position"], "2",
        "an explicitly replayed acknowledged receipt receives a fresh durable position"
    );

    clear_key_environment();
}

#[test]
fn trusted_connector_receipts_are_external_attestation_or_explicit_legacy_harness_signatures() {
    let fixture = IntegrationControlCodec::decode_receipt_envelope(include_bytes!(
        "../../../crates/aether-integration-control/tests/fixtures/integration-control/v1alpha1/action-receipt-provider-accepted.valid.json"
    ))
    .expect("receipt fixture");
    let receipt =
        SpooledActionReceipt::new(1, fixture.payload().clone()).expect("spooled receipt fact");
    let session = SessionBinding::new(GATEWAY_ID, SESSION_ID, 7, 3).expect("session");
    let trusted = UplinkAuthentication::trusted_connector_broker_attestation();

    let externally_attested = receipt_json(
        &encode_receipt(&session, &trusted, None, &receipt).expect("unsigned receipt"),
    );
    assert!(
        externally_attested.get("message_authentication").is_none(),
        "external broker attestation must not create a placeholder payload signature"
    );

    let key = SigningKey::from_bytes(&[9_u8; 32]);
    let legacy = LegacyReceiptSigner {
        key_id: "legacy-control-harness-key".to_owned(),
        key: key.clone(),
    };
    let signed = receipt_json(
        &encode_receipt(&session, &trusted, Some(&legacy), &receipt)
            .expect("explicit legacy harness receipt"),
    );
    assert_eq!(
        signed["message_authentication"]["key_id"],
        "legacy-control-harness-key"
    );
    assert_edge_signature(&signed, &key);
}

async fn projection() -> Arc<InMemoryIntegrationProjection> {
    let point = EntityPointDescriptor::new(
        IntegrationPointKey::new("is_on").expect("point key"),
        "Power",
        IntegrationPointKind::State,
        ObservedValueType::Boolean,
        None,
    )
    .expect("point");
    let entity = EntityRecord::new(
        EntityId::new(ENTITY_ID).expect("entity"),
        "Bedroom light",
        "light",
        vec![point],
        None,
        None,
        vec![
            ExternalAlias::new("home-assistant", "entity-id", "light.bedroom")
                .expect("provider alias"),
        ],
    )
    .expect("entity record");
    let topology = IntegrationTopologySnapshot::new(
        GatewayIdentity::new(GATEWAY_ID).expect("gateway"),
        IntegrationId::new(INTEGRATION_ID).expect("integration"),
        TopologyGeneration::new(1).expect("generation"),
        TimestampMs::new(current_timestamp_ms().expect("clock")),
        SnapshotDigest::new(format!("sha256:{}", "a".repeat(64))).expect("digest"),
        vec![],
        vec![],
        vec![entity],
    )
    .expect("topology");
    let projection = Arc::new(InMemoryIntegrationProjection::default());
    projection
        .replace_snapshot(IntegrationSnapshot::new(topology, vec![]).expect("snapshot"))
        .await
        .expect("project snapshot");
    projection
}

async fn connected_home_assistant() -> (WebSocketHomeAssistantTransport, tokio::task::JoinHandle<()>)
{
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("Home Assistant listener");
    let address = listener.local_addr().expect("listener address");
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.expect("Home Assistant client");
        let mut socket = accept_async(stream).await.expect("WebSocket");
        authenticate_and_subscribe(&mut socket).await;

        let command = read_json(&mut socket).await;
        assert_eq!(
            command,
            json!({
                "id": command["id"],
                "type": "call_service",
                "domain": "light",
                "service": "turn_on",
                "target": {"entity_id": "light.bedroom"}
            }),
            "the runtime must not forward arbitrary domain, service, or service_data"
        );
        send_json(
            &mut socket,
            json!({
                "id": command["id"],
                "type": "result",
                "success": true,
                "result": {
                    "context": {
                        "id": "home-assistant-context-1",
                        "parent_id": null,
                        "user_id": "edge-control-test"
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
    .expect("Home Assistant connection")
    .with_request_timeout(Duration::from_secs(2))
    .expect("request timeout");
    let transport =
        WebSocketHomeAssistantTransport::connect(config, Arc::new(StaticSecretResolver))
            .await
            .expect("Home Assistant transport");
    (transport, server)
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

async fn authenticate_and_subscribe(socket: &mut WebSocketStream<TcpStream>) {
    send_json(socket, json!({"type": "auth_required"})).await;
    assert_eq!(
        read_json(socket).await,
        json!({"type": "auth", "access_token": HOME_ASSISTANT_TOKEN})
    );
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
}

fn signed_offer(key: &SigningKey) -> Vec<u8> {
    let now = current_timestamp_ms().expect("clock");
    let mut value: Value = serde_json::from_slice(OFFER).expect("offer fixture");
    value["issued_at_ms"] = Value::String(now.to_string());
    value["expires_at_ms"] = Value::String(now.saturating_add(60_000).to_string());
    value["intent"]["authorization"]["authorized_at_ms"] =
        Value::String(now.saturating_sub(100).to_string());
    value["intent"]["confirmation"]["confirmed_at_ms"] =
        Value::String(now.saturating_sub(50).to_string());
    value["intent_digest"] = Value::String(
        IntegrationControlCodec::intent_digest_json(&value["intent"])
            .expect("updated intent digest"),
    );
    value["cloud_authentication"]["key_id"] = Value::String("cloud-control-key-1".to_string());
    value["cloud_authentication"]["signature"] = Value::String("A".repeat(86));
    let unsigned =
        IntegrationControlCodec::decode_offer(&serde_json::to_vec(&value).expect("unsigned offer"))
            .expect("structurally valid offer");
    value["cloud_authentication"]["signature"] = Value::String(
        URL_SAFE_NO_PAD.encode(
            key.sign(&unsigned.signing_bytes().expect("bytes"))
                .to_bytes(),
        ),
    );
    serde_json::to_vec(&value).expect("signed offer")
}

fn resign_offer_for_session(
    offer: &[u8],
    key: &SigningKey,
    session_id: &str,
    session_epoch: u64,
) -> Vec<u8> {
    let mut value: Value = serde_json::from_slice(offer).expect("signed offer JSON");
    value["session_id"] = Value::String(session_id.to_owned());
    value["session_epoch"] = Value::String(session_epoch.to_string());
    value["cloud_authentication"]["signature"] = Value::String("A".repeat(86));
    let unsigned =
        IntegrationControlCodec::decode_offer(&serde_json::to_vec(&value).expect("unsigned offer"))
            .expect("structurally valid resumed offer");
    value["cloud_authentication"]["signature"] = Value::String(
        URL_SAFE_NO_PAD.encode(
            key.sign(&unsigned.signing_bytes().expect("bytes"))
                .to_bytes(),
        ),
    );
    serde_json::to_vec(&value).expect("resumed signed offer")
}

fn runtime_config(root: &Path) -> HomeAssistantRuntimeConfig {
    let values = BTreeMap::from([
        ("AETHER_HOME_ASSISTANT_ENABLED", "true".to_string()),
        (
            "AETHER_HOME_ASSISTANT_ORIGIN",
            "http://homeassistant.invalid:8123".to_string(),
        ),
        (
            "AETHER_HOME_ASSISTANT_ACCESS_TOKEN_REF",
            "env:AETHER_TEST_HOME_ASSISTANT_TOKEN".to_string(),
        ),
        ("AETHER_GATEWAY_ID", GATEWAY_ID.to_string()),
        (
            "AETHER_HOME_ASSISTANT_INTEGRATION_ID",
            INTEGRATION_ID.to_string(),
        ),
        (
            "AETHER_HOME_ASSISTANT_GENERATION_STORE_PATH",
            root.join("generation.json").to_string_lossy().into_owned(),
        ),
        (
            "AETHER_HOME_ASSISTANT_CLOUDLINK_ENABLED",
            "true".to_string(),
        ),
        (
            "AETHER_HOME_ASSISTANT_CLOUDLINK_ORIGIN_MODEL",
            "gateway-signed".to_string(),
        ),
        (
            "AETHER_HOME_ASSISTANT_CLOUDLINK_CLOUD_KEY_ID",
            "development-cloud-key-1".to_string(),
        ),
        (
            "AETHER_HOME_ASSISTANT_CLOUDLINK_CLOUD_PUBLIC_KEY_REF",
            format!("env:{CLOUD_KEY_ENV}"),
        ),
        (
            "AETHER_HOME_ASSISTANT_CLOUDLINK_GATEWAY_KEY_ID",
            "edge-control-key-1".to_string(),
        ),
        (
            "AETHER_HOME_ASSISTANT_CLOUDLINK_GATEWAY_SIGNING_KEY_REF",
            format!("env:{EDGE_KEY_ENV}"),
        ),
        (
            "AETHER_HOME_ASSISTANT_CLOUDLINK_CHALLENGE_LEDGER_PATH",
            root.join("challenge-ledger.json")
                .to_string_lossy()
                .into_owned(),
        ),
        (
            "AETHER_HOME_ASSISTANT_CLOUDLINK_RUNTIME_CONFIG_DIR",
            root.to_string_lossy().into_owned(),
        ),
        (
            "AETHER_HOME_ASSISTANT_CLOUDLINK_CLOUD_EXTENSION",
            "aether.cloudlink.integration.v1alpha1".to_string(),
        ),
        (
            "AETHER_HOME_ASSISTANT_CLOUDLINK_TOPOLOGY_SPOOL_PATH",
            root.join("topology.spool").to_string_lossy().into_owned(),
        ),
        (
            "AETHER_HOME_ASSISTANT_CLOUDLINK_OBSERVATION_SPOOL_PATH",
            root.join("observations.spool")
                .to_string_lossy()
                .into_owned(),
        ),
        (
            "AETHER_HOME_ASSISTANT_CLOUDLINK_MQTT_BROKER_HOST",
            "localhost".to_string(),
        ),
        (
            "AETHER_HOME_ASSISTANT_CLOUDLINK_MQTT_BROKER_PORT",
            "8883".to_string(),
        ),
        (
            "AETHER_HOME_ASSISTANT_CLOUDLINK_MQTT_CLIENT_ID",
            "aether-edge-control-test".to_string(),
        ),
        (
            "AETHER_HOME_ASSISTANT_CLOUDLINK_MQTT_TOPIC_PREFIX",
            "aether-test".to_string(),
        ),
        (
            "AETHER_HOME_ASSISTANT_CLOUDLINK_MQTT_USERNAME",
            "edge-control-test".to_string(),
        ),
        (
            "AETHER_HOME_ASSISTANT_CLOUDLINK_MQTT_PASSWORD_REF",
            "env:AETHER_TEST_CONTROL_MQTT_PASSWORD".to_string(),
        ),
        (
            "AETHER_HOME_ASSISTANT_CLOUDLINK_CREDENTIAL_ID",
            "edge-control-credential".to_string(),
        ),
        (
            "AETHER_HOME_ASSISTANT_CLOUDLINK_CREDENTIAL_GENERATION",
            "3".to_string(),
        ),
        (
            "AETHER_HOME_ASSISTANT_CLOUDLINK_SESSION_EPOCH_PATH",
            root.join("session-epoch").to_string_lossy().into_owned(),
        ),
        ("AETHER_HOME_ASSISTANT_CONTROL_ENABLED", "true".to_string()),
        (
            "AETHER_HOME_ASSISTANT_CONTROL_CLOUD_EXTENSION",
            "aether.cloudlink.integration-control.v1alpha1".to_string(),
        ),
        (
            "AETHER_HOME_ASSISTANT_CONTROL_LEDGER_PATH",
            root.join("control-ledger.json")
                .to_string_lossy()
                .into_owned(),
        ),
        (
            "AETHER_HOME_ASSISTANT_CONTROL_POLICY_PATH",
            root.join("control-policy.json")
                .to_string_lossy()
                .into_owned(),
        ),
        (
            "AETHER_HOME_ASSISTANT_CONTROL_AUDIT_PATH",
            root.join("control-audit.jsonl")
                .to_string_lossy()
                .into_owned(),
        ),
        (
            "AETHER_HOME_ASSISTANT_CONTROL_CLOUD_KEY_ID",
            "cloud-control-key-1".to_string(),
        ),
        (
            "AETHER_HOME_ASSISTANT_CONTROL_CLOUD_PUBLIC_KEY_REF",
            format!("env:{CLOUD_KEY_ENV}"),
        ),
        (
            "AETHER_HOME_ASSISTANT_CONTROL_EDGE_KEY_ID",
            "edge-control-key-1".to_string(),
        ),
        (
            "AETHER_HOME_ASSISTANT_CONTROL_EDGE_SIGNING_KEY_REF",
            format!("env:{EDGE_KEY_ENV}"),
        ),
    ]);
    HomeAssistantRuntimeConfig::from_lookup(|name| values.get(name).cloned())
        .expect("complete control configuration")
}

fn write_policy(root: &Path) {
    std::fs::write(
        root.join("control-policy.json"),
        serde_json::to_vec(&json!({
            "schema": POLICY_SCHEMA,
            "gateway_id": GATEWAY_ID,
            "integration_id": INTEGRATION_ID,
            "permission": PERMISSION,
            "commissioned_entities": [ENTITY_ID],
            "delegated_entities": [ENTITY_ID],
            "allowed_subjects": ["user-homeowner"]
        }))
        .expect("policy JSON"),
    )
    .expect("write policy");
}

fn set_key_environment(cloud: &SigningKey, edge: &SigningKey) {
    // SAFETY: these process variables are uniquely scoped to this test module.
    unsafe {
        std::env::set_var(
            CLOUD_KEY_ENV,
            URL_SAFE_NO_PAD.encode(cloud.verifying_key().to_bytes()),
        );
        std::env::set_var(EDGE_KEY_ENV, URL_SAFE_NO_PAD.encode(edge.to_bytes()));
    }
}

fn clear_key_environment() {
    // SAFETY: paired cleanup for the uniquely scoped variables above.
    unsafe {
        std::env::remove_var(CLOUD_KEY_ENV);
        std::env::remove_var(EDGE_KEY_ENV);
    }
}

fn gateway_authentication(key: &SigningKey) -> UplinkAuthentication {
    GatewaySessionAuthenticator::new(
        "development-cloud-key-1",
        SigningKey::from_bytes(&[7_u8; 32])
            .verifying_key()
            .to_bytes(),
        "edge-control-key-1",
        key.to_bytes(),
    )
    .expect("session authenticator")
    .uplink_authentication()
}

fn receipt_json(bytes: &[u8]) -> Value {
    IntegrationControlCodec::decode_receipt_envelope(bytes).expect("strict receipt envelope");
    serde_json::from_slice(bytes).expect("receipt JSON")
}

fn assert_same_delivery(left: &Value, right: &Value) {
    assert_eq!(
        left["delivery"]["stream_id"],
        right["delivery"]["stream_id"]
    );
    assert_eq!(
        left["delivery"]["stream_epoch"],
        right["delivery"]["stream_epoch"]
    );
    assert_eq!(left["delivery"]["position"], right["delivery"]["position"]);
    assert_eq!(left["delivery"]["batch_id"], right["delivery"]["batch_id"]);
    assert_eq!(left["delivery"]["digest"], right["delivery"]["digest"]);
}

fn assert_edge_signature(receipt: &Value, key: &SigningKey) {
    let encoded = serde_json::to_vec(receipt).expect("receipt bytes");
    let envelope =
        IntegrationControlCodec::decode_receipt_envelope(&encoded).expect("receipt envelope");
    let session = ControlSession::new(
        GATEWAY_ID,
        receipt["session_id"].as_str().expect("session ID"),
        receipt["session_epoch"]
            .as_str()
            .expect("epoch")
            .parse()
            .expect("epoch number"),
        receipt["credential_generation"]
            .as_str()
            .expect("generation")
            .parse()
            .expect("generation number"),
    )
    .expect("control session");
    let sent_at = receipt["sent_at_ms"]
        .as_str()
        .expect("sent time")
        .parse()
        .expect("sent time number");
    let signing_bytes =
        IntegrationControlCodec::receipt_signing_bytes(&session, sent_at, envelope.delivery())
            .expect("receipt signing bytes");
    let signature = decode_base64url::<64>(
        receipt["message_authentication"]["signature"]
            .as_str()
            .expect("signature"),
    )
    .expect("signature encoding");
    key.verifying_key()
        .verify(&signing_bytes, &Signature::from_bytes(&signature))
        .expect("edge receipt signature");
}

fn durable_ack(receipt: &Value, session_id: &str, session_epoch: u64) -> Vec<u8> {
    serde_json::to_vec(&json!({
        "schema": "aether.cloudlink.durable-ack.v1",
        "protocol": "aether.cloudlink",
        "protocol_version": "1.0",
        "message_kind": "durable-ack",
        "gateway_id": GATEWAY_ID,
        "session_id": session_id,
        "session_epoch": session_epoch.to_string(),
        "credential_generation": "3",
        "stream_id": receipt["delivery"]["stream_id"],
        "stream_epoch": receipt["delivery"]["stream_epoch"],
        "acknowledged_position": receipt["delivery"]["position"],
        "batch_id": receipt["delivery"]["batch_id"],
        "digest": receipt["delivery"]["digest"],
        "receipt_id": "cloud-control-ack-1",
        "acknowledged_at_ms": current_timestamp_ms().expect("clock").to_string()
    }))
    .expect("durable ACK")
}
