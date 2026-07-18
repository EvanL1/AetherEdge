use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use aether_cloudlink::{
    CLOUDLINK_INTEGRATION_EXTENSION, CandidateMessage, CloudLinkCodec, CloudLinkCodecError,
    CloudLinkIntegrationExtension, SessionBinding,
};
use aether_domain::TimestampMs;
use aether_integration_contract::{
    IntegrationContractCodec, IntegrationObservationBatchV1Alpha1,
    IntegrationTopologySnapshotV1Alpha1,
};
use aether_ports::{CloudLinkSpool, CloudLinkTransportRoute, DurableAckOutcome};
use aether_store_local::FileCloudLinkSpool;
use serde_json::Value;
use sha2::{Digest, Sha256};

const CONTRACT_MANIFEST_SHA256: &str =
    "a8209d02077b1abe8b34d8b89328452d4b0b561830453276a4f6485c28d7b827";
const INTEGRATION_FIXTURE_MANIFEST_SHA256: &str =
    "8b8e13327fa1c07a0281f051d99fd0a996bfbd6cf5132f189b351603e9ccef06";
const CLOUDLINK_INTEGRATION_FIXTURE_MANIFEST_SHA256: &str =
    "8d474f65319988fa9211ebfec54a23c6ea617daf474b1609252c78091e2c3627";
const CLOUDLINK_INTEGRATION_PROFILE_SHA256: &str =
    "93e3b9d0772066be98e344b39debbef3d8204511e91d17e8d8eaf45bdc1147ab";

fn extension_fixture_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/cloudlink-integration/v1alpha1")
}

fn extension_fixture(name: &str) -> Vec<u8> {
    fs::read(extension_fixture_root().join(name)).expect("checked-in extension fixture")
}

