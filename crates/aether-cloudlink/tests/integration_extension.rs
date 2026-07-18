use std::path::{Path, PathBuf};

use aether_cloudlink::{
    CLOUDLINK_INTEGRATION_EXTENSION, CloudLinkCodec, CloudLinkCodecError,
    CloudLinkIntegrationExtension, MAX_CLOUDLINK_MESSAGE_BYTES, SessionBinding,
    UplinkAuthentication,
};
use aether_domain::TimestampMs;
use aether_integration_contract::{
    IntegrationContractCodec, IntegrationObservationBatchV1Alpha1,
    IntegrationTopologySnapshotV1Alpha1,
};
use aether_ports::{
    CloudLinkMessageKind, CloudLinkRecord, CloudLinkRecordIdentity, CloudLinkSpool,
};
use aether_store_local::MemoryCloudLinkSpool;
use serde_json::{Value, json};

fn integration_fixture(relative: &str) -> Vec<u8> {
    std::fs::read(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../aether-integration-contract/tests/fixtures/integration/v1alpha1")
            .join(relative),
    )
    .expect("pinned Integration fixture")
}

fn topology() -> IntegrationTopologySnapshotV1Alpha1 {
    IntegrationContractCodec::decode_topology(&integration_fixture(
        "valid/home-assistant-topology.json",
    ))
    .expect("topology fixture")
}

fn observations(
    topology: &IntegrationTopologySnapshotV1Alpha1,
) -> IntegrationObservationBatchV1Alpha1 {
    IntegrationContractCodec::decode_observation_batch(
        &integration_fixture("valid/home-assistant-observations.json"),
        topology,
    )
    .expect("observation fixture")
}

async fn extension() -> CloudLinkIntegrationExtension {
    let topology_spool =
        MemoryCloudLinkSpool::new("integration-topology-home", 16).expect("topology spool");
    let observation_spool =
        MemoryCloudLinkSpool::new("integration-observations-home", 16).expect("observation spool");
    CloudLinkIntegrationExtension::enable_cloud_first(
        &[CLOUDLINK_INTEGRATION_EXTENSION],
        &[CLOUDLINK_INTEGRATION_EXTENSION],
        "home-assistant.home",
        &topology_spool.status().await.expect("topology status"),
        &observation_spool
            .status()
            .await
            .expect("observation status"),
    )
    .expect("extension activation")
}

#[tokio::test]
async fn activation_requires_runtime_declaration_cloud_confirmation_and_distinct_streams() {
    let topology_spool =
        MemoryCloudLinkSpool::new("integration-topology-home", 16).expect("topology spool");
    let observation_spool =
        MemoryCloudLinkSpool::new("integration-observations-home", 16).expect("observation spool");
    let topology_status = topology_spool.status().await.expect("topology status");
    let observation_status = observation_spool
        .status()
        .await
        .expect("observation status");

    for (runtime_protocols, cloud_extensions) in [
        (Vec::<&str>::new(), vec![CLOUDLINK_INTEGRATION_EXTENSION]),
        (vec![CLOUDLINK_INTEGRATION_EXTENSION], Vec::<&str>::new()),
        (vec!["aether.cloudlink"], vec!["1.0"]),
    ] {
        let error = CloudLinkIntegrationExtension::enable_cloud_first(
            &runtime_protocols,
            &cloud_extensions,
            "home-assistant.home",
            &topology_status,
            &observation_status,
        )
        .expect_err("base negotiation or one-sided enablement must not activate the extension");
        assert_eq!(error.failure_code(), "UNSUPPORTED_VERSION");
    }

    let error = CloudLinkIntegrationExtension::enable_cloud_first(
        &[CLOUDLINK_INTEGRATION_EXTENSION],
        &[CLOUDLINK_INTEGRATION_EXTENSION],
        "home-assistant.home",
        &topology_status,
        &topology_status,
    )
    .expect_err("topology and observations require distinct streams");
    assert_eq!(error.failure_code(), "STREAM_BINDING_CONFLICT");
}

#[tokio::test]
async fn topology_is_one_atomic_fact_with_the_contract_batch_binding() {
    let extension = extension().await;
    let topology = topology();
    let input = extension
        .prepare_topology(&topology, TimestampMs::new(1_784_217_600_000), None)
        .expect("topology preparation");

    assert_eq!(
        input.message_kind(),
        CloudLinkMessageKind::IntegrationTopologySnapshot
    );
    assert_eq!(input.batch_id(), "topology-1");
    assert_eq!(
        IntegrationContractCodec::decode_topology(input.payload())
            .expect("unchanged public payload"),
        topology
    );
    assert!(input.payload().len() < MAX_CLOUDLINK_MESSAGE_BYTES);
}

