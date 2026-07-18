use std::fs;
use std::path::{Path, PathBuf};

use aether_integration_control::{
    AETHER_CONTRACTS_RELEASE, ActionDecision, ActionReceiptPayload, ActionReceiptStage,
    INTEGRATION_CONTROL_EXTENSION, IntegrationControlCodec, PhysicalOutcome,
};
use serde::Deserialize;
use sha2::{Digest, Sha256};

#[derive(Deserialize)]
struct FixtureManifest {
    fixtures: Vec<FixtureEntry>,
}

#[derive(Deserialize)]
struct FixtureEntry {
    file: String,
    expectation: String,
    sha256: String,
}

fn fixture_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/integration-control/v1alpha1")
}

fn read(relative: &str) -> Vec<u8> {
    fs::read(fixture_root().join(relative)).expect("fixture")
}

#[test]
fn frozen_candidate_hashes_and_expectations_are_preserved() {
    let manifest_bytes = read("fixture-manifest.json");
    assert_eq!(
        format!("{:x}", Sha256::digest(&manifest_bytes)),
        "998d552b5377947cd66f0ac49ec2b627492a41bbfab2530f0d5d33bc4b854471"
    );
    let manifest: FixtureManifest =
        serde_json::from_slice(&manifest_bytes).expect("fixture manifest");
    assert_eq!(
        format!("{:x}", Sha256::digest(read("profile.json"))),
        "25ea6f8bc88eb6a28b094e7c3122c7c5b2ffe415aa0fd000a706721e55adc8c9"
    );
    assert_eq!(
        format!("{:x}", Sha256::digest(read("home-assistant-profile.json"))),
        "2c42ce67a154985f9f0be0fb239b710ba199eb739674c98fece469287d6724a0"
    );

    for fixture in manifest.fixtures {
        let bytes = read(&fixture.file);
        assert_eq!(
            format!("{:x}", Sha256::digest(&bytes)),
            fixture.sha256,
            "{} changed without a candidate lock update",
            fixture.file
        );
        match fixture.expectation.as_str() {
            "valid" if fixture.file.starts_with("action-offer") => {
                IntegrationControlCodec::decode_offer(&bytes).expect("valid offer");
            },
            "valid" => {
                IntegrationControlCodec::decode_receipt_envelope(&bytes)
                    .expect("valid receipt envelope");
            },
            "wire-invalid" if fixture.file.starts_with("invalid/action-offer") => {
                assert!(IntegrationControlCodec::decode_offer(&bytes).is_err());
            },
            "wire-invalid" => {
                assert!(IntegrationControlCodec::decode_receipt_envelope(&bytes).is_err());
            },
            expectation => panic!("unexpected fixture expectation {expectation}"),
        }
    }
}

#[test]
fn valid_offer_has_the_exact_intent_digest_and_signed_projection() {
    let offer =
        IntegrationControlCodec::decode_offer(&read("action-offer.valid.json")).expect("offer");
    assert_eq!(AETHER_CONTRACTS_RELEASE, "0.1.0-alpha.4");
    assert_eq!(offer.extension(), INTEGRATION_CONTROL_EXTENSION);
    assert_eq!(
        offer.intent_digest(),
        "sha256:40108827ca617c95f9d9c48c357fdd94b2b5f019d8ccf8a23842642e934c7327"
    );

    let signed: serde_json::Value =
        serde_json::from_slice(&offer.signing_bytes().expect("signing bytes"))
            .expect("signed projection");
    assert!(signed.get("cloud_authentication").is_none());
    assert_eq!(signed["job_id"], offer.job_id());
    assert_eq!(signed["intent_digest"], offer.intent_digest());
}

#[test]
fn provider_acceptance_is_never_reported_as_physical_completion() {
    let envelope = IntegrationControlCodec::decode_receipt_envelope(&read(
        "action-receipt-provider-accepted.valid.json",
    ))
    .expect("receipt");
    assert_eq!(
        envelope.delivery().digest(),
        "sha256:f42bb6dfcd28ca27a7c1079569ffcd0f6144f741461cd362c3c679f471af80a7"
    );
    assert_eq!(
        envelope.payload().stage(),
        ActionReceiptStage::ProviderAccepted
    );
    assert_eq!(envelope.payload().decision(), ActionDecision::Accepted);
    assert_eq!(
        envelope.payload().physical_outcome(),
        PhysicalOutcome::Unknown
    );
    assert!(!envelope.payload().physical_completed());
    assert!(!envelope.payload().job_succeeded());
}

#[test]
fn edge_rejection_may_preserve_provider_evidence_allowed_by_the_schema() {
    let envelope: serde_json::Value =
        serde_json::from_slice(&read("action-receipt-provider-accepted.valid.json"))
            .expect("receipt JSON");
    let mut payload = envelope["payload"].clone();
    payload["stage"] = serde_json::Value::String("edge-rejected".to_string());
    payload["decision"] = serde_json::Value::String("rejected".to_string());
    payload["failure_code"] = serde_json::Value::String("LOCAL_POLICY_DENIED".to_string());
    let payload: ActionReceiptPayload =
        serde_json::from_value(payload).expect("closed receipt payload");

    payload
        .validate_contract()
        .expect("edge-rejected evidence is optional under the frozen schema");
}