fn integration_fixture(name: &str) -> Vec<u8> {
    fs::read(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../aether-integration-contract/tests/fixtures/integration/v1alpha1")
            .join(name),
    )
    .expect("checked-in Integration fixture")
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

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn session() -> SessionBinding {
    SessionBinding::new(
        "33333333-3333-4333-8333-333333333333",
        "44444444-4444-4444-8444-444444444444",
        7,
        3,
    )
    .expect("fixture session")
}

#[test]
fn candidate_lock_pins_the_exact_alpha4_extension_artifacts_without_claiming_full_adoption() {
    let root = extension_fixture_root();
    let lock: Value = serde_json::from_slice(
        &fs::read(root.join("candidate-lock.json")).expect("candidate lock"),
    )
    .expect("candidate lock JSON");
    assert_eq!(lock["status"], "experimental-extension-only");
    assert_eq!(lock["complete_alpha4_adoption"], false);
    assert_eq!(
        lock["upstream_hashes"]["contract_manifest_sha256"],
        CONTRACT_MANIFEST_SHA256
    );
    assert_eq!(
        lock["upstream_hashes"]["integration_fixture_manifest_sha256"],
        INTEGRATION_FIXTURE_MANIFEST_SHA256
    );
    assert_eq!(
        lock["upstream_hashes"]["cloudlink_integration_fixture_manifest_sha256"],
        CLOUDLINK_INTEGRATION_FIXTURE_MANIFEST_SHA256
    );
    assert_eq!(
        lock["upstream_hashes"]["cloudlink_integration_profile_sha256"],
        CLOUDLINK_INTEGRATION_PROFILE_SHA256
    );
    assert_eq!(
        digest(&fs::read(root.join("fixture-manifest.json")).expect("fixture manifest")),
        CLOUDLINK_INTEGRATION_FIXTURE_MANIFEST_SHA256
    );
    assert_eq!(
        digest(&fs::read(root.join("profile.json")).expect("extension profile")),
        CLOUDLINK_INTEGRATION_PROFILE_SHA256
    );
    assert_eq!(
        digest(&integration_fixture("fixture-manifest.json")),
        INTEGRATION_FIXTURE_MANIFEST_SHA256
    );
}

#[test]
fn fixture_manifest_pins_every_checked_in_public_extension_fixture() {
    let root = extension_fixture_root();
    let manifest: Value = serde_json::from_slice(&extension_fixture("fixture-manifest.json"))
        .expect("fixture manifest JSON");
    let entries = manifest["fixtures"].as_array().expect("fixture entries");
    assert_eq!(entries.len(), 5);

    let pinned = entries
        .iter()
        .map(|entry| {
            let name = entry["file"].as_str().expect("fixture file");
            let bytes = extension_fixture(name);
            assert_eq!(
                digest(&bytes),
                entry["sha256"].as_str().expect("fixture hash"),
                "{name} drifted from the public candidate"
            );
            name.to_string()
        })
        .collect::<BTreeSet<_>>();
    let checked_in = fs::read_dir(&root)
        .expect("fixture directory")
        .map(|entry| {
            entry
                .expect("fixture entry")
                .file_name()
                .into_string()
                .expect("UTF-8 file name")
        })
        .filter(|name| name.ends_with(".valid.json") || name.ends_with(".invalid.json"))
        .collect::<BTreeSet<_>>();
    assert_eq!(checked_in, pinned);
}

#[test]
fn public_extension_envelopes_and_receipts_decode_but_secret_material_fails_closed() {
    for name in [
        "integration-topology.valid.json",
        "integration-observations.valid.json",
        "integration-topology-ack.valid.json",
        "integration-observations-ack.valid.json",
    ] {
        CloudLinkCodec::decode(&extension_fixture(name))
            .unwrap_or_else(|error| panic!("{name} must decode: {error}"));
    }

    let error = CloudLinkCodec::decode(&extension_fixture(
        "integration-topology-secret.invalid.json",
    ))
    .expect_err("provider secret material is outside the closed payload");
    assert_eq!(error.failure_code(), "UNKNOWN_FIELD");
}

#[test]
fn delivery_batch_identity_is_bound_to_the_embedded_integration_fact() {
    for (name, replacement) in [
        ("integration-topology.valid.json", "topology-2"),
        ("integration-observations.valid.json", "other-batch"),
    ] {
        let mut envelope: Value =
            serde_json::from_slice(&extension_fixture(name)).expect("delivery fixture");
        envelope["delivery"]["batch_id"] = Value::String(replacement.to_string());
        let error =
            CloudLinkCodec::decode(&serde_json::to_vec(&envelope).expect("mutated delivery JSON"))
                .expect_err("outer and embedded batch identities cannot diverge");
        assert!(matches!(
            error,
            CloudLinkCodecError::IntegrationBatchIdMismatch
        ));
        assert_eq!(error.failure_code(), "BATCH_ID_MISMATCH");
    }
}

#[tokio::test]
async fn separate_file_streams_survive_restart_replay_and_only_application_ack_removes_them() {
    let root = tempfile::tempdir().expect("temporary journal directory");
    let topology_path = root.path().join("topology.spool");
    let observations_path = root.path().join("observations.spool");
    let topology = topology();
    let observations = observations(&topology);
    let fixture_session = session();

    let (topology_record, observation_record) = {
        let topology_spool =
            FileCloudLinkSpool::open(&topology_path, "integration-topology-home", 16)
                .expect("topology spool");
        let observation_spool =
            FileCloudLinkSpool::open(&observations_path, "integration-observations-home", 16)
                .expect("observation spool");
        let extension = CloudLinkIntegrationExtension::enable_cloud_first(
            &[CLOUDLINK_INTEGRATION_EXTENSION],
            &[CLOUDLINK_INTEGRATION_EXTENSION],
            "home-assistant.home",
            &topology_spool.status().await.expect("topology status"),
            &observation_spool
                .status()
                .await
                .expect("observation status"),
        )
        .expect("explicit Cloud-first activation");

        let topology_record = topology_spool
            .enqueue(
                extension
                    .prepare_topology(&topology, TimestampMs::new(1_784_217_600_000), None)
                    .expect("topology preparation"),
            )
            .await
            .expect("durable topology");
        let observation_record = observation_spool
            .enqueue(
                extension
                    .prepare_observation_batches(
                        &topology,
                        &observations,
                        TimestampMs::new(1_784_217_600_100),
                        None,
                    )
                    .expect("observation preparation")
                    .into_iter()
                    .next()
                    .expect("fitting observation batch"),
            )
            .await
            .expect("durable observations");

        assert_eq!(
            extension
                .route_for_record(&topology_record)
                .expect("topology route"),
            CloudLinkTransportRoute::IntegrationTopologyUp
        );
        assert_eq!(
            extension
                .route_for_record(&observation_record)
                .expect("observation route"),
            CloudLinkTransportRoute::IntegrationObservationsUp
        );
        assert_eq!(
            topology_record.digest(),
            "sha256:32193a4724adc86e721802aca209e68438b7baf433b2f6c01565c0a82767f146"
        );
        assert_eq!(
            observation_record.digest(),
            "sha256:051b0291d257084052a86c90b163b191b72f10d6093789c132180a69226494b6"
        );

        for (spool, record) in [
            (&topology_spool as &dyn CloudLinkSpool, &topology_record),
            (
                &observation_spool as &dyn CloudLinkSpool,
                &observation_record,
            ),
        ] {
            spool
                .mark_offered(record.identity(), &fixture_session.spool_binding())
                .await
                .expect("offer");
            spool
                .mark_transport_published(record.identity(), &fixture_session.spool_binding())
                .await
                .expect("transport publication");
        }
        (topology_record, observation_record)
    };

    let topology_spool = FileCloudLinkSpool::open(&topology_path, "integration-topology-home", 16)
        .expect("reopen topology spool");
    let observation_spool =
        FileCloudLinkSpool::open(&observations_path, "integration-observations-home", 16)
            .expect("reopen observation spool");
    assert_eq!(
        topology_spool
            .replay_from(1, 16)
            .await
            .expect("topology replay")
            .records()[0]
            .identity(),
        topology_record.identity()
    );
    assert_eq!(
        observation_spool
            .replay_from(1, 16)
            .await
            .expect("observation replay")
            .records()[0]
            .identity(),
        observation_record.identity()
    );

    for (spool, ack_name) in [
        (
            &topology_spool as &dyn CloudLinkSpool,
            "integration-topology-ack.valid.json",
        ),
        (
            &observation_spool as &dyn CloudLinkSpool,
            "integration-observations-ack.valid.json",
        ),
    ] {
        let ack = match CloudLinkCodec::decode(&extension_fixture(ack_name))
            .expect("public durable ACK")
        {
            CandidateMessage::DurableAck(message) => message
                .to_spool_ack(&fixture_session)
                .expect("current-session application ACK"),
            other => panic!("unexpected fixture: {other:?}"),
        };
        assert_eq!(
            spool
                .acknowledge(&ack)
                .await
                .expect("first application ACK"),
            DurableAckOutcome::Applied { removed: 1 }
        );
        assert_eq!(
            spool
                .acknowledge(&ack)
                .await
                .expect("idempotent application ACK"),
            DurableAckOutcome::Duplicate
        );
        assert_eq!(
            spool
                .status()
                .await
                .expect("empty status")
                .pending_records(),
            0
        );
    }
}
