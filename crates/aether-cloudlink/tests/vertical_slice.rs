use std::path::Path;

use aether_cloudlink::{
    CandidateMessage, CloudLinkCodec, GatewaySessionAuthenticator, HeartbeatMessage,
    MessageAuthentication, SessionBinding, SessionHello, TopologyBinding, UplinkAuthentication,
};
use aether_domain::{
    InstanceId, PointAddress, PointId, PointKind, PointQuality, PointSample, TimestampMs,
};
use aether_ports::{
    CloudLinkMessageKind, CloudLinkSpool, CloudLinkTransport, CloudLinkTransportEvent,
    CloudLinkTransportMessage, CloudLinkTransportRoute, DurableAckOutcome,
};
use aether_store_local::MemoryCloudLinkSpool;
use aether_testkit::MemoryCloudLinkTransport;
use ed25519_dalek::SigningKey;
use serde_json::{Value, json};

#[tokio::test]
async fn deterministic_edge_slice_negotiates_reports_and_replays_until_application_ack() {
    let authentication = GatewaySessionAuthenticator::new(
        "development-cloud-key-1",
        SigningKey::from_bytes(&[7_u8; 32])
            .verifying_key()
            .to_bytes(),
        "development-gateway-key-17",
        [9_u8; 32],
    )
    .expect("session authenticator")
    .uplink_authentication();
    let (edge, cloud) = MemoryCloudLinkTransport::pair(32).expect("transport pair");
    assert_eq!(
        edge.receive().await.expect("edge connected"),
        CloudLinkTransportEvent::Connected
    );
    assert_eq!(
        cloud.receive().await.expect("cloud connected"),
        CloudLinkTransportEvent::Connected
    );

    let hello = SessionHello::new_gateway_signed(
        "33333333-3333-4333-8333-333333333333",
        "development-binding-17",
        3,
        "22222222-2222-4222-8222-222222222222",
        "development-gateway-key-17",
        MessageAuthentication::new("development-gateway-key-17", "E".repeat(86))
            .expect("signature shape"),
        vec!["1.0".to_string()],
        "A".repeat(43),
        vec![aether_cloudlink::ResumeCursor::new("business", 1, 0).expect("cursor")],
    )
    .expect("hello");
    edge.send(CloudLinkTransportMessage::new(
        CloudLinkTransportRoute::SessionUp,
        CloudLinkCodec::encode(&hello).expect("hello JSON"),
        None,
    ))
    .await
    .expect("send hello");
    let inbound_hello = inbound(&cloud).await;
    assert!(matches!(
        CloudLinkCodec::decode(inbound_hello.payload()).expect("decode hello"),
        CandidateMessage::SessionHello(_)
    ));

    let accepted = fixture("session-accepted.valid.json");
    cloud
        .send(CloudLinkTransportMessage::new(
            CloudLinkTransportRoute::SessionDown,
            accepted,
            None,
        ))
        .await
        .expect("accept session");
    let session =
        match CloudLinkCodec::decode(inbound(&edge).await.payload()).expect("decode acceptance") {
            CandidateMessage::SessionAccepted(value) => value
                .bind("33333333-3333-4333-8333-333333333333", 3, &["1.0"], 6)
                .expect("current session"),
            other => panic!("unexpected message: {other:?}"),
        };

    let heartbeat = HeartbeatMessage::new(
        &session,
        TimestampMs::new(1_721_000_000_123),
        Vec::new(),
        &authentication,
    )
    .expect("heartbeat");
    edge.send(CloudLinkTransportMessage::new(
        CloudLinkTransportRoute::HeartbeatUp,
        CloudLinkCodec::encode(&heartbeat).expect("heartbeat JSON"),
        None,
    ))
    .await
    .expect("heartbeat send");
    let heartbeat_up = inbound(&cloud).await;
    assert!(matches!(
        CloudLinkCodec::decode(heartbeat_up.payload()).expect("heartbeat decode"),
        CandidateMessage::Heartbeat(_)
    ));
    let heartbeat_ack =
        HeartbeatMessage::ack(&session, TimestampMs::new(1_721_000_000_124), Vec::new())
            .expect("heartbeat ACK");
    cloud
        .send(CloudLinkTransportMessage::new(
            CloudLinkTransportRoute::AckDown,
            CloudLinkCodec::encode(&heartbeat_ack).expect("heartbeat ACK JSON"),
            None,
        ))
        .await
        .expect("heartbeat ACK send");
    CloudLinkCodec::decode(inbound(&edge).await.payload())
        .expect("heartbeat ACK decode")
        .validate_session(&session)
        .expect("heartbeat ACK session");

    let spool = MemoryCloudLinkSpool::new("business", 8).expect("spool");
    let manifest = std::fs::read(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../config.template/runtime-manifest.json"),
    )
    .expect("manifest");
    let report =
        CloudLinkCodec::runtime_manifest_report(&manifest, TimestampMs::new(1_721_000_000_123))
            .expect("report");
    let manifest_record = spool
        .enqueue(
            CloudLinkCodec::prepare(
                CloudLinkMessageKind::RuntimeManifestReport,
                "manifest-1",
                &report,
                TimestampMs::new(1_721_000_000_123),
                None,
            )
            .expect("manifest content"),
        )
        .await
        .expect("manifest record");
    offer(
        &edge,
        &spool,
        &session,
        &manifest_record,
        CloudLinkTransportRoute::ManifestUp,
        &authentication,
    )
    .await;
    let manifest_envelope =
        CloudLinkCodec::decode(inbound(&cloud).await.payload()).expect("manifest envelope");
    assert!(matches!(manifest_envelope, CandidateMessage::Delivery(_)));
    apply_cloud_ack(&cloud, &edge, &spool, &session, &manifest_record).await;

    let sample = PointSample::new(
        PointAddress::new(InstanceId::new(42), PointKind::Telemetry, PointId::new(8)),
        12.5,
        TimestampMs::new(1_721_000_000_123),
        PointQuality::Uncertain,
    );
    let batch = CloudLinkCodec::telemetry_batch(
        TopologyBinding::new(11, "fx64:0123456789abcdef").expect("topology"),
        &[sample],
    )
    .expect("telemetry");
    let record = spool
        .enqueue(
            CloudLinkCodec::prepare(
                CloudLinkMessageKind::TelemetryBatch,
                "batch-1",
                &batch,
                TimestampMs::new(1_721_000_000_123),
                None,
            )
            .expect("telemetry content"),
        )
        .await
        .expect("telemetry record");
    offer(
        &edge,
        &spool,
        &session,
        &record,
        CloudLinkTransportRoute::TelemetryUp,
        &authentication,
    )
    .await;
    let first_delivery = inbound(&cloud).await;
    let first = delivery_signature(first_delivery.payload());
    assert_eq!(spool.status().await.expect("pending").pending_records(), 1);

    edge.disconnect().await.expect("fault-injected disconnect");
    assert_eq!(
        edge.receive().await.expect("disconnect event"),
        CloudLinkTransportEvent::Disconnected
    );
    assert_eq!(
        cloud.receive().await.expect("peer disconnect event"),
        CloudLinkTransportEvent::Disconnected
    );

    let resumed = SessionBinding::new(
        "33333333-3333-4333-8333-333333333333",
        "55555555-5555-4555-8555-555555555555",
        8,
        3,
    )
    .expect("resumed session");
    let replayed = spool
        .replay_from(record.identity().position(), 1)
        .await
        .expect("replay")
        .records()[0]
        .clone();
    offer(
        &edge,
        &spool,
        &resumed,
        &replayed,
        CloudLinkTransportRoute::TelemetryUp,
        &authentication,
    )
    .await;
    let replay = inbound(&cloud).await;
    assert_eq!(delivery_signature(replay.payload()), first);
    apply_cloud_ack(&cloud, &edge, &spool, &resumed, &replayed).await;
    assert_eq!(spool.status().await.expect("empty").pending_records(), 0);
}

