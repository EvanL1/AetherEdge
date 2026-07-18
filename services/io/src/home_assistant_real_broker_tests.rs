use std::collections::BTreeMap;
use std::fs::{OpenOptions, read, read_to_string};
use std::io::Write as _;
use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use aether_home_assistant_bridge::HomeAssistantConnectionConfig;
use aether_integration_control::IntegrationControlLedger;
use aether_ports::{CloudLinkSpool, IntegrationTopologyGenerationStore};
use aether_store_local::{
    FileCloudLinkSpool, FileIntegrationControlLedger, FileIntegrationTopologyGenerationStore,
};
use futures::{SinkExt as _, StreamExt as _};
use serde_json::{Value, json};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::oneshot;
use tokio_tungstenite::{WebSocketStream, accept_async, tungstenite::Message};

use super::*;

const RUN_SETTING: &str = "AETHER_HA_E2E_RUN";
const BROKER_PORT_SETTING: &str = "AETHER_HA_E2E_BROKER_PORT";
const TOPIC_PREFIX_SETTING: &str = "AETHER_HA_E2E_TOPIC_PREFIX";
const GATEWAY_ID_SETTING: &str = "AETHER_HA_E2E_GATEWAY_ID";
const EVIDENCE_LOG_SETTING: &str = "AETHER_HA_E2E_EVIDENCE_LOG";
const CLOUD_SESSION_PUBLIC_KEY_SETTING: &str = "AETHER_HA_E2E_CLOUD_SESSION_PUBLIC_X";
const GATEWAY_SESSION_PRIVATE_KEY_SETTING: &str = "AETHER_HA_E2E_GATEWAY_SESSION_PRIVATE_D";
const CONTROL_CLOUD_PUBLIC_KEY_SETTING: &str = "AETHER_HA_E2E_CLOUD_PUBLIC_X";
const HOME_ASSISTANT_TOKEN_SETTING: &str = "AETHER_HA_E2E_HOME_ASSISTANT_TOKEN";
const MQTT_PASSWORD_SETTING: &str = "AETHER_HA_E2E_MQTT_PASSWORD";
const INTEGRATION_ID: &str = "home-assistant.home";
const ENTITY_ID: &str = "entity-registry-light-bedroom";
const ACCESS_TOKEN: &str = "home-assistant-loopback-test-token";
const CONTROL_OFFER_PUBLICATION_DEADLINE: Duration = Duration::from_secs(60);
const SERVICE_CALL_DEADLINE: Duration = Duration::from_secs(30);
const CLOUD_COMPLETION_DEADLINE: Duration = Duration::from_secs(30);

