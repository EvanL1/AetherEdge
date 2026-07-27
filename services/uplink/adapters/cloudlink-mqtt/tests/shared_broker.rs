//! Opt-in real MQTT broker vertical-slice harness.
//!
//! The default opt-in test uses a fake Cloud peer to isolate the Edge binding.
//! Separate phase tests are driven by AetherCloud's real-ingress dual harness;
//! neither path claims production authentication or crash durability.

use std::collections::BTreeSet;
use std::env;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use aether_cloudlink::{
    CandidateMessage, CloudLinkCodec, GatewaySessionAuthenticator, MessageAuthentication,
    SessionBinding, SessionHello, TopologyBinding, UplinkAuthentication,
};
use aether_cloudlink_mqtt::{
    CLOUDLINK_MQTT_QOS, CLOUDLINK_MQTT_RETAIN, CloudLinkMqttConfig, CloudLinkTlsConfig,
    DeploymentSecurity, MqttClientIdentity, MqttCloudLinkTransport, SecretString, TopicNamespace,
};
use aether_domain::{
    InstanceId, PointAddress, PointId, PointKind, PointQuality, PointSample, TimestampMs,
};
use aether_ports::{
    CloudLinkMessageKind, CloudLinkSpool, CloudLinkTransport, CloudLinkTransportEvent,
    CloudLinkTransportMessage, CloudLinkTransportRoute,
};
use aether_store_local::{FileCloudLinkSpool, MemoryCloudLinkSpool};
use ed25519_dalek::SigningKey;
use rumqttc::tokio_native_tls::native_tls::{Certificate, Identity, TlsConnector};
use rumqttc::{AsyncClient, Event, Incoming, MqttOptions, Transport};
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio::time::{Instant, timeout};

const WAIT: Duration = Duration::from_secs(15);
const TEST_CREDENTIAL_GENERATION: u64 = 1;

fn test_uplink_authentication() -> UplinkAuthentication {
    GatewaySessionAuthenticator::new(
        "development-cloud-key",
        SigningKey::from_bytes(&[7_u8; 32])
            .verifying_key()
            .to_bytes(),
        "development-integration-key",
        [9_u8; 32],
    )
    .expect("test session authenticator")
    .uplink_authentication()
}