async fn offer(
    transport: &MemoryCloudLinkTransport,
    spool: &dyn CloudLinkSpool,
    session: &SessionBinding,
    record: &aether_ports::CloudLinkRecord,
    route: CloudLinkTransportRoute,
    authentication: &UplinkAuthentication,
) {
    spool
        .mark_offered(record.identity(), &session.spool_binding())
        .await
        .expect("mark offered");
    let envelope =
        CloudLinkCodec::delivery_envelope(session, record, None, authentication).expect("envelope");
    transport
        .send(CloudLinkTransportMessage::new(
            route,
            CloudLinkCodec::encode(&envelope).expect("envelope JSON"),
            Some(record.identity().clone()),
        ))
        .await
        .expect("offer");
    assert_eq!(
        transport.receive().await.expect("transport published"),
        CloudLinkTransportEvent::TransportPublished(record.identity().clone())
    );
    spool
        .mark_transport_published(record.identity(), &session.spool_binding())
        .await
        .expect("mark transport published");
}

async fn apply_cloud_ack(
    cloud: &MemoryCloudLinkTransport,
    edge: &MemoryCloudLinkTransport,
    spool: &dyn CloudLinkSpool,
    session: &SessionBinding,
    record: &aether_ports::CloudLinkRecord,
) {
    let ack = json!({
        "schema": "aether.cloudlink.durable-ack.v1",
        "protocol": "aether.cloudlink",
        "protocol_version": "1.0",
        "message_kind": "durable-ack",
        "gateway_id": session.gateway_id(),
        "session_id": session.session_id(),
        "session_epoch": session.session_epoch().to_string(),
        "credential_generation": session.credential_generation().to_string(),
        "stream_id": record.identity().stream_id(),
        "stream_epoch": record.identity().stream_epoch().to_string(),
        "acknowledged_position": record.identity().position().to_string(),
        "batch_id": record.batch_id(),
        "digest": record.digest(),
        "receipt_id": format!("receipt-{}", record.identity().position()),
        "acknowledged_at_ms": "1721000000300"
    });
    cloud
        .send(CloudLinkTransportMessage::new(
            CloudLinkTransportRoute::AckDown,
            serde_json::to_vec(&ack).expect("ACK JSON"),
            None,
        ))
        .await
        .expect("ACK send");
    let message = inbound(edge).await;
    let ack = match CloudLinkCodec::decode(message.payload()).expect("ACK decode") {
        CandidateMessage::DurableAck(value) => {
            value.to_spool_ack(session).expect("current-session ACK")
        },
        other => panic!("unexpected message: {other:?}"),
    };
    assert_eq!(
        spool.acknowledge(&ack).await.expect("apply ACK"),
        DurableAckOutcome::Applied { removed: 1 }
    );
    assert_eq!(
        spool.acknowledge(&ack).await.expect("duplicate ACK"),
        DurableAckOutcome::Duplicate
    );
}

async fn inbound(transport: &MemoryCloudLinkTransport) -> CloudLinkTransportMessage {
    match transport.receive().await.expect("transport event") {
        CloudLinkTransportEvent::Inbound(message) => message,
        other => panic!("unexpected transport event: {other:?}"),
    }
}

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../contracts/cloudlink/v1/fixtures")
            .join(name),
    )
    .expect("fixture")
}

fn delivery_signature(bytes: &[u8]) -> (String, u64, u64, String, String) {
    let value: Value = serde_json::from_slice(bytes).expect("delivery JSON");
    (
        value["delivery"]["stream_id"]
            .as_str()
            .unwrap_or_default()
            .to_string(),
        value["delivery"]["stream_epoch"]
            .as_str()
            .and_then(|value| value.parse().ok())
            .unwrap_or_default(),
        value["delivery"]["position"]
            .as_str()
            .and_then(|value| value.parse().ok())
            .unwrap_or_default(),
        value["delivery"]["batch_id"]
            .as_str()
            .unwrap_or_default()
            .to_string(),
        value["delivery"]["digest"]
            .as_str()
            .unwrap_or_default()
            .to_string(),
    )
}