#[test]
fn real_broker_runtime_config_uses_one_gateway_session_signer_for_every_uplink() {
    let root = tempfile::tempdir().expect("temporary Edge configuration root");
    let config = runtime_config(
        root.path(),
        "http://127.0.0.1:8123",
        "1883",
        "aether-ha-e2e/config-test",
        "33333333-3333-4333-8333-333333333333",
    );
    let enabled = config.enabled().expect("Home Assistant enabled");
    let cloudlink = enabled.cloudlink().expect("CloudLink enabled");

    assert_eq!(
        cloudlink.origin_model(),
        HomeAssistantCloudLinkOriginModel::GatewaySigned
    );
    assert_eq!(
        cloudlink.challenge_ledger_path(),
        Some(root.path().join("challenge-ledger.json").as_path())
    );
    assert!(
        enabled
            .integration_control()
            .expect("Integration control enabled")
            .legacy_receipt_signing()
            .is_none(),
        "the real harness must reuse the CloudLink Gateway session signer"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "opt-in real Broker test; run only through the AetherCloud Home Assistant harness"]
async fn real_broker_projects_home_assistant_and_completes_governed_control() {
    assert_eq!(
        std::env::var(RUN_SETTING).as_deref(),
        Ok("1"),
        "run this ignored test only through the opt-in AetherCloud harness"
    );
    let broker_port = required_environment(BROKER_PORT_SETTING);
    let topic_prefix = required_environment(TOPIC_PREFIX_SETTING);
    let gateway_id = required_environment(GATEWAY_ID_SETTING);
    let evidence_log = required_environment(EVIDENCE_LOG_SETTING);
    required_environment(CLOUD_SESSION_PUBLIC_KEY_SETTING);
    required_environment(GATEWAY_SESSION_PRIVATE_KEY_SETTING);
    required_environment(CONTROL_CLOUD_PUBLIC_KEY_SETTING);

    // SAFETY: the opt-in harness runs only this test in its own process and the
    // process exits after the test, so these test-only secrets cannot race with
    // another runtime or escape into a long-lived process.
    unsafe {
        std::env::set_var(HOME_ASSISTANT_TOKEN_SETTING, ACCESS_TOKEN);
        std::env::set_var(MQTT_PASSWORD_SETTING, "temporary-broker-password");
    }

    let root = tempfile::tempdir().expect("temporary Edge evidence root");
    aether_runtime_catalog::KernelRuntimeManifest::from_io_features(
        env!("CARGO_PKG_VERSION"),
        "aarch64-unknown-linux-musl",
        ["home-assistant-integration-control"],
    )
    .expect("control runtime manifest")
    .write_to_config_directory(root.path())
    .expect("write control runtime manifest");
    write_policy(root.path(), &gateway_id);

    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("loopback Home Assistant listener");
    let home_assistant_origin = format!(
        "http://{}",
        listener.local_addr().expect("Home Assistant address")
    );
    let (service_called_tx, service_called_rx) = oneshot::channel();
    let (server_stop_tx, server_stop_rx) = oneshot::channel();
    let home_assistant_server = tokio::spawn(run_mock_home_assistant(
        listener,
        service_called_tx,
        server_stop_rx,
    ));

    let runtime_config = runtime_config(
        root.path(),
        &home_assistant_origin,
        &broker_port,
        &topic_prefix,
        &gateway_id,
    );
    let enabled = runtime_config
        .enabled()
        .expect("Home Assistant explicitly enabled")
        .clone();
    let connection =
        HomeAssistantConnectionConfig::new(enabled.origin(), enabled.access_token_ref().clone())
            .expect("loopback Home Assistant connection");
    let generation_store: Arc<dyn IntegrationTopologyGenerationStore> = Arc::new(
        FileIntegrationTopologyGenerationStore::open(enabled.generation_store_path())
            .expect("topology generation store"),
    );
    let prepared = prepare_cloudlink_runtime_with_security(
        &enabled,
        enabled.cloudlink().expect("CloudLink enabled"),
        CloudLinkTlsConfig::Disabled,
        DeploymentSecurity::Development,
    )
    .expect("development-only real Broker composition");
    let shutdown = CancellationToken::new();
    let task_shutdown = shutdown.clone();
    let runtime = tokio::spawn(run_enabled_integration(
        enabled,
        connection,
        generation_store,
        Some(prepared),
        task_shutdown,
    ));

    wait_for_cloud_event(
        &evidence_log,
        "control-offer-published",
        CONTROL_OFFER_PUBLICATION_DEADLINE,
    )
    .await;
    tokio::time::timeout(SERVICE_CALL_DEADLINE, service_called_rx)
        .await
        .expect("fixed Home Assistant service call deadline")
        .expect("Home Assistant service call evidence");
    wait_for_cloud_event(&evidence_log, "control-complete", CLOUD_COMPLETION_DEADLINE).await;
    wait_for_control_ack(&root.path().join("control-ledger.json")).await;

    shutdown.cancel();
    tokio::time::timeout(Duration::from_secs(5), runtime)
        .await
        .expect("Edge runtime shutdown deadline")
        .expect("Edge runtime task");
    let _ = server_stop_tx.send(());
    tokio::time::timeout(Duration::from_secs(2), home_assistant_server)
        .await
        .expect("Home Assistant server shutdown deadline")
        .expect("Home Assistant server task");

    assert_spool_drained(
        &root.path().join("topology.spool"),
        &format!("integration-topology-{INTEGRATION_ID}"),
    )
    .await;
    assert_spool_drained(
        &root.path().join("observations.spool"),
        &format!("integration-observations-{INTEGRATION_ID}"),
    )
    .await;
    let ledger = FileIntegrationControlLedger::open(root.path().join("control-ledger.json"))
        .expect("reopen control ledger after runtime shutdown");
    assert!(
        ledger
            .pending_receipts(16)
            .await
            .expect("pending control receipts")
            .is_empty(),
        "the Edge receipt ledger is removed only after Cloud's durable ACK"
    );
    append_evidence(
        &evidence_log,
        json!({
            "source": "aether-edge",
            "event": "edge-complete",
            "capability_id": "device.power.set.v1",
            "provider_call_observed": true,
            "topology_spool_drained": true,
            "observation_spool_drained": true,
            "control_receipt_ledger_drained": true
        }),
    );
}

fn runtime_config(
    root: &Path,
    origin: &str,
    broker_port: &str,
    topic_prefix: &str,
    gateway_id: &str,
) -> HomeAssistantRuntimeConfig {
    let values = BTreeMap::from([
        ("AETHER_HOME_ASSISTANT_ENABLED", "true".to_string()),
        ("AETHER_HOME_ASSISTANT_ORIGIN", origin.to_string()),
        (
            "AETHER_HOME_ASSISTANT_ACCESS_TOKEN_REF",
            format!("env:{HOME_ASSISTANT_TOKEN_SETTING}"),
        ),
        ("AETHER_GATEWAY_ID", gateway_id.to_string()),
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
            "cloud-session-key-1".to_string(),
        ),
        (
            "AETHER_HOME_ASSISTANT_CLOUDLINK_CLOUD_PUBLIC_KEY_REF",
            format!("env:{CLOUD_SESSION_PUBLIC_KEY_SETTING}"),
        ),
        (
            "AETHER_HOME_ASSISTANT_CLOUDLINK_GATEWAY_KEY_ID",
            "gateway-session-key-17".to_string(),
        ),
        (
            "AETHER_HOME_ASSISTANT_CLOUDLINK_GATEWAY_SIGNING_KEY_REF",
            format!("env:{GATEWAY_SESSION_PRIVATE_KEY_SETTING}"),
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
            "127.0.0.1".to_string(),
        ),
        (
            "AETHER_HOME_ASSISTANT_CLOUDLINK_MQTT_BROKER_PORT",
            broker_port.to_string(),
        ),
        (
            "AETHER_HOME_ASSISTANT_CLOUDLINK_MQTT_CLIENT_ID",
            format!("aether-edge-ha-e2e-{}", std::process::id()),
        ),
        (
            "AETHER_HOME_ASSISTANT_CLOUDLINK_MQTT_TOPIC_PREFIX",
            topic_prefix.to_string(),
        ),
        (
            "AETHER_HOME_ASSISTANT_CLOUDLINK_MQTT_USERNAME",
            "aether-edge-ha-e2e".to_string(),
        ),
        (
            "AETHER_HOME_ASSISTANT_CLOUDLINK_MQTT_PASSWORD_REF",
            format!("env:{MQTT_PASSWORD_SETTING}"),
        ),
        (
            "AETHER_HOME_ASSISTANT_CLOUDLINK_CREDENTIAL_ID",
            "development-binding-17".to_string(),
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
            format!("env:{CONTROL_CLOUD_PUBLIC_KEY_SETTING}"),
        ),
    ]);
    HomeAssistantRuntimeConfig::from_lookup(|name| values.get(name).cloned())
        .expect("real Broker test configuration")
}