#[tokio::test]
async fn observations_preserve_one_fitting_batch_and_split_only_at_observation_boundaries() {
    let extension = extension().await;
    let topology = topology();
    let original = observations(&topology);
    let fitting = extension
        .prepare_observation_batches(
            &topology,
            &original,
            TimestampMs::new(1_784_217_600_100),
            None,
        )
        .expect("fitting batch");
    assert_eq!(fitting.len(), 1);
    assert_eq!(fitting[0].batch_id(), original.batch_id());

    let mut oversized: Value = serde_json::from_slice(&integration_fixture(
        "valid/home-assistant-observations.json",
    ))
    .expect("observation JSON");
    oversized["batch_id"] = json!("batch-that-must-be-partitioned");
    let template = json!({
        "entity_id": "entity-registry-climate-living",
        "point_key": "hvac_mode",
        "observed_at_ms": "1784217600100",
        "quality": "good",
        "value": {
            "type": "string",
            "value": "x".repeat(4096)
        }
    });
    oversized["observations"] = Value::Array(vec![template; 96]);
    let oversized = IntegrationContractCodec::decode_observation_batch(
        &serde_json::to_vec(&oversized).expect("oversized JSON"),
        &topology,
    )
    .expect("schema-valid oversized batch");

    let partitions = extension
        .prepare_observation_batches(
            &topology,
            &oversized,
            TimestampMs::new(1_784_217_600_100),
            None,
        )
        .expect("partitioned observations");
    assert!(partitions.len() > 1);
    assert_eq!(
        partitions
            .iter()
            .map(|partition| {
                assert_eq!(
                    partition.message_kind(),
                    CloudLinkMessageKind::IntegrationObservationBatch
                );
                let decoded = IntegrationContractCodec::decode_observation_batch(
                    partition.payload(),
                    &topology,
                )
                .expect("independent public batch");
                assert_eq!(partition.batch_id(), decoded.batch_id());
                decoded.observations().len()
            })
            .sum::<usize>(),
        oversized.observations().len()
    );
    let mut batch_ids = partitions
        .iter()
        .map(|partition| partition.batch_id())
        .collect::<Vec<_>>();
    batch_ids.sort_unstable();
    batch_ids.dedup();
    assert_eq!(batch_ids.len(), partitions.len());

    for (index, partition) in partitions.into_iter().enumerate() {
        let record = CloudLinkRecord::from_enqueue(
            CloudLinkRecordIdentity::new(
                "s".repeat(128),
                u64::MAX,
                u64::try_from(index + 1).expect("position"),
            ),
            partition,
        );
        let session = SessionBinding::new(
            "33333333-3333-4333-8333-333333333333",
            "44444444-4444-4444-8444-444444444444",
            u64::MAX,
            u64::MAX,
        )
        .expect("maximum-width session");
        let envelope = CloudLinkCodec::delivery_envelope(
            &session,
            &record,
            Some("00-11111111111111111111111111111111-2222222222222222-01"),
            &UplinkAuthentication::trusted_connector_broker_attestation(),
        )
        .expect("worst-case envelope");
        assert!(
            CloudLinkCodec::encode(&envelope)
                .expect("bounded complete message")
                .len()
                <= MAX_CLOUDLINK_MESSAGE_BYTES
        );
    }
}

#[tokio::test]
async fn oversized_topology_fails_as_one_fact_and_is_never_fragmented() {
    let extension = extension().await;
    let mut candidate = json!({
        "schema": "aether.integration.topology-snapshot.v1alpha1",
        "integration_id": "home-assistant.home",
        "integration_kind": "home-assistant",
        "snapshot_generation": "2",
        "observed_at_ms": "1784217600000",
        "areas": [],
        "devices": [],
        "entities": []
    });
    candidate["entities"] = Value::Array(
        (0..2_000)
            .map(|index| {
                json!({
                    "entity_id": format!("entity-{index}"),
                    "source_address": format!("sensor.entity_{index}"),
                    "name": format!("Large topology entity {index} {}", "n".repeat(180)),
                    "entity_kind": "sensor",
                    "points": [{
                        "point_key": "state",
                        "title": "State",
                        "kind": "telemetry",
                        "value_type": "string"
                    }]
                })
            })
            .collect(),
    );
    let topology = IntegrationContractCodec::decode_topology(
        &serde_json::to_vec(&candidate).expect("topology JSON"),
    )
    .expect("schema-valid large topology");

    let error = extension
        .prepare_topology(&topology, TimestampMs::new(1_784_217_600_000), None)
        .expect_err("complete topology cannot be split");
    assert!(matches!(error, CloudLinkCodecError::MessageTooLarge { .. }));
    assert_eq!(error.failure_code(), "FIELD_BOUND");
}

#[test]
fn fixture_path_is_workspace_relative_and_contains_no_external_checkout_dependency() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../aether-integration-contract/tests/fixtures/integration/v1alpha1");
    assert!(path.is_dir());
}
