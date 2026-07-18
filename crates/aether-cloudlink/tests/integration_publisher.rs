use std::path::Path;
use std::sync::Arc;

use aether_cloudlink::{
    CLOUDLINK_INTEGRATION_EXTENSION, CandidateMessage, CloudLinkCodec,
    CloudLinkIntegrationExtension, CloudLinkIntegrationPublisher, GatewaySessionAuthenticator,
    SessionBinding, UplinkAuthentication,
};
use aether_domain::TimestampMs;
use aether_integration_contract::IntegrationContractCodec;
use aether_ports::{
    CloudLinkSpool, CloudLinkTransport, CloudLinkTransportEvent, CloudLinkTransportMessage,
    CloudLinkTransportRoute,
};
use aether_store_local::FileCloudLinkSpool;
use aether_testkit::MemoryCloudLinkTransport;
use ed25519_dalek::SigningKey;
use serde_json::json;

fn fixture(relative: &str) -> Vec<u8> {
    std::fs::read(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../aether-integration-contract/tests/fixtures/integration/v1alpha1")
            .join(relative),
    )
    .expect("pinned Integration fixture")
}

fn session() -> SessionBinding {
    SessionBinding::new(
        "33333333-3333-4333-8333-333333333333",
        "44444444-4444-4444-8444-444444444444",
        9,
        3,
    )
    .expect("verified session")
}

fn resumed_session() -> SessionBinding {
    SessionBinding::new(
        "33333333-3333-4333-8333-333333333333",
        "55555555-5555-4555-8555-555555555555",
        10,
        3,
    )
    .expect("resumed verified session")
}

fn authentication() -> UplinkAuthentication {
    GatewaySessionAuthenticator::new(
        "development-cloud-key-1",
        SigningKey::from_bytes(&[7_u8; 32])
            .verifying_key()
            .to_bytes(),
        "development-gateway-key-17",
        [9_u8; 32],
    )
    .expect("session authenticator")
    .uplink_authentication()
}

async fn seed(
    topology: &Arc<FileCloudLinkSpool>,
    observations: &Arc<FileCloudLinkSpool>,
) -> CloudLinkIntegrationExtension {
    let extension = CloudLinkIntegrationExtension::enable_cloud_first(
        &[CLOUDLINK_INTEGRATION_EXTENSION],
        &[CLOUDLINK_INTEGRATION_EXTENSION],
        "home-assistant.home",
        &topology.status().await.expect("topology status"),
        &observations.status().await.expect("observation status"),
    )
    .expect("extension");
    let public_topology =
        IntegrationContractCodec::decode_topology(&fixture("valid/home-assistant-topology.json"))
            .expect("topology fixture");
    let public_observations = IntegrationContractCodec::decode_observation_batch(
        &fixture("valid/home-assistant-observations.json"),
        &public_topology,
    )
    .expect("observation fixture");
    topology
        .enqueue(
            extension
                .prepare_topology(&public_topology, TimestampMs::new(1_784_217_600_000), None)
                .expect("topology input"),
        )
        .await
        .expect("topology enqueue");
    for input in extension
        .prepare_observation_batches(
            &public_topology,
            &public_observations,
            TimestampMs::new(1_784_217_600_100),
            None,
        )
        .expect("observation inputs")
    {
        observations
            .enqueue(input)
            .await
            .expect("observation enqueue");
    }
    extension
}

async fn drain_connected(transport: &MemoryCloudLinkTransport) {
    assert_eq!(
        transport.receive().await.expect("connected"),
        CloudLinkTransportEvent::Connected
    );
}

async fn inbound(transport: &MemoryCloudLinkTransport) -> CloudLinkTransportMessage {
    match transport.receive().await.expect("inbound") {
        CloudLinkTransportEvent::Inbound(message) => message,
        other => panic!("unexpected transport event: {other:?}"),
    }
}