fn write_policy(root: &Path, gateway_id: &str) {
    let path = root.join("control-policy.json");
    std::fs::write(
        &path,
        serde_json::to_vec(&json!({
            "schema": "aether.edge.integration-control-policy.v1",
            "gateway_id": gateway_id,
            "integration_id": INTEGRATION_ID,
            "permission": "integration.device.control",
            "commissioned_entities": [ENTITY_ID],
            "delegated_entities": [ENTITY_ID],
            "allowed_subjects": ["user-homeowner"]
        }))
        .expect("policy JSON"),
    )
    .expect("write local control policy");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
        .expect("private local control policy");
}

async fn run_mock_home_assistant(
    listener: TcpListener,
    service_called: oneshot::Sender<()>,
    server_stop: oneshot::Receiver<()>,
) {
    let (stream, _) = listener.accept().await.expect("Home Assistant client");
    let mut socket = accept_async(stream)
        .await
        .expect("Home Assistant WebSocket");
    send_json(&mut socket, json!({"type": "auth_required"})).await;
    assert_eq!(
        read_json(&mut socket).await,
        json!({"type": "auth", "access_token": ACCESS_TOKEN})
    );
    send_json(&mut socket, json!({"type": "auth_ok"})).await;

    for _ in 0..2 {
        let command = read_json(&mut socket).await;
        send_json(
            &mut socket,
            json!({"id": command["id"], "type": "result", "success": true, "result": null}),
        )
        .await;
    }
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
                "id": ENTITY_ID,
                "entity_id": "light.bedroom",
                "name": null,
                "original_name": "Bedroom light",
                "platform": "aether-e2e",
                "device_id": null,
                "area_id": null,
                "labels": []
            }]),
            "get_states" => json!([{
                "entity_id": "light.bedroom",
                "state": "on",
                "attributes": {},
                "last_updated": "2026-07-17T10:00:00Z",
                "context": {"id": "ctx-ha-state-e2e"}
            }]),
            _ => unreachable!("closed Home Assistant command set"),
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
        }),
        "the real runtime must emit only the fixed mapped power operation"
    );
    send_json(
        &mut socket,
        json!({
            "id": command["id"],
            "type": "result",
            "success": true,
            "result": {
                "context": {
                    "id": "ctx-ha-control-e2e",
                    "parent_id": null,
                    "user_id": "edge-e2e"
                },
                "response": null
            }
        }),
    )
    .await;
    let _ = service_called.send(());
    let _ = server_stop.await;
}

