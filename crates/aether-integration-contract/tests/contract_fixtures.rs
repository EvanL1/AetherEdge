use std::fs;
use std::path::{Path, PathBuf};

use aether_integration_contract::{IntegrationContractCodec, IntegrationContractErrorCode};
use serde::Deserialize;
use sha2::{Digest, Sha256};

const OFFICIAL_ALPHA4_FIXTURE_MANIFEST_SHA256: &str =
    "8b8e13327fa1c07a0281f051d99fd0a996bfbd6cf5132f189b351603e9ccef06";

#[derive(Deserialize)]
struct FixtureManifest {
    fixtures: Vec<FixtureEntry>,
}

#[derive(Deserialize)]
struct FixtureEntry {
    file: String,
    expectation: String,
    schema_id: String,
    #[serde(default)]
    failure_code: Option<String>,
    sha256: String,
}

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/integration/v1alpha1")
}

fn read(relative: &str) -> Vec<u8> {
    fs::read(fixtures().join(relative)).expect("pinned fixture is readable")
}

fn sha256(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[test]
fn official_alpha4_valid_fixtures_round_trip_through_the_product_binding() {
    let topology_bytes = read("valid/home-assistant-topology.json");
    let topology =
        IntegrationContractCodec::decode_topology(&topology_bytes).expect("official topology");
    let batch = IntegrationContractCodec::decode_observation_batch(
        &read("valid/home-assistant-observations.json"),
        &topology,
    )
    .expect("official observations");

    let topology_round_trip =
        IntegrationContractCodec::encode_topology(&topology).expect("topology encodes");
    let batch_round_trip = IntegrationContractCodec::encode_observation_batch(&batch, &topology)
        .expect("batch encodes");
    assert_eq!(
        IntegrationContractCodec::decode_topology(&topology_round_trip)
            .expect("topology round trip"),
        topology
    );
    assert_eq!(
        IntegrationContractCodec::decode_observation_batch(&batch_round_trip, &topology)
            .expect("batch round trip"),
        batch
    );
}

#[test]
fn official_alpha4_manifest_hashes_and_failure_codes_are_enforced() {
    let manifest_bytes = read("fixture-manifest.json");
    assert_eq!(
        sha256(&manifest_bytes),
        OFFICIAL_ALPHA4_FIXTURE_MANIFEST_SHA256
    );
    let manifest: FixtureManifest =
        serde_json::from_slice(&manifest_bytes).expect("official fixture manifest");
    let topology =
        IntegrationContractCodec::decode_topology(&read("valid/home-assistant-topology.json"))
            .expect("official topology");

    for entry in manifest.fixtures {
        let bytes = read(&entry.file);
        assert_eq!(sha256(&bytes), entry.sha256, "{}", entry.file);
        if entry.expectation == "valid" {
            continue;
        }

        let result = if entry
            .schema_id
            .ends_with("integration-topology-snapshot.schema.json")
        {
            IntegrationContractCodec::decode_topology(&bytes).map(|_| ())
        } else if entry
            .schema_id
            .ends_with("integration-observation-batch.schema.json")
        {
            IntegrationContractCodec::decode_observation_batch(&bytes, &topology).map(|_| ())
        } else {
            IntegrationContractCodec::decode_observed_value(&bytes).map(|_| ())
        };
        let error = result.expect_err(&entry.file);
        assert_eq!(
            error.code().as_str(),
            entry.failure_code.as_deref().expect("invalid fixture code"),
            "{}",
            entry.file
        );
    }
}

#[test]
fn closed_decoder_rejects_duplicate_fields_and_foundation_unsafe_numbers() {
    let duplicate = br#"{
      "type":"boolean",
      "value":true,
      "value":false
    }"#;
    assert_eq!(
        IntegrationContractCodec::decode_observed_value(duplicate)
            .expect_err("duplicate field")
            .code(),
        IntegrationContractErrorCode::JsonSyntaxError
    );

    for value in [b"1e100".as_slice(), b"1.5e20".as_slice()] {
        let candidate = [
            br#"{"type":"float64","value":"#.as_slice(),
            value,
            b"}".as_slice(),
        ]
        .concat();
        assert_eq!(
            IntegrationContractCodec::decode_observed_value(&candidate)
                .expect_err("unsafe number")
                .code(),
            IntegrationContractErrorCode::JsonUnsafeNumber
        );
    }

    for value in [b"1e-100".as_slice(), b"1.7976931348623157e308".as_slice()] {
        let candidate = [
            br#"{"type":"float64","value":"#.as_slice(),
            value,
            b"}".as_slice(),
        ]
        .concat();
        IntegrationContractCodec::decode_observed_value(&candidate).expect("Foundation-safe float");
    }
}