#[tokio::test]
async fn file_spools_replay_after_restart_and_application_ack_is_written_back() {
    let root = tempfile::tempdir().expect("temporary directory");
    let topology_path = root.path().join("topology.spool");
    let observations_path = root.path().join("observations.spool");
    let first_payloads = {
        let topology = Arc::new(
            FileCloudLinkSpool::open(&topology_path, "ha-topology", 32).expect("topology spool"),
        );
        let observations = Arc::new(
            FileCloudLinkSpool::open(&observations_path, "ha-observations", 32)
                .expect("observation spool"),
        );
        let extension = seed(&topology, &observations).await;
        let (edge, cloud) = MemoryCloudLinkTransport::pair(16).expect("transport pair");
        let edge: Arc<dyn CloudLinkTransport> = Arc::new(edge);
        let publisher = CloudLinkIntegrationPublisher::new(
            extension,
            topology,
            observations,
            edge,
            session(),
            authentication(),
        );
        drain_connected(&cloud).await;
        publisher
            .receive_and_process()
            .await
            .expect("connected replay");
        let first = inbound(&cloud).await;
        let second = inbound(&cloud).await;
        vec![first.payload().to_vec(), second.payload().to_vec()]
    };

    let topology = Arc::new(
        FileCloudLinkSpool::open(&topology_path, "ha-topology", 32).expect("reopen topology"),
    );
    let observations = Arc::new(
        FileCloudLinkSpool::open(&observations_path, "ha-observations", 32)
            .expect("reopen observations"),
    );
    let extension = CloudLinkIntegrationExtension::enable_cloud_first(
        &[CLOUDLINK_INTEGRATION_EXTENSION],
        &[CLOUDLINK_INTEGRATION_EXTENSION],
        "home-assistant.home",
        &topology.status().await.expect("topology status"),
        &observations.status().await.expect("observation status"),
    )
    .expect("recovered extension");
    let (edge, cloud) = MemoryCloudLinkTransport::pair(16).expect("restarted transport pair");
    let edge: Arc<dyn CloudLinkTransport> = Arc::new(edge);
    let publisher = CloudLinkIntegrationPublisher::new(
        extension,
        topology.clone(),
        observations.clone(),
        edge,
        resumed_session(),
        authentication(),
    );
    drain_connected(&cloud).await;
    publisher
        .receive_and_process()
        .await
        .expect("restart replay");
    let replayed = [inbound(&cloud).await, inbound(&cloud).await];
    for (before, after) in first_payloads.iter().zip(&replayed) {
        assert_eq!(
            immutable_delivery_facts(before),
            immutable_delivery_facts(after.payload()),
            "restart and a newer session must preserve every durable delivery fact"
        );
        assert_ne!(
            delivery_signature(before),
            delivery_signature(after.payload()),
            "the immutable fact must be re-signed for the newer session"
        );
    }

    publisher
        .receive_and_process()
        .await
        .expect("topology transport publication");
    publisher
        .receive_and_process()
        .await
        .expect("observation transport publication");
    let observation_delivery = replayed
        .iter()
        .find(|message| message.route() == CloudLinkTransportRoute::IntegrationObservationsUp)
        .expect("observation delivery");
    let decoded =
        CloudLinkCodec::decode(observation_delivery.payload()).expect("delivery envelope");
    let delivery = match decoded {
        CandidateMessage::Delivery(delivery) => delivery,
        other => panic!("unexpected message: {other:?}"),
    };
    let identity = observation_delivery.delivery().expect("durable identity");
    let ack = json!({
        "schema": "aether.cloudlink.durable-ack.v1",
        "protocol": "aether.cloudlink",
        "protocol_version": "1.0",
        "message_kind": "durable-ack",
        "gateway_id": resumed_session().gateway_id(),
        "session_id": resumed_session().session_id(),
        "session_epoch": resumed_session().session_epoch().to_string(),
        "credential_generation": resumed_session().credential_generation().to_string(),
        "stream_id": identity.stream_id(),
        "stream_epoch": identity.stream_epoch().to_string(),
        "acknowledged_position": identity.position().to_string(),
        "batch_id": delivery.delivery().batch_id(),
        "digest": delivery.delivery().digest(),
        "receipt_id": "cloud-receipt-observations-1",
        "acknowledged_at_ms": "1784217600400"
    });
    cloud
        .send(CloudLinkTransportMessage::new(
            CloudLinkTransportRoute::AckDown,
            serde_json::to_vec(&ack).expect("ACK JSON"),
            None,
        ))
        .await
        .expect("cloud ACK");
    publisher
        .receive_and_process()
        .await
        .expect("application ACK writeback");

    assert_eq!(
        observations
            .status()
            .await
            .expect("observation status")
            .pending_records(),
        0
    );
    assert_eq!(
        topology
            .status()
            .await
            .expect("topology status")
            .pending_records(),
        1
    );
}