#[derive(Debug)]
enum FakeCloudObservation {
    Ready,
    FirstTelemetry,
    ReplayedTelemetry,
    Failure(String),
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shared_broker_session_manifest_telemetry_ack_and_replay() {
    if env::var("AETHER_CLOUDLINK_RUN_INTEGRATION").as_deref() != Ok("1") {
        return;
    }
    if let Ok(cloud_root) = env::var("AETHERCLOUD_ROOT") {
        assert!(
            Path::new(&cloud_root).join("AGENTS.md").is_file(),
            "AETHERCLOUD_ROOT must name a readable AetherCloud checkout"
        );
    }

    let run_id = format!("{}", std::process::id());
    let gateway_id = format!("33333333-3333-4333-8333-{:012x}", std::process::id());
    let topics = TopicNamespace::new(&format!("aether-integration/{run_id}"), &gateway_id)
        .expect("integration topic namespace");
    let config = integration_config(&gateway_id);
    let security = if matches!(config.tls, CloudLinkTlsConfig::Disabled) {
        DeploymentSecurity::Development
    } else {
        DeploymentSecurity::Production
    };
    config
        .validate(security)
        .expect("integration configuration");

    let (cloud_events_tx, mut cloud_events_rx) = mpsc::channel(16);
    let cloud = tokio::spawn(fake_cloud(
        config.clone(),
        topics.clone(),
        gateway_id.clone(),
        cloud_events_tx,
    ));
    expect_cloud(&mut cloud_events_rx, |event| {
        matches!(event, FakeCloudObservation::Ready)
    })
    .await;

    let spool = Arc::new(MemoryCloudLinkSpool::new("business", 16).expect("spool"));
    let first_transport = MqttCloudLinkTransport::connect(config.clone(), topics.clone(), security)
        .expect("edge transport");
    wait_connected(&first_transport).await;
    send_hello(&first_transport, &gateway_id, 0).await;
    let first_session = wait_session(&first_transport, &gateway_id, 0).await;

    let heartbeat = CloudLinkCodec::encode(
        &aether_cloudlink::HeartbeatMessage::new(
            &first_session,
            TimestampMs::new(1_721_000_000_123),
            Vec::new(),
            &test_uplink_authentication(),
        )
        .expect("heartbeat"),
    )
    .expect("heartbeat JSON");
    first_transport
        .send(CloudLinkTransportMessage::new(
            CloudLinkTransportRoute::HeartbeatUp,
            heartbeat,
            None,
        ))
        .await
        .expect("send heartbeat");
    wait_heartbeat_ack(&first_transport, &first_session).await;

    let manifest_path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../config.template/runtime-manifest.json");
    let manifest = std::fs::read(manifest_path).expect("runtime manifest fixture");
    let manifest_payload =
        CloudLinkCodec::runtime_manifest_report(&manifest, TimestampMs::new(1_721_000_000_123))
            .expect("manifest report");
    let manifest_record = spool
        .enqueue(
            CloudLinkCodec::prepare(
                CloudLinkMessageKind::RuntimeManifestReport,
                "manifest-integration",
                &manifest_payload,
                TimestampMs::new(1_721_000_000_123),
                None,
            )
            .expect("sealed manifest"),
        )
        .await
        .expect("spooled manifest");
    offer(
        &first_transport,
        spool.as_ref(),
        &first_session,
        &manifest_record,
        CloudLinkTransportRoute::ManifestUp,
    )
    .await;
    wait_durable_ack(&first_transport, spool.as_ref(), &first_session).await;

    let sample = PointSample::new(
        PointAddress::new(InstanceId::new(42), PointKind::Telemetry, PointId::new(8)),
        12.5,
        TimestampMs::new(1_721_000_000_123),
        PointQuality::Uncertain,
    );
    let telemetry = CloudLinkCodec::telemetry_batch(
        TopologyBinding::new(11, "fx64:0123456789abcdef").expect("topology"),
        &[sample],
    )
    .expect("point telemetry");
    let telemetry_record = spool
        .enqueue(
            CloudLinkCodec::prepare(
                CloudLinkMessageKind::TelemetryBatch,
                "telemetry-integration",
                &telemetry,
                TimestampMs::new(1_721_000_000_123),
                Some(TimestampMs::new(1_721_003_600_000)),
            )
            .expect("sealed telemetry"),
        )
        .await
        .expect("spooled telemetry");
    offer(
        &first_transport,
        spool.as_ref(),
        &first_session,
        &telemetry_record,
        CloudLinkTransportRoute::TelemetryUp,
    )
    .await;
    expect_cloud(&mut cloud_events_rx, |event| {
        matches!(event, FakeCloudObservation::FirstTelemetry)
    })
    .await;
    assert_eq!(spool.status().await.expect("status").pending_records(), 1);

    drop(first_transport);
    tokio::time::sleep(Duration::from_millis(250)).await;
    let resumed_transport =
        MqttCloudLinkTransport::connect(config, topics, security).expect("resumed edge transport");
    wait_connected(&resumed_transport).await;
    send_hello(&resumed_transport, &gateway_id, 1).await;
    let resumed_session = wait_session(&resumed_transport, &gateway_id, 1).await;
    wait_replay_request(&resumed_transport, &resumed_session, &telemetry_record).await;

    let replayed = spool
        .replay_from(telemetry_record.identity().position(), 1)
        .await
        .expect("replay window")
        .records()[0]
        .clone();
    assert_eq!(replayed.identity(), telemetry_record.identity());
    assert_eq!(replayed.batch_id(), telemetry_record.batch_id());
    assert_eq!(replayed.digest(), telemetry_record.digest());
    offer(
        &resumed_transport,
        spool.as_ref(),
        &resumed_session,
        &replayed,
        CloudLinkTransportRoute::TelemetryUp,
    )
    .await;
    expect_cloud(&mut cloud_events_rx, |event| {
        matches!(event, FakeCloudObservation::ReplayedTelemetry)
    })
    .await;
    wait_durable_ack(&resumed_transport, spool.as_ref(), &resumed_session).await;
    assert_eq!(spool.status().await.expect("status").pending_records(), 0);

    cloud.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn external_cloud_dual_harness() {
    if env::var("AETHER_CLOUDLINK_EXTERNAL_CLOUD").as_deref() != Ok("1") {
        return;
    }

    let gateway_id = env::var("AETHER_CLOUDLINK_GATEWAY_ID").expect("dual Gateway ID");
    let topic_prefix = env::var("AETHER_CLOUDLINK_TOPIC_PREFIX").expect("dual topic prefix");
    let evidence_path = env::var("AETHER_CLOUDLINK_EDGE_EVIDENCE").expect("edge evidence path");
    let topics = TopicNamespace::new(&topic_prefix, &gateway_id).expect("dual topic namespace");
    let config = integration_config(&gateway_id);
    config
        .validate(DeploymentSecurity::Development)
        .expect("dual development configuration");

    let spool_root = tempfile::tempdir().expect("dual spool directory");
    let manifest_path = spool_root.path().join("manifest.spool");
    let telemetry_path = spool_root.path().join("telemetry.spool");
    let manifest_spool =
        FileCloudLinkSpool::open(&manifest_path, "manifest", 8).expect("manifest spool");
    let telemetry_spool =
        FileCloudLinkSpool::open(&telemetry_path, "telemetry", 8).expect("telemetry spool");

    let first_transport = MqttCloudLinkTransport::connect(
        config.clone(),
        topics.clone(),
        DeploymentSecurity::Development,
    )
    .expect("first Edge transport");
    wait_connected(&first_transport).await;
    send_hello(&first_transport, &gateway_id, 0).await;
    let first_session = wait_session(&first_transport, &gateway_id, 0).await;

    let heartbeat = CloudLinkCodec::encode(
        &aether_cloudlink::HeartbeatMessage::new(
            &first_session,
            TimestampMs::new(1_721_000_000_123),
            Vec::new(),
            &test_uplink_authentication(),
        )
        .expect("heartbeat"),
    )
    .expect("heartbeat JSON");
    first_transport
        .send(CloudLinkTransportMessage::new(
            CloudLinkTransportRoute::HeartbeatUp,
            heartbeat,
            None,
        ))
        .await
        .expect("heartbeat send");
    wait_heartbeat_ack(&first_transport, &first_session).await;

    let runtime_manifest = std::fs::read(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../config.template/runtime-manifest.json"),
    )
    .expect("runtime manifest");
    let manifest_payload = CloudLinkCodec::runtime_manifest_report(
        &runtime_manifest,
        TimestampMs::new(1_721_000_000_123),
    )
    .expect("manifest report");
    let manifest_record = manifest_spool
        .enqueue(
            CloudLinkCodec::prepare(
                CloudLinkMessageKind::RuntimeManifestReport,
                "manifest-dual",
                &manifest_payload,
                TimestampMs::new(1_721_000_000_123),
                None,
            )
            .expect("sealed manifest"),
        )
        .await
        .expect("manifest enqueue");
    offer(
        &first_transport,
        &manifest_spool,
        &first_session,
        &manifest_record,
        CloudLinkTransportRoute::ManifestUp,
    )
    .await;
    assert!(
        wait_for_durable_ack(
            &first_transport,
            Some(&manifest_spool),
            &first_session,
            WAIT,
        )
        .await,
        "manifest requires an application ACK"
    );

    if env::var("AETHER_CLOUDLINK_EXPECT_BROKER_RESTART").as_deref() != Ok("0") {
        wait_for_broker_reconnect(&first_transport).await;
    }

    let sample = PointSample::new(
        PointAddress::new(InstanceId::new(42), PointKind::Telemetry, PointId::new(8)),
        12.5,
        TimestampMs::new(1_721_000_000_123),
        PointQuality::Uncertain,
    );
    let telemetry_payload = CloudLinkCodec::telemetry_batch(
        TopologyBinding::new(11, "fx64:0123456789abcdef").expect("topology"),
        &[sample],
    )
    .expect("telemetry payload");
    let telemetry_record = telemetry_spool
        .enqueue(
            CloudLinkCodec::prepare(
                CloudLinkMessageKind::TelemetryBatch,
                "telemetry-dual",
                &telemetry_payload,
                TimestampMs::new(1_721_000_000_123),
                Some(TimestampMs::new(1_721_003_600_000)),
            )
            .expect("sealed telemetry"),
        )
        .await
        .expect("telemetry enqueue");
    offer(
        &first_transport,
        &telemetry_spool,
        &first_session,
        &telemetry_record,
        CloudLinkTransportRoute::TelemetryUp,
    )
    .await;
    assert!(
        !wait_for_durable_ack(
            &first_transport,
            Some(&telemetry_spool),
            &first_session,
            Duration::from_secs(2),
        )
        .await,
        "the injected ACK loss must leave the Edge record pending"
    );
    assert_eq!(
        telemetry_spool
            .status()
            .await
            .expect("pending telemetry")
            .pending_records(),
        1
    );

    drop(first_transport);
    drop(telemetry_spool);
    tokio::time::sleep(Duration::from_secs(1)).await;
    let telemetry_spool =
        FileCloudLinkSpool::open(&telemetry_path, "telemetry", 8).expect("reopened Edge spool");
    let resumed_transport =
        MqttCloudLinkTransport::connect(config, topics, DeploymentSecurity::Development)
            .expect("resumed Edge transport");
    wait_connected(&resumed_transport).await;
    send_hello(&resumed_transport, &gateway_id, 1).await;
    let resumed_session = wait_session(
        &resumed_transport,
        &gateway_id,
        first_session.session_epoch(),
    )
    .await;

    let replayed = telemetry_spool
        .replay_from(telemetry_record.identity().position(), 1)
        .await
        .expect("replay after Edge restart")
        .records()[0]
        .clone();
    assert_eq!(replayed.identity(), telemetry_record.identity());
    assert_eq!(replayed.batch_id(), telemetry_record.batch_id());
    assert_eq!(replayed.digest(), telemetry_record.digest());
    offer(
        &resumed_transport,
        &telemetry_spool,
        &resumed_session,
        &replayed,
        CloudLinkTransportRoute::TelemetryUp,
    )
    .await;
    assert!(
        wait_for_durable_ack(
            &resumed_transport,
            Some(&telemetry_spool),
            &resumed_session,
            WAIT,
        )
        .await,
        "replayed telemetry requires the stable application ACK"
    );

    send_record_without_spool_transition(
        &resumed_transport,
        &resumed_session,
        &replayed,
        CloudLinkTransportRoute::TelemetryUp,
    )
    .await;
    assert!(
        wait_for_durable_ack(
            &resumed_transport,
            Some(&telemetry_spool),
            &resumed_session,
            WAIT,
        )
        .await,
        "an exact duplicate receives the same logical receipt"
    );

    let conflicting = contextual_fixture(
        "conflicting-replay.valid-digest.json",
        &gateway_id,
        &resumed_session,
        replayed.identity(),
        "batch-conflict",
        None,
    );
    send_raw_delivery(
        &resumed_transport,
        CloudLinkTransportRoute::TelemetryUp,
        conflicting,
        replayed.identity(),
    )
    .await;
    assert!(
        !wait_for_durable_ack(
            &resumed_transport,
            None,
            &resumed_session,
            Duration::from_secs(1),
        )
        .await,
        "a digest conflict must not receive a successful ACK"
    );

    let expired = contextual_fixture(
        "telemetry-batch.valid.json",
        &gateway_id,
        &resumed_session,
        replayed.identity(),
        "batch-expired",
        Some(("2", "1721000000400")),
    );
    send_raw_delivery(
        &resumed_transport,
        CloudLinkTransportRoute::TelemetryUp,
        expired,
        replayed.identity(),
    )
    .await;
    assert!(
        !wait_for_durable_ack(
            &resumed_transport,
            None,
            &resumed_session,
            Duration::from_secs(1),
        )
        .await,
        "an expired delivery must not receive a successful ACK"
    );

    let out_of_order = contextual_fixture(
        "telemetry-batch.valid.json",
        &gateway_id,
        &resumed_session,
        replayed.identity(),
        "batch-out-of-order",
        Some(("3", "1721003600000")),
    );
    send_raw_delivery(
        &resumed_transport,
        CloudLinkTransportRoute::TelemetryUp,
        out_of_order,
        replayed.identity(),
    )
    .await;
    assert!(
        !wait_for_durable_ack(
            &resumed_transport,
            None,
            &resumed_session,
            Duration::from_secs(1),
        )
        .await,
        "an unresolved position gap must not advance the cumulative cursor"
    );

    let partial_spool = MemoryCloudLinkSpool::new("partial", 4).expect("partial spool");
    let partial_payload = CloudLinkCodec::telemetry_batch(
        TopologyBinding::new(11, "fx64:0123456789abcdef").expect("topology"),
        &[sample, sample],
    )
    .expect("two-sample payload");
    let partial_record = partial_spool
        .enqueue(
            CloudLinkCodec::prepare(
                CloudLinkMessageKind::TelemetryBatch,
                "batch-partial",
                &partial_payload,
                TimestampMs::new(1_721_000_000_123),
                None,
            )
            .expect("partial sealed"),
        )
        .await
        .expect("partial enqueue");
    send_record_without_spool_transition(
        &resumed_transport,
        &resumed_session,
        &partial_record,
        CloudLinkTransportRoute::TelemetryUp,
    )
    .await;
    assert!(
        !wait_for_durable_ack(
            &resumed_transport,
            None,
            &resumed_session,
            Duration::from_secs(1),
        )
        .await,
        "an unsupported multi-sample batch must fail atomically"
    );

    let loss_spool = MemoryCloudLinkSpool::new("loss", 4).expect("loss spool");
    let loss = aether_ports::CloudLinkDataLossEvidence::new(
        "telemetry",
        1,
        2,
        4,
        5,
        "capacity-overflow",
        TimestampMs::new(1_721_000_000_300),
    );
    let loss_payload = aether_cloudlink::DataLossPayload::from_evidence(&loss);
    let loss_record = loss_spool
        .enqueue(
            CloudLinkCodec::prepare(
                CloudLinkMessageKind::DataLoss,
                "loss-dual",
                &loss_payload,
                TimestampMs::new(1_721_000_000_300),
                None,
            )
            .expect("loss sealed"),
        )
        .await
        .expect("loss enqueue");
    offer(
        &resumed_transport,
        &loss_spool,
        &resumed_session,
        &loss_record,
        CloudLinkTransportRoute::DataLossUp,
    )
    .await;
    assert!(
        wait_for_durable_ack(
            &resumed_transport,
            Some(&loss_spool),
            &resumed_session,
            WAIT,
        )
        .await,
        "explicit data-loss evidence requires an application ACK"
    );

    let final_status = telemetry_spool.status().await.expect("final Edge status");
    let evidence = json!({
        "component": "AetherEdge",
        "edge_transport": "aether-cloudlink-mqtt/rumqttc",
        "spool": "FileCloudLinkSpool",
        "sessions": 2,
        "heartbeat_acks": 1,
        "manifest_acks": 1,
        "telemetry_application_acks": 2,
        "ack_loss_replays": 1,
        "duplicate_replays": 1,
        "conflicts_without_ack": 1,
        "expired_without_ack": 1,
        "out_of_order_without_ack": 1,
        "partial_batches_without_ack": 1,
        "data_loss_acks": 1,
        "final_cursor": final_status.last_acknowledged_position(),
        "pending_records": final_status.pending_records(),
        "mqtt_puback_deletes_spool": false,
        "physical_control": false,
        "edge_safety_authority_preserved": true
    });
    std::fs::write(
        evidence_path,
        serde_json::to_vec_pretty(&evidence).expect("edge evidence JSON"),
    )
    .expect("write Edge evidence");
    println!("AETHER_CLOUDLINK_EDGE_EVIDENCE={evidence}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn external_cloud_dual_phase1_before_edge_restart() {
    if env::var("AETHER_CLOUDLINK_EDGE_PHASE").as_deref() != Ok("before-restart") {
        return;
    }

    let gateway_id = env::var("AETHER_CLOUDLINK_GATEWAY_ID").expect("dual Gateway ID");
    let topic_prefix = env::var("AETHER_CLOUDLINK_TOPIC_PREFIX").expect("dual topic prefix");
    let spool_root = env::var("AETHER_CLOUDLINK_SPOOL_ROOT").expect("dual spool root");
    let evidence_path = env::var("AETHER_CLOUDLINK_EDGE_EVIDENCE").expect("edge evidence path");
    std::fs::create_dir_all(&spool_root).expect("create spool root");
    let manifest_path = Path::new(&spool_root).join("manifest.spool");
    let telemetry_path = Path::new(&spool_root).join("telemetry.spool");
    let topics = TopicNamespace::new(&topic_prefix, &gateway_id).expect("dual topic namespace");
    let config = integration_config(&gateway_id);
    config
        .validate(DeploymentSecurity::Development)
        .expect("dual development configuration");
    let manifest_spool =
        FileCloudLinkSpool::open(&manifest_path, "manifest", 8).expect("manifest spool");
    let telemetry_spool =
        FileCloudLinkSpool::open(&telemetry_path, "telemetry", 8).expect("telemetry spool");

    let transport =
        MqttCloudLinkTransport::connect(config, topics, DeploymentSecurity::Development)
            .expect("first Edge transport");
    wait_connected(&transport).await;
    send_hello_with_nonce(&transport, &gateway_id, 0, 1).await;
    let session = wait_session(&transport, &gateway_id, 0).await;

    let heartbeat = CloudLinkCodec::encode(
        &aether_cloudlink::HeartbeatMessage::new(
            &session,
            TimestampMs::new(1_721_000_000_123),
            Vec::new(),
            &test_uplink_authentication(),
        )
        .expect("heartbeat"),
    )
    .expect("heartbeat JSON");
    transport
        .send(CloudLinkTransportMessage::new(
            CloudLinkTransportRoute::HeartbeatUp,
            heartbeat,
            None,
        ))
        .await
        .expect("heartbeat send");
    wait_heartbeat_ack(&transport, &session).await;

    let runtime_manifest = std::fs::read(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../config.template/runtime-manifest.json"),
    )
    .expect("runtime manifest");
    let manifest_payload = CloudLinkCodec::runtime_manifest_report(
        &runtime_manifest,
        TimestampMs::new(1_721_000_000_123),
    )
    .expect("manifest report");
    let manifest_record = manifest_spool
        .enqueue(
            CloudLinkCodec::prepare(
                CloudLinkMessageKind::RuntimeManifestReport,
                "manifest-dual",
                &manifest_payload,
                TimestampMs::new(1_721_000_000_123),
                None,
            )
            .expect("sealed manifest"),
        )
        .await
        .expect("manifest enqueue");
    offer(
        &transport,
        &manifest_spool,
        &session,
        &manifest_record,
        CloudLinkTransportRoute::ManifestUp,
    )
    .await;
    assert!(
        wait_for_durable_ack(&transport, Some(&manifest_spool), &session, WAIT).await,
        "manifest requires an application ACK"
    );

    request_harness_control("broker-restart").await;
    wait_for_broker_reconnect(&transport).await;
    tokio::time::sleep(Duration::from_secs(1)).await;

    let sample = PointSample::new(
        PointAddress::new(InstanceId::new(42), PointKind::Telemetry, PointId::new(8)),
        12.5,
        TimestampMs::new(1_721_000_000_123),
        PointQuality::Uncertain,
    );
    let payload = CloudLinkCodec::telemetry_batch(
        TopologyBinding::new(11, "fx64:0123456789abcdef").expect("topology"),
        &[sample],
    )
    .expect("telemetry payload");
    let record = telemetry_spool
        .enqueue(
            CloudLinkCodec::prepare(
                CloudLinkMessageKind::TelemetryBatch,
                "telemetry-ack-loss",
                &payload,
                TimestampMs::new(1_721_000_000_123),
                Some(TimestampMs::new(1_721_003_600_000)),
            )
            .expect("sealed telemetry"),
        )
        .await
        .expect("telemetry enqueue");
    offer(
        &transport,
        &telemetry_spool,
        &session,
        &record,
        CloudLinkTransportRoute::TelemetryUp,
    )
    .await;
    assert!(
        !wait_for_durable_ack(
            &transport,
            Some(&telemetry_spool),
            &session,
            Duration::from_secs(2),
        )
        .await,
        "injected ACK loss must leave the Edge record pending"
    );
    let status = telemetry_spool.status().await.expect("phase-one status");
    assert_eq!(status.pending_records(), 1);
    assert_eq!(status.last_acknowledged_position(), 0);

    let evidence = json!({
        "phase": "before-edge-process-restart",
        "session_epoch": session.session_epoch(),
        "heartbeat_acks": 1,
        "manifest_acks": 1,
        "broker_restarts": 1,
        "mqtt_puback_observed_without_spool_delete": true,
        "pending_records": status.pending_records(),
        "final_cursor": status.last_acknowledged_position(),
        "physical_control": false
    });
    std::fs::write(
        evidence_path,
        serde_json::to_vec_pretty(&evidence).expect("phase-one evidence JSON"),
    )
    .expect("write phase-one evidence");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn external_cloud_dual_phase2_after_edge_restart() {
    if env::var("AETHER_CLOUDLINK_EDGE_PHASE").as_deref() != Ok("after-restart") {
        return;
    }

    let gateway_id = env::var("AETHER_CLOUDLINK_GATEWAY_ID").expect("dual Gateway ID");
    let topic_prefix = env::var("AETHER_CLOUDLINK_TOPIC_PREFIX").expect("dual topic prefix");
    let spool_root = env::var("AETHER_CLOUDLINK_SPOOL_ROOT").expect("dual spool root");
    let evidence_path = env::var("AETHER_CLOUDLINK_EDGE_EVIDENCE").expect("edge evidence path");
    let telemetry_path = Path::new(&spool_root).join("telemetry.spool");
    let topics = TopicNamespace::new(&topic_prefix, &gateway_id).expect("dual topic namespace");
    let config = integration_config(&gateway_id);
    config
        .validate(DeploymentSecurity::Development)
        .expect("dual development configuration");
    let telemetry_spool =
        FileCloudLinkSpool::open(&telemetry_path, "telemetry", 8).expect("recovered Edge spool");
    assert_eq!(
        telemetry_spool
            .status()
            .await
            .expect("recovered status")
            .pending_records(),
        1,
        "the application-unacknowledged fact must survive the Edge process restart"
    );

    let transport =
        MqttCloudLinkTransport::connect(config, topics, DeploymentSecurity::Development)
            .expect("restarted Edge transport");
    wait_connected(&transport).await;
    send_hello_with_nonce(&transport, &gateway_id, 0, 2).await;
    let (session, resume_cursors) = wait_session_with_resume(&transport, &gateway_id, 1).await;
    assert!(
        resume_cursors.iter().any(|cursor| {
            cursor.stream_id() == "manifest"
                && cursor.stream_epoch() == 1
                && cursor.acknowledged_position() == 1
        }),
        "Cloud must return its durable manifest/1/1 resume cursor"
    );

    let replayed = telemetry_spool
        .replay_from(1, 1)
        .await
        .expect("replay after Edge process restart")
        .records()[0]
        .clone();
    offer(
        &transport,
        &telemetry_spool,
        &session,
        &replayed,
        CloudLinkTransportRoute::TelemetryUp,
    )
    .await;
    assert!(
        wait_for_durable_ack(&transport, Some(&telemetry_spool), &session, WAIT).await,
        "stable replay must receive the previously lost application ACK"
    );
    assert_eq!(
        telemetry_spool
            .status()
            .await
            .expect("acknowledged status")
            .pending_records(),
        0
    );

    send_record_without_spool_transition(
        &transport,
        &session,
        &replayed,
        CloudLinkTransportRoute::TelemetryUp,
    )
    .await;
    assert!(
        wait_for_durable_ack(&transport, Some(&telemetry_spool), &session, WAIT).await,
        "exact duplicate QoS 1 delivery must receive the same logical ACK"
    );

    let conflicting = contextual_fixture(
        "conflicting-replay.valid-digest.json",
        &gateway_id,
        &session,
        replayed.identity(),
        "batch-conflict",
        None,
    );
    send_raw_delivery(
        &transport,
        CloudLinkTransportRoute::TelemetryUp,
        conflicting,
        replayed.identity(),
    )
    .await;
    assert!(
        !wait_for_durable_ack(&transport, None, &session, Duration::from_secs(1)).await,
        "digest conflict must not receive an ACK"
    );

    let expired = contextual_fixture(
        "telemetry-batch.valid.json",
        &gateway_id,
        &session,
        replayed.identity(),
        "batch-expired",
        Some(("2", "1721000000400")),
    );
    send_raw_delivery(
        &transport,
        CloudLinkTransportRoute::TelemetryUp,
        expired,
        replayed.identity(),
    )
    .await;
    assert!(
        !wait_for_durable_ack(&transport, None, &session, Duration::from_secs(1)).await,
        "expired fact must not receive an ACK"
    );

    let out_of_order = contextual_fixture(
        "telemetry-batch.valid.json",
        &gateway_id,
        &session,
        replayed.identity(),
        "batch-out-of-order",
        Some(("3", "1721003600000")),
    );
    send_raw_delivery(
        &transport,
        CloudLinkTransportRoute::TelemetryUp,
        out_of_order,
        replayed.identity(),
    )
    .await;
    assert!(
        !wait_for_durable_ack(&transport, None, &session, Duration::from_secs(1)).await,
        "unresolved ordering gap must not receive a cumulative ACK"
    );

    let sample = PointSample::new(
        PointAddress::new(InstanceId::new(42), PointKind::Telemetry, PointId::new(9)),
        13.5,
        TimestampMs::new(1_721_000_000_123),
        PointQuality::Good,
    );
    let partial_spool = MemoryCloudLinkSpool::new("partial", 4).expect("partial spool");
    let partial_payload = CloudLinkCodec::telemetry_batch(
        TopologyBinding::new(11, "fx64:0123456789abcdef").expect("topology"),
        &[sample],
    )
    .expect("partial payload");
    let partial_record = partial_spool
        .enqueue(
            CloudLinkCodec::prepare(
                CloudLinkMessageKind::TelemetryBatch,
                "partial-success",
                &partial_payload,
                TimestampMs::new(1_721_000_000_123),
                None,
            )
            .expect("partial sealed"),
        )
        .await
        .expect("partial enqueue");
    send_record_without_spool_transition(
        &transport,
        &session,
        &partial_record,
        CloudLinkTransportRoute::TelemetryUp,
    )
    .await;
    assert!(
        !wait_for_durable_ack(&transport, None, &session, Duration::from_secs(1)).await,
        "non-durable partial application outcome must not receive an ACK"
    );

    let loss_spool = MemoryCloudLinkSpool::new("loss", 4).expect("loss spool");
    let loss = aether_ports::CloudLinkDataLossEvidence::new(
        "telemetry",
        1,
        2,
        4,
        5,
        "capacity-overflow",
        TimestampMs::new(1_721_000_000_300),
    );
    let loss_payload = aether_cloudlink::DataLossPayload::from_evidence(&loss);
    let loss_record = loss_spool
        .enqueue(
            CloudLinkCodec::prepare(
                CloudLinkMessageKind::DataLoss,
                "loss-dual",
                &loss_payload,
                TimestampMs::new(1_721_000_000_300),
                None,
            )
            .expect("loss sealed"),
        )
        .await
        .expect("loss enqueue");
    offer(
        &transport,
        &loss_spool,
        &session,
        &loss_record,
        CloudLinkTransportRoute::DataLossUp,
    )
    .await;
    assert!(
        wait_for_durable_ack(&transport, Some(&loss_spool), &session, WAIT).await,
        "explicit data-loss fact requires an application ACK"
    );

    request_harness_control("cloud-restart").await;
    send_hello_with_nonce(&transport, &gateway_id, 1, 3).await;
    let (post_crash_session, epoch_rollback_detected) =
        wait_session_allowing_cloud_state_loss(&transport, &gateway_id, session.session_epoch())
            .await;
    assert!(
        epoch_rollback_detected,
        "process-local Cloud state loss must be surfaced as unknown, not durable continuity"
    );
    send_record_without_spool_transition(
        &transport,
        &post_crash_session,
        &replayed,
        CloudLinkTransportRoute::TelemetryUp,
    )
    .await;
    assert!(
        wait_for_durable_ack(&transport, None, &post_crash_session, WAIT).await,
        "fresh in-memory Cloud process honestly re-accepts an unknown replay"
    );

    let final_status = telemetry_spool.status().await.expect("final Edge status");
    let evidence = json!({
        "phase": "after-edge-process-restart",
        "edge_transport": "aether-cloudlink-mqtt/rumqttc",
        "spool": "FileCloudLinkSpool",
        "edge_process_restarts": 1,
        "cloud_process_restarts": 1,
        "sessions": 3,
        "telemetry_application_acks": 3,
        "ack_loss_replays": 1,
        "mqtt_duplicate_deliveries": 1,
        "same_position_same_digest_idempotent": true,
        "conflicts_without_ack": 1,
        "expired_without_ack": 1,
        "out_of_order_without_ack": 1,
        "partial_success_without_ack": 1,
        "data_loss_acks": 1,
        "cloud_restart_epoch_rollback_detected": epoch_rollback_detected,
        "cloud_restart_durability": "unknown-reaccepted",
        "final_cursor": final_status.last_acknowledged_position(),
        "pending_records": final_status.pending_records(),
        "mqtt_puback_deletes_spool": false,
        "physical_control": false,
        "edge_safety_authority_preserved": true
    });
    std::fs::write(
        evidence_path,
        serde_json::to_vec_pretty(&evidence).expect("edge evidence JSON"),
    )
    .expect("write Edge evidence");
    println!("AETHER_CLOUDLINK_EDGE_EVIDENCE={evidence}");
}

async fn request_harness_control(action: &str) {
    let root = env::var("AETHER_CLOUDLINK_CONTROL_DIR").expect("harness control directory");
    let request = Path::new(&root).join(format!("{action}.request"));
    let completed = Path::new(&root).join(format!("{action}.done"));
    let _ = std::fs::remove_file(&completed);
    std::fs::write(&request, b"requested\n").expect("write harness control request");
    let deadline = Instant::now() + Duration::from_secs(30);
    while !completed.is_file() {
        assert!(
            Instant::now() < deadline,
            "harness control {action} timed out"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn wait_session_allowing_cloud_state_loss(
    transport: &Arc<MqttCloudLinkTransport>,
    gateway_id: &str,
    previous_epoch: u64,
) -> (SessionBinding, bool) {
    loop {
        let event = timeout(WAIT, transport.receive())
            .await
            .expect("post-restart session timeout")
            .expect("post-restart session transport event");
        if let CloudLinkTransportEvent::Inbound(message) = event
            && message.route() == CloudLinkTransportRoute::SessionDown
            && let CandidateMessage::SessionAccepted(accepted) =
                CloudLinkCodec::decode(message.payload()).expect("accepted JSON")
        {
            let rollback = accepted
                .bind(
                    gateway_id,
                    TEST_CREDENTIAL_GENERATION,
                    &["1.0"],
                    previous_epoch,
                )
                .is_err();
            let binding = accepted
                .bind(gateway_id, TEST_CREDENTIAL_GENERATION, &["1.0"], 0)
                .expect("fresh Cloud process session binding");
            return (binding, rollback);
        }
    }
}

fn contextual_fixture(
    name: &str,
    gateway_id: &str,
    session: &SessionBinding,
    identity: &aether_ports::CloudLinkRecordIdentity,
    batch_id: &str,
    position_and_expiry: Option<(&str, &str)>,
) -> Value {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../contracts/cloudlink/v1/fixtures")
        .join(name);
    let mut value: Value =
        serde_json::from_slice(&std::fs::read(path).expect("dual fixture")).expect("fixture JSON");
    value["gateway_id"] = json!(gateway_id);
    value["session_id"] = json!(session.session_id());
    value["session_epoch"] = json!(session.session_epoch().to_string());
    value["credential_generation"] = json!(session.credential_generation().to_string());
    value["delivery"]["stream_id"] = json!(identity.stream_id());
    value["delivery"]["stream_epoch"] = json!(identity.stream_epoch().to_string());
    value["delivery"]["position"] = json!(identity.position().to_string());
    value["delivery"]["batch_id"] = json!(batch_id);
    if let Some((position, expires_at)) = position_and_expiry {
        value["delivery"]["position"] = json!(position);
        value["expires_at_ms"] = json!(expires_at);
    }
    value
}

async fn send_raw_delivery(
    transport: &Arc<MqttCloudLinkTransport>,
    route: CloudLinkTransportRoute,
    value: Value,
    identity: &aether_ports::CloudLinkRecordIdentity,
) {
    transport
        .send(CloudLinkTransportMessage::new(
            route,
            serde_json::to_vec(&value).expect("contextual delivery JSON"),
            Some(identity.clone()),
        ))
        .await
        .expect("contextual delivery send");
}

async fn send_record_without_spool_transition(
    transport: &Arc<MqttCloudLinkTransport>,
    session: &SessionBinding,
    record: &aether_ports::CloudLinkRecord,
    route: CloudLinkTransportRoute,
) {
    let envelope =
        CloudLinkCodec::delivery_envelope(session, record, None, &test_uplink_authentication())
            .expect("direct delivery envelope");
    transport
        .send(CloudLinkTransportMessage::new(
            route,
            CloudLinkCodec::encode(&envelope).expect("direct envelope JSON"),
            Some(record.identity().clone()),
        ))
        .await
        .expect("direct record send");
}

async fn wait_for_broker_reconnect(transport: &Arc<MqttCloudLinkTransport>) {
    let deadline = Instant::now() + WAIT;
    let mut disconnected = false;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let event = timeout(remaining, transport.receive())
            .await
            .expect("Broker reconnect timeout")
            .expect("Broker reconnect event");
        match event {
            CloudLinkTransportEvent::Disconnected => disconnected = true,
            CloudLinkTransportEvent::Connected if disconnected => return,
            _ => {},
        }
    }
}

async fn wait_for_durable_ack(
    transport: &Arc<MqttCloudLinkTransport>,
    spool: Option<&dyn CloudLinkSpool>,
    session: &SessionBinding,
    maximum_wait: Duration,
) -> bool {
    let deadline = Instant::now() + maximum_wait;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return false;
        }
        let event = match timeout(remaining, transport.receive()).await {
            Ok(Ok(event)) => event,
            Ok(Err(error)) => panic!("durable ACK transport failure: {error}"),
            Err(_) => return false,
        };
        match event {
            CloudLinkTransportEvent::TransportPublished(identity) => {
                if let Some(spool) = spool {
                    let _ = spool
                        .mark_transport_published(&identity, &session.spool_binding())
                        .await;
                }
            },
            CloudLinkTransportEvent::Inbound(message)
                if message.route() == CloudLinkTransportRoute::AckDown =>
            {
                if let CandidateMessage::DurableAck(ack) =
                    CloudLinkCodec::decode(message.payload()).expect("durable ACK JSON")
                {
                    if let Some(spool) = spool {
                        spool
                            .acknowledge(&ack.to_spool_ack(session).expect("ACK session"))
                            .await
                            .expect("application ACK");
                    }
                    return true;
                }
            },
            _ => {},
        }
    }
}

fn integration_config(gateway_id: &str) -> CloudLinkMqttConfig {
    let host = env::var("AETHER_CLOUDLINK_BROKER_HOST").unwrap_or_else(|_| "127.0.0.1".to_string());
    let port = env::var("AETHER_CLOUDLINK_BROKER_PORT")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(1883);
    let mut config = CloudLinkMqttConfig::development(host, port, gateway_id);
    config.username = env::var("AETHER_CLOUDLINK_BROKER_USERNAME").ok();
    config.password = env::var("AETHER_CLOUDLINK_BROKER_PASSWORD")
        .ok()
        .map(SecretString::new);
    config.tls = if let Ok(ca_path) = env::var("AETHER_CLOUDLINK_BROKER_CA") {
        let certificate = env::var("AETHER_CLOUDLINK_BROKER_CLIENT_CERT").ok();
        let private_key = env::var("AETHER_CLOUDLINK_BROKER_CLIENT_KEY").ok();
        let client_identity = match (certificate, private_key) {
            (Some(certificate), Some(private_key)) => Some(MqttClientIdentity {
                certificate_path: certificate.into(),
                private_key_path: private_key.into(),
            }),
            _ => None,
        };
        CloudLinkTlsConfig::Custom {
            ca_path: ca_path.into(),
            client_identity,
        }
    } else if env::var("AETHER_CLOUDLINK_BROKER_TLS").as_deref() == Ok("1") {
        CloudLinkTlsConfig::SystemRoots
    } else {
        CloudLinkTlsConfig::Disabled
    };
    config
}

async fn fake_cloud(
    config: CloudLinkMqttConfig,
    topics: TopicNamespace,
    gateway_id: String,
    observations: mpsc::Sender<FakeCloudObservation>,
) {
    let (client, mut event_loop) = fake_cloud_client(&config);
    let mut sessions = 0_u64;
    let mut telemetry_identity = None::<(String, String, String, String, String)>;
    let mut duplicate_facts = BTreeSet::new();
    loop {
        match event_loop.poll().await {
            Ok(Event::Incoming(Incoming::ConnAck(_))) => {
                for topic in topics.publish_topics() {
                    if client.subscribe(topic, CLOUDLINK_MQTT_QOS).await.is_err() {
                        let _ = observations
                            .send(FakeCloudObservation::Failure(
                                "fake cloud could not subscribe".to_string(),
                            ))
                            .await;
                        return;
                    }
                }
                let _ = observations.send(FakeCloudObservation::Ready).await;
            },
            Ok(Event::Incoming(Incoming::Publish(publication))) => {
                let Some(route) = outbound_route(&topics, &publication.topic) else {
                    continue;
                };
                let value: Value = match serde_json::from_slice(&publication.payload) {
                    Ok(value) => value,
                    Err(error) => {
                        let _ = observations
                            .send(FakeCloudObservation::Failure(format!(
                                "fake cloud received invalid JSON: {error}"
                            )))
                            .await;
                        return;
                    },
                };
                match route {
                    CloudLinkTransportRoute::SessionUp => {
                        sessions += 1;
                        let accepted = json!({
                            "schema": "aether.cloudlink.session-accepted.v1",
                            "protocol": "aether.cloudlink",
                            "message_kind": "session-accepted",
                            "gateway_id": gateway_id,
                            "selected_protocol_version": "1.0",
                            "session_id": if sessions == 1 {
                                "44444444-4444-4444-8444-444444444444"
                            } else {
                                "55555555-5555-4555-8555-555555555555"
                            },
                            "session_epoch": sessions.to_string(),
                            "credential_generation": "1",
                            "server_time_ms": "1721000000123",
                            "heartbeat_interval_ms": "30000",
                            "resume": [{
                                "stream_id": "business",
                                "stream_epoch": "1",
                                "acknowledged_position": if sessions == 1 { "0" } else { "1" }
                            }]
                        });
                        publish_json(
                            &client,
                            topics.topic(CloudLinkTransportRoute::SessionDown),
                            accepted,
                        )
                        .await;
                        if sessions == 2 {
                            let replay = json!({
                                "schema": "aether.cloudlink.replay-request.v1",
                                "protocol": "aether.cloudlink",
                                "protocol_version": "1.0",
                                "message_kind": "replay-request",
                                "gateway_id": gateway_id,
                                "session_id": "55555555-5555-4555-8555-555555555555",
                                "session_epoch": "2",
                                "credential_generation": "1",
                                "stream_id": "business",
                                "stream_epoch": "1",
                                "from_position": "2",
                                "requested_at_ms": "1721000000500"
                            });
                            publish_json(
                                &client,
                                topics.topic(CloudLinkTransportRoute::ReplayDown),
                                replay,
                            )
                            .await;
                        }
                    },
                    CloudLinkTransportRoute::HeartbeatUp => {
                        let mut response = value;
                        response["message_kind"] = json!("heartbeat-ack");
                        publish_json(
                            &client,
                            topics.topic(CloudLinkTransportRoute::AckDown),
                            response,
                        )
                        .await;
                    },
                    CloudLinkTransportRoute::ManifestUp => {
                        publish_ack(&client, &topics, &value).await;
                    },
                    CloudLinkTransportRoute::TelemetryUp => {
                        let delivery = &value["delivery"];
                        let identity = (
                            delivery["stream_id"]
                                .as_str()
                                .unwrap_or_default()
                                .to_string(),
                            delivery["stream_epoch"]
                                .as_str()
                                .unwrap_or_default()
                                .to_string(),
                            delivery["position"]
                                .as_str()
                                .unwrap_or_default()
                                .to_string(),
                            delivery["batch_id"]
                                .as_str()
                                .unwrap_or_default()
                                .to_string(),
                            delivery["digest"].as_str().unwrap_or_default().to_string(),
                        );
                        let fact_identity = format!(
                            "{}:{}:{}:{}",
                            identity.0, identity.1, identity.2, identity.3
                        );
                        if !duplicate_facts.insert(fact_identity)
                            && telemetry_identity.as_ref() != Some(&identity)
                        {
                            let _ = observations
                                .send(FakeCloudObservation::Failure(
                                    "equal identity arrived with a conflicting digest".to_string(),
                                ))
                                .await;
                            return;
                        }
                        if let Some(first) = &telemetry_identity {
                            if first != &identity {
                                let _ = observations
                                    .send(FakeCloudObservation::Failure(
                                        "replay allocated a different identity or digest"
                                            .to_string(),
                                    ))
                                    .await;
                                return;
                            }
                            let _ = observations
                                .send(FakeCloudObservation::ReplayedTelemetry)
                                .await;
                            publish_ack(&client, &topics, &value).await;
                        } else {
                            telemetry_identity = Some(identity);
                            let _ = observations
                                .send(FakeCloudObservation::FirstTelemetry)
                                .await;
                        }
                    },
                    _ => {},
                }
            },
            Ok(_) => {},
            Err(error) => {
                let _ = observations
                    .send(FakeCloudObservation::Failure(format!(
                        "fake cloud MQTT failure: {error}"
                    )))
                    .await;
                return;
            },
        }
    }
}

fn fake_cloud_client(config: &CloudLinkMqttConfig) -> (AsyncClient, rumqttc::EventLoop) {
    let mut options = MqttOptions::new(
        format!("{}-fake-cloud", config.client_id),
        &config.broker_host,
        config.broker_port,
    );
    options.set_keep_alive(Duration::from_secs(config.keep_alive_secs));
    options.set_clean_session(true);
    options.set_max_packet_size(config.maximum_packet_bytes, config.maximum_packet_bytes);
    if let Some(username) = &config.username {
        let password = env::var("AETHER_CLOUDLINK_BROKER_PASSWORD").unwrap_or_default();
        options.set_credentials(username, password);
    }
    match &config.tls {
        CloudLinkTlsConfig::Disabled => {},
        CloudLinkTlsConfig::SystemRoots => {
            options.set_transport(Transport::tls_with_config(
                rumqttc::TlsConfiguration::Native,
            ));
        },
        CloudLinkTlsConfig::Custom {
            ca_path,
            client_identity,
        } => {
            let ca = Certificate::from_pem(&std::fs::read(ca_path).expect("integration CA"))
                .expect("parse integration CA");
            let mut connector = TlsConnector::builder();
            connector.add_root_certificate(ca);
            if let Some(identity) = client_identity {
                connector.identity(
                    Identity::from_pkcs8(
                        &std::fs::read(&identity.certificate_path)
                            .expect("integration client certificate"),
                        &std::fs::read(&identity.private_key_path)
                            .expect("integration client private key"),
                    )
                    .expect("parse integration client identity"),
                );
            }
            options.set_transport(Transport::tls_with_config(
                rumqttc::TlsConfiguration::NativeConnector(
                    connector.build().expect("integration TLS connector"),
                ),
            ));
        },
    }
    AsyncClient::new(options, config.request_capacity)
}

async fn publish_json(client: &AsyncClient, topic: String, value: Value) {
    client
        .publish(
            topic,
            CLOUDLINK_MQTT_QOS,
            CLOUDLINK_MQTT_RETAIN,
            serde_json::to_vec(&value).expect("fake cloud JSON"),
        )
        .await
        .expect("fake cloud publish");
}

async fn publish_ack(client: &AsyncClient, topics: &TopicNamespace, envelope: &Value) {
    let ack = json!({
        "schema": "aether.cloudlink.durable-ack.v1",
        "protocol": "aether.cloudlink",
        "protocol_version": "1.0",
        "message_kind": "durable-ack",
        "gateway_id": envelope["gateway_id"],
        "session_id": envelope["session_id"],
        "session_epoch": envelope["session_epoch"],
        "credential_generation": envelope["credential_generation"],
        "stream_id": envelope["delivery"]["stream_id"],
        "stream_epoch": envelope["delivery"]["stream_epoch"],
        "acknowledged_position": envelope["delivery"]["position"],
        "batch_id": envelope["delivery"]["batch_id"],
        "digest": envelope["delivery"]["digest"],
        "receipt_id": format!("receipt-{}", envelope["delivery"]["position"].as_str().unwrap_or("unknown")),
        "acknowledged_at_ms": "1721000000600"
    });
    publish_json(client, topics.topic(CloudLinkTransportRoute::AckDown), ack).await;
}

fn outbound_route(topics: &TopicNamespace, topic: &str) -> Option<CloudLinkTransportRoute> {
    [
        CloudLinkTransportRoute::SessionUp,
        CloudLinkTransportRoute::HeartbeatUp,
        CloudLinkTransportRoute::ManifestUp,
        CloudLinkTransportRoute::TelemetryUp,
        CloudLinkTransportRoute::DataLossUp,
    ]
    .into_iter()
    .find(|route| topics.topic(*route) == topic)
}

async fn wait_connected(transport: &Arc<MqttCloudLinkTransport>) {
    loop {
        let event = timeout(WAIT, transport.receive())
            .await
            .expect("MQTT connection timeout")
            .expect("MQTT connection event");
        if event == CloudLinkTransportEvent::Connected {
            return;
        }
    }
}

async fn send_hello(
    transport: &Arc<MqttCloudLinkTransport>,
    gateway_id: &str,
    acknowledged_position: u64,
) {
    send_hello_with_nonce(
        transport,
        gateway_id,
        acknowledged_position,
        acknowledged_position,
    )
    .await;
}

async fn send_hello_with_nonce(
    transport: &Arc<MqttCloudLinkTransport>,
    gateway_id: &str,
    acknowledged_position: u64,
    nonce_marker: u64,
) {
    let hello = SessionHello::new_gateway_signed(
        gateway_id,
        "development-integration-binding",
        TEST_CREDENTIAL_GENERATION,
        "22222222-2222-4222-8222-222222222222",
        "development-integration-key",
        MessageAuthentication::new("development-integration-key", "E".repeat(86))
            .expect("signature shape"),
        vec!["1.0".to_string()],
        format!("{nonce_marker:0>43}"),
        vec![
            aether_cloudlink::ResumeCursor::new("business", 1, acknowledged_position)
                .expect("resume cursor"),
        ],
    )
    .expect("session hello");
    transport
        .send(CloudLinkTransportMessage::new(
            CloudLinkTransportRoute::SessionUp,
            CloudLinkCodec::encode(&hello).expect("hello JSON"),
            None,
        ))
        .await
        .expect("send hello");
}

async fn wait_session(
    transport: &Arc<MqttCloudLinkTransport>,
    gateway_id: &str,
    previous_epoch: u64,
) -> SessionBinding {
    wait_session_with_resume(transport, gateway_id, previous_epoch)
        .await
        .0
}

async fn wait_session_with_resume(
    transport: &Arc<MqttCloudLinkTransport>,
    gateway_id: &str,
    previous_epoch: u64,
) -> (SessionBinding, Vec<aether_cloudlink::ResumeCursor>) {
    loop {
        let event = timeout(WAIT, transport.receive())
            .await
            .expect("session timeout")
            .expect("session transport event");
        if let CloudLinkTransportEvent::Inbound(message) = event
            && message.route() == CloudLinkTransportRoute::SessionDown
            && let CandidateMessage::SessionAccepted(accepted) =
                CloudLinkCodec::decode(message.payload()).expect("accepted JSON")
        {
            let resume_cursors = accepted.resume_cursors().to_vec();
            let binding = accepted
                .bind(
                    gateway_id,
                    TEST_CREDENTIAL_GENERATION,
                    &["1.0"],
                    previous_epoch,
                )
                .expect("accepted session binding");
            return (binding, resume_cursors);
        }
    }
}

async fn wait_heartbeat_ack(transport: &Arc<MqttCloudLinkTransport>, session: &SessionBinding) {
    loop {
        let event = timeout(WAIT, transport.receive())
            .await
            .expect("heartbeat timeout")
            .expect("heartbeat event");
        if let CloudLinkTransportEvent::Inbound(message) = event
            && message.route() == CloudLinkTransportRoute::AckDown
        {
            let decoded = CloudLinkCodec::decode(message.payload()).expect("heartbeat ACK JSON");
            if matches!(decoded, CandidateMessage::Heartbeat(_)) {
                decoded
                    .validate_session(session)
                    .expect("heartbeat session");
                return;
            }
        }
    }
}

async fn offer(
    transport: &Arc<MqttCloudLinkTransport>,
    spool: &dyn CloudLinkSpool,
    session: &SessionBinding,
    record: &aether_ports::CloudLinkRecord,
    route: CloudLinkTransportRoute,
) {
    let spool_session = session.spool_binding();
    spool
        .mark_offered(record.identity(), &spool_session)
        .await
        .expect("mark offered");
    let envelope =
        CloudLinkCodec::delivery_envelope(session, record, None, &test_uplink_authentication())
            .expect("delivery envelope");
    transport
        .send(CloudLinkTransportMessage::new(
            route,
            CloudLinkCodec::encode(&envelope).expect("envelope JSON"),
            Some(record.identity().clone()),
        ))
        .await
        .expect("transport offer");
}

async fn wait_durable_ack(
    transport: &Arc<MqttCloudLinkTransport>,
    spool: &dyn CloudLinkSpool,
    session: &SessionBinding,
) {
    loop {
        let event = timeout(WAIT, transport.receive())
            .await
            .expect("durable ACK timeout")
            .expect("durable ACK event");
        match event {
            CloudLinkTransportEvent::TransportPublished(identity) => {
                spool
                    .mark_transport_published(&identity, &session.spool_binding())
                    .await
                    .expect("transport PUBACK state");
            },
            CloudLinkTransportEvent::Inbound(message)
                if message.route() == CloudLinkTransportRoute::AckDown =>
            {
                if let CandidateMessage::DurableAck(ack) =
                    CloudLinkCodec::decode(message.payload()).expect("durable ACK JSON")
                {
                    spool
                        .acknowledge(&ack.to_spool_ack(session).expect("ACK session"))
                        .await
                        .expect("application ACK");
                    return;
                }
            },
            _ => {},
        }
    }
}

async fn wait_replay_request(
    transport: &Arc<MqttCloudLinkTransport>,
    session: &SessionBinding,
    record: &aether_ports::CloudLinkRecord,
) {
    loop {
        let event = timeout(WAIT, transport.receive())
            .await
            .expect("replay request timeout")
            .expect("replay request event");
        if let CloudLinkTransportEvent::Inbound(message) = event
            && message.route() == CloudLinkTransportRoute::ReplayDown
            && let CandidateMessage::ReplayRequest(request) =
                CloudLinkCodec::decode(message.payload()).expect("replay JSON")
        {
            request
                .validate_session(session)
                .expect("replay current session");
            assert_eq!(request.stream_id(), record.identity().stream_id());
            assert_eq!(request.stream_epoch(), record.identity().stream_epoch());
            assert_eq!(request.from_position(), record.identity().position());
            return;
        }
    }
}

async fn expect_cloud(
    observations: &mut mpsc::Receiver<FakeCloudObservation>,
    predicate: impl Fn(&FakeCloudObservation) -> bool,
) {
    loop {
        let observation = timeout(WAIT, observations.recv())
            .await
            .expect("fake cloud observation timeout")
            .expect("fake cloud observation stream");
        match observation {
            FakeCloudObservation::Failure(message) => panic!("{message}"),
            other if predicate(&other) => return,
            _ => {},
        }
    }
}