async fn read_json(socket: &mut WebSocketStream<TcpStream>) -> Value {
    let message = socket
        .next()
        .await
        .expect("Home Assistant client frame")
        .expect("valid Home Assistant client frame");
    serde_json::from_str(message.to_text().expect("Home Assistant text frame"))
        .expect("Home Assistant client JSON")
}

async fn send_json(socket: &mut WebSocketStream<TcpStream>, value: Value) {
    socket
        .send(Message::Text(value.to_string().into()))
        .await
        .expect("Home Assistant server response");
}

async fn wait_for_cloud_event(path: &str, event: &str, timeout: Duration) {
    let deadline = tokio::time::Instant::now() + timeout;
    while tokio::time::Instant::now() < deadline {
        if read_to_string(path).ok().is_some_and(|source| {
            source.lines().any(|line| {
                serde_json::from_str::<Value>(line)
                    .ok()
                    .is_some_and(|value| value["event"] == event)
            })
        }) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("Cloud evidence event {event} was not observed");
}

async fn wait_for_control_ack(path: &Path) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while tokio::time::Instant::now() < deadline {
        if read(path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
            .is_some_and(|document| {
                document["last_ack"].is_object()
                    && document["pending_positions"]
                        .as_array()
                        .is_some_and(Vec::is_empty)
            })
        {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("Cloud durable ACK did not clear the Edge receipt ledger");
}

async fn assert_spool_drained(path: &Path, stream_id: &str) {
    let spool = FileCloudLinkSpool::open(path, stream_id, 4_096).expect("reopen CloudLink spool");
    assert_eq!(
        spool
            .status()
            .await
            .expect("CloudLink spool status")
            .pending_records(),
        0,
        "Cloud application ACK must drain {stream_id}"
    );
}

fn append_evidence(path: &str, event: Value) {
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .expect("open temporary evidence log");
    let mut bytes = serde_json::to_vec(&event).expect("Edge evidence JSON");
    bytes.push(b'\n');
    file.write_all(&bytes).expect("append Edge evidence");
    file.sync_data().expect("sync Edge evidence");
}

fn required_environment(name: &str) -> String {
    std::env::var(name)
        .ok()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| panic!("{name} is required"))
}