#[tokio::test]
async fn replay_within_one_session_reuses_the_exact_signed_payload_bytes() {
    let root = tempfile::tempdir().expect("temporary directory");
    let topology = Arc::new(
        FileCloudLinkSpool::open(root.path().join("topology.spool"), "ha-topology", 32)
            .expect("topology spool"),
    );
    let observations = Arc::new(
        FileCloudLinkSpool::open(
            root.path().join("observations.spool"),
            "ha-observations",
            32,
        )
        .expect("observation spool"),
    );
    let extension = seed(&topology, &observations).await;
    let (edge, cloud) = MemoryCloudLinkTransport::pair(16).expect("transport pair");
    let edge: Arc<dyn CloudLinkTransport> = Arc::new(edge);
    let publisher = CloudLinkIntegrationPublisher::new(
        extension,
        topology,
        observations,
        edge,
        session(),
        authentication(),
    );
    drain_connected(&cloud).await;
    publisher
        .receive_and_process()
        .await
        .expect("connected replay");
    let initial = [inbound(&cloud).await, inbound(&cloud).await];
    let topology_message = initial
        .iter()
        .find(|message| message.route() == CloudLinkTransportRoute::IntegrationTopologyUp)
        .expect("topology offer");
    let identity = topology_message
        .delivery()
        .expect("durable topology identity")
        .clone();
    let exact_first_payload = topology_message.payload().to_vec();

    publisher.receive_and_process().await.expect("first PUBACK");
    publisher
        .receive_and_process()
        .await
        .expect("second PUBACK");
    let request = json!({
        "schema": "aether.cloudlink.replay-request.v1",
        "protocol": "aether.cloudlink",
        "protocol_version": "1.0",
        "message_kind": "replay-request",
        "gateway_id": session().gateway_id(),
        "session_id": session().session_id(),
        "session_epoch": session().session_epoch().to_string(),
        "credential_generation": session().credential_generation().to_string(),
        "stream_id": identity.stream_id(),
        "stream_epoch": identity.stream_epoch().to_string(),
        "from_position": identity.position().to_string(),
        "requested_at_ms": "1784217600400"
    });
    cloud
        .send(CloudLinkTransportMessage::new(
            CloudLinkTransportRoute::ReplayDown,
            serde_json::to_vec(&request).expect("replay JSON"),
            None,
        ))
        .await
        .expect("request replay");
    publisher.receive_and_process().await.expect("serve replay");
    let replay = inbound(&cloud).await;

    assert_eq!(
        replay.payload(),
        exact_first_payload,
        "business envelope, sent time, and signature must remain byte-identical within a session"
    );
}

fn immutable_delivery_facts(bytes: &[u8]) -> serde_json::Value {
    let value: serde_json::Value = serde_json::from_slice(bytes).expect("delivery JSON");
    json!({
        "message_kind": value["message_kind"],
        "sent_at_ms": value["sent_at_ms"],
        "expires_at_ms": value.get("expires_at_ms"),
        "delivery": value["delivery"],
        "payload": value["payload"],
    })
}

fn delivery_signature(bytes: &[u8]) -> String {
    serde_json::from_slice::<serde_json::Value>(bytes)
        .expect("delivery JSON")["message_authentication"]["signature"]
        .as_str()
        .expect("Gateway signature")
        .to_owned()
}
