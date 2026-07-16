use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::Path;

use aether_cloudlink::{
    CandidateMessage, CloudLinkCodec, CloudLinkCodecError, MessageAuthentication, SessionBinding,
    SessionHello, TopologyBinding,
};
use aether_domain::{
    InstanceId, PointAddress, PointId, PointKind, PointQuality, PointSample, TimestampMs,
};
use aether_ports::{CloudLinkMessageKind, CloudLinkRecord, CloudLinkRecordIdentity};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

#[test]
fn aether_contracts_distribution_lock_pins_imported_bytes() {
    let repository_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let lock: Value = serde_json::from_slice(
        &fs::read(repository_root.join("aether-contracts.lock.json"))
            .expect("checked-in AetherContracts consumer lock"),
    )
    .expect("AetherContracts consumer lock JSON");

    assert_eq!(lock["schema"], "aether.contracts.consumer-lock.v1alpha1");
    assert_eq!(lock["status"], "complete-consumer");
    assert_eq!(lock["release"]["version"], "0.1.0-alpha.3");
    assert_eq!(lock["pending_imports"], serde_json::json!([]));
    assert_eq!(lock["release"]["version"], "0.1.0-alpha.3");
    assert_eq!(lock["policy"]["conformance_claim"], "distribution-only");
    assert_eq!(lock["policy"]["production_release"], false);
    assert_eq!(lock["policy"]["legacy_default"], true);
    assert_eq!(lock["policy"]["physical_control"], false);

    let local_manifest = lock["manifest"]["local_path"]
        .as_str()
        .expect("local manifest path");
    let manifest_bytes = fs::read(repository_root.join(local_manifest))
        .expect("checked-in AetherContracts release manifest");
    let manifest_digest = format!("{:x}", Sha256::digest(&manifest_bytes));
    assert_eq!(
        manifest_digest,
        lock["manifest"]["sha256"]
            .as_str()
            .expect("manifest SHA-256")
    );

    let manifest: Value =
        serde_json::from_slice(&manifest_bytes).expect("AetherContracts release manifest JSON");
    assert_eq!(manifest["contract"], "aether.contracts");
    assert_eq!(manifest["release_version"], "0.1.0-alpha.3");
    assert_eq!(manifest["production_release"], false);
    assert_eq!(manifest["legacy_default"], true);
    assert_eq!(manifest["physical_control"], false);
    let artifacts = manifest["artifacts"]
        .as_array()
        .expect("release artifacts")
        .iter()
        .map(|artifact| {
            (
                artifact["path"].as_str().expect("artifact path").to_owned(),
                artifact["sha256"]
                    .as_str()
                    .expect("artifact SHA-256")
                    .to_owned(),
            )
        })
        .collect::<HashMap<_, _>>();

    let imports = lock["imports"].as_array().expect("consumer imports");
    let pending = lock["pending_imports"]
        .as_array()
        .expect("pending consumer imports");
    assert_eq!(imports.len(), 53, "complete alpha.3 adoption closure");
    assert!(pending.is_empty(), "alpha.3 has no pending imports");

    let mut sources = HashSet::new();
    let mut destinations = HashSet::new();
    for import in imports {
        let source = import["source"].as_str().expect("import source");
        let destination = import["destination"].as_str().expect("import destination");
        let expected = import["sha256"].as_str().expect("import SHA-256");
        assert!(sources.insert(source), "duplicate import source: {source}");
        assert!(
            destinations.insert(destination),
            "duplicate import destination: {destination}"
        );
        assert_eq!(
            artifacts.get(source).map(String::as_str),
            Some(expected),
            "release manifest drift: {source}"
        );
        let actual = format!(
            "{:x}",
            Sha256::digest(
                fs::read(repository_root.join(destination)).expect("imported consumer artifact")
            )
        );
        assert_eq!(actual, expected, "consumer artifact drift: {destination}");
    }
    for entry in pending {
        let source = entry["source"].as_str().expect("pending source");
        let expected = entry["sha256"].as_str().expect("pending SHA-256");
        assert!(
            sources.insert(source),
            "source is both imported and pending: {source}"
        );
        assert_eq!(
            artifacts.get(source).map(String::as_str),
            Some(expected),
            "pending source is not release-pinned: {source}"
        );
    }
}

fn fixture(name: &str) -> Vec<u8> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../contracts/cloudlink/v1/fixtures")
        .join(name);
    fs::read(path).expect("checked-in CloudLink fixture")
}

#[test]
fn product_integration_manifest_and_hello_are_pinned() {
    let contract_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../contracts/cloudlink/v1");
    let manifest: Value = serde_json::from_slice(
        &fs::read(contract_root.join("contract-manifest.json"))
            .expect("checked-in joint contract manifest"),
    )
    .expect("contract manifest JSON");
    assert_eq!(manifest["contracts_release"], "0.1.0-alpha.3");
    assert_eq!(manifest["status"], "product-local-implementation-overlay");
    assert_eq!(manifest["authority"], "AetherContracts v0.1.0-alpha.3");
    assert_eq!(manifest["owner"], "AetherIot implementation evidence");
    assert_eq!(
        manifest["authoritative_artifacts"]["core_profile"],
        "AetherContracts profiles/cloudlink/v1alpha1/core.json"
    );
    assert_eq!(manifest["implementation_limits"]["legacy_default"], true);
    assert_eq!(
        manifest["implementation_limits"]["physical_control"],
        "forbidden"
    );
    assert!(manifest.get("mqtt_topics").is_none());
    assert!(manifest.get("freeze").is_none());

    let hello: Value = serde_json::from_slice(&fixture("session-hello.valid.json"))
        .expect("session hello fixture JSON");
    assert_eq!(
        hello["credential_binding"]["origin_model"],
        "gateway-signed"
    );
    assert_eq!(hello["gateway_signature"]["algorithm"], "Ed25519");
    assert!(
        hello["gateway_id"]
            .as_str()
            .is_some_and(|value| value.len() == 36)
    );
}

#[test]
fn product_fixture_manifest_pins_every_fixture_byte() {
    let contract_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../contracts/cloudlink/v1");
    let manifest: Value = serde_json::from_slice(
        &fs::read(contract_root.join("fixture-manifest.json")).expect("fixture manifest"),
    )
    .expect("fixture manifest JSON");
    let entries = manifest["fixtures"].as_array().expect("fixture entries");
    let fixture_root = contract_root.join("fixtures");
    let mut pinned = entries
        .iter()
        .map(|entry| {
            entry["file"]
                .as_str()
                .expect("fixture file name")
                .to_string()
        })
        .collect::<Vec<_>>();
    let mut checked_in = fs::read_dir(&fixture_root)
        .expect("fixture directory")
        .map(|entry| {
            entry
                .expect("fixture entry")
                .file_name()
                .into_string()
                .expect("UTF-8 fixture file name")
        })
        .collect::<Vec<_>>();
    pinned.sort();
    checked_in.sort();
    assert_eq!(pinned, checked_in);

    for entry in entries {
        let name = entry["file"].as_str().expect("fixture file name");
        let expected = entry["sha256"].as_str().expect("fixture SHA-256");
        let actual = format!("{:x}", Sha256::digest(fixture(name)));
        assert_eq!(
            actual, expected,
            "fixture {name} changed without a freeze update"
        );
    }
}

#[test]
fn product_codec_executes_the_complete_public_fixture_manifest() {
    let contract_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../contracts/cloudlink/v1");
    let manifest: Value = serde_json::from_slice(
        &fs::read(contract_root.join("fixture-manifest.json")).expect("fixture manifest"),
    )
    .expect("fixture manifest JSON");
    let entries = manifest["fixtures"].as_array().expect("fixture entries");
    assert_eq!(entries.len(), 25, "the complete alpha.3 public fixture set");

    for entry in entries {
        let name = entry["file"].as_str().expect("fixture file");
        let expectation = entry["expectation"].as_str().expect("expectation");
        let decoded = CloudLinkCodec::decode(&fixture(name));
        let decoder_rejects_context = matches!(
            name,
            "conflicting-replay.json" | "session-accepted-duplicate-cursor.json"
        );
        if expectation == "wire-invalid" || decoder_rejects_context {
            let error = decoded.expect_err("fixture must fail at the product codec boundary");
            assert_eq!(
                error.failure_code(),
                entry["failure_code"].as_str().expect("failure code"),
                "{name} must use the public stable failure taxonomy"
            );
            continue;
        }
        assert!(
            decoded.is_ok(),
            "{name} must be structurally valid: {decoded:?}"
        );
        if expectation == "context-invalid" {
            let failure_code = match name {
                "conflicting-replay.valid-digest.json" => {
                    let current: Value = serde_json::from_slice(&fixture(name)).expect("conflict");
                    let prior: Value =
                        serde_json::from_slice(&fixture("telemetry-batch.valid.json"))
                            .expect("prior delivery");
                    for pointer in [
                        "/gateway_id",
                        "/delivery/stream_id",
                        "/delivery/stream_epoch",
                        "/delivery/position",
                    ] {
                        assert_eq!(current.pointer(pointer), prior.pointer(pointer));
                    }
                    assert!(
                        current.pointer("/delivery/batch_id")
                            != prior.pointer("/delivery/batch_id")
                            || current.pointer("/delivery/digest")
                                != prior.pointer("/delivery/digest")
                    );
                    "DIGEST_CONFLICT"
                },
                "stale-ack.json" | "wrong-session-epoch.json" => decoded
                    .expect("structural context fixture")
                    .validate_session(&session())
                    .expect_err("stale session context")
                    .failure_code(),
                other => panic!("unhandled context fixture {other}"),
            };
            assert_eq!(
                failure_code,
                entry["failure_code"].as_str().expect("failure code"),
                "{name} must use the public stable context failure taxonomy"
            );
        }
    }
}

#[test]
fn session_hello_signature_shape_is_serialized_but_redacted_from_diagnostics() {
    let signature = MessageAuthentication::new("development-gateway-key-17", "B".repeat(86))
        .expect("signature shape");
    assert_eq!(
        format!("{signature:?}"),
        "MessageAuthentication([REDACTED])"
    );
    let hello = SessionHello::new_gateway_signed(
        "33333333-3333-4333-8333-333333333333",
        "development-binding-17",
        3,
        "22222222-2222-4222-8222-222222222222",
        "development-gateway-key-17",
        signature,
        vec!["1.0".to_string()],
        "A".repeat(43),
        Vec::new(),
    )
    .expect("hello");
    let diagnostics = format!("{hello:?}");
    for secret_or_identifier in [
        "33333333-3333-4333-8333-333333333333",
        "development-binding-17",
        "22222222-2222-4222-8222-222222222222",
        "development-gateway-key-17",
        &"A".repeat(43),
        &"B".repeat(86),
    ] {
        assert!(
            !diagnostics.contains(secret_or_identifier),
            "ordinary diagnostics must redact auth transcript material"
        );
    }
    assert!(
        String::from_utf8(CloudLinkCodec::encode(&hello).expect("hello JSON"))
            .expect("UTF-8")
            .contains(&"B".repeat(86))
    );
}

fn session() -> SessionBinding {
    SessionBinding::new(
        "33333333-3333-4333-8333-333333333333",
        "44444444-4444-4444-8444-444444444444",
        7,
        3,
    )
    .expect("valid session")
}

fn telemetry_sample() -> PointSample {
    PointSample::new(
        PointAddress::new(InstanceId::new(42), PointKind::Telemetry, PointId::new(8)),
        12.5,
        TimestampMs::new(1_721_000_000_123),
        PointQuality::Uncertain,
    )
}

#[test]
fn all_valid_golden_fixtures_decode_strictly() {
    for name in [
        "session-challenge.valid.json",
        "session-hello.valid.json",
        "session-accepted.valid.json",
        "heartbeat.valid.json",
        "heartbeat-ack.valid.json",
        "runtime-manifest-report.valid.json",
        "telemetry-batch.valid.json",
        "durable-ack.valid.json",
        "replay-request.valid.json",
        "data-loss.valid.json",
    ] {
        CloudLinkCodec::decode(&fixture(name)).unwrap_or_else(|error| {
            panic!("fixture {name} must decode: {error}");
        });
    }
}

#[test]
fn alpha3_authentication_and_manifest_gap_fixtures_fail_closed() {
    for name in [
        "session-hello-auth-required.json",
        "session-hello-auth-invalid.json",
        "runtime-manifest-invalid-semver.json",
    ] {
        let _ = CloudLinkCodec::decode(&fixture(name))
            .expect_err("alpha.3 gap fixture must fail closed");
    }
}

#[test]
fn unsupported_versions_unknown_fields_and_unsafe_uint64_fail_closed() {
    for (name, expected) in [
        ("unsupported-version.json", "protocol version"),
        ("unknown-field.json", "unknown field"),
        ("unsafe-uint64.json", "canonical uint64"),
        ("overflow-uint64.json", "out of range"),
    ] {
        let error = CloudLinkCodec::decode(&fixture(name)).expect_err("invalid fixture");
        assert!(
            error.to_string().contains(expected),
            "{name}: expected {expected:?}, got {error}"
        );
    }
}

#[test]
fn message_size_and_sample_count_are_bounded_before_serialisation() {
    let oversized = vec![b' '; aether_cloudlink::MAX_CLOUDLINK_MESSAGE_BYTES + 1];
    assert!(matches!(
        CloudLinkCodec::decode(&oversized),
        Err(CloudLinkCodecError::MessageTooLarge { .. })
    ));

    let samples = vec![telemetry_sample(); aether_cloudlink::MAX_POINT_SAMPLES + 1];
    assert!(matches!(
        CloudLinkCodec::telemetry_batch(
            TopologyBinding::new(11, "fx64:0123456789abcdef").expect("topology"),
            &samples,
        ),
        Err(CloudLinkCodecError::TooManySamples { .. })
    ));
}

#[test]
fn telemetry_preserves_real_point_fact_fields_without_inventing_a_model() {
    let payload = CloudLinkCodec::telemetry_batch(
        TopologyBinding::new(11, "fx64:0123456789abcdef").expect("topology"),
        &[telemetry_sample()],
    )
    .expect("valid telemetry payload");
    let value = serde_json::to_value(payload).expect("JSON value");

    assert_eq!(value["topology"]["publication_epoch"], "11");
    assert_eq!(value["samples"][0]["instance_id"], "42");
    assert_eq!(value["samples"][0]["point_kind"], "telemetry");
    assert_eq!(value["samples"][0]["point_id"], "8");
    assert_eq!(value["samples"][0]["value"], 12.5);
    assert_eq!(value["samples"][0]["source_timestamp_ms"], "1721000000123");
    assert_eq!(value["samples"][0]["quality"], "uncertain");
    assert!(value["samples"][0].get("thing_model").is_none());
    assert!(value.get("metrics").is_none());
}

#[test]
fn writable_point_kinds_are_not_a_cloudlink_control_backdoor() {
    let action = PointSample::new(
        PointAddress::new(InstanceId::new(42), PointKind::Action, PointId::new(8)),
        1.0,
        TimestampMs::new(1),
        PointQuality::Good,
    );

    assert!(matches!(
        CloudLinkCodec::telemetry_batch(
            TopologyBinding::new(11, "fx64:0123456789abcdef").expect("topology"),
            &[action],
        ),
        Err(CloudLinkCodecError::ControlPointForbidden)
    ));
}

#[test]
fn operational_telemetry_cannot_enter_a_business_point_batch() {
    let payload = CloudLinkCodec::telemetry_batch(
        TopologyBinding::new(11, "fx64:0123456789abcdef").expect("topology"),
        &[telemetry_sample()],
    )
    .expect("payload");
    let mut value = serde_json::to_value(payload).expect("payload JSON");
    value["metrics"] = json!({"aether.channel.online": 1});

    assert!(
        CloudLinkCodec::prepare_value(
            CloudLinkMessageKind::TelemetryBatch,
            "batch-ops",
            value,
            TimestampMs::new(1),
            None,
        )
        .is_err()
    );
}

#[test]
fn envelope_digest_excludes_session_and_trace_but_detects_business_changes() {
    let payload = CloudLinkCodec::telemetry_batch(
        TopologyBinding::new(11, "fx64:0123456789abcdef").expect("topology"),
        &[telemetry_sample()],
    )
    .expect("payload");
    let prepared = CloudLinkCodec::prepare(
        CloudLinkMessageKind::TelemetryBatch,
        "batch-1",
        &payload,
        TimestampMs::new(10),
        Some(TimestampMs::new(20)),
    )
    .expect("prepared record");
    let record = CloudLinkRecord::from_enqueue(
        CloudLinkRecordIdentity::new("telemetry", 4, 19),
        prepared.clone(),
    );

    let first = CloudLinkCodec::delivery_envelope(
        &session(),
        &record,
        TimestampMs::new(12),
        Some("00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01"),
    )
    .expect("first envelope");
    let second_session = SessionBinding::new(
        "33333333-3333-4333-8333-333333333333",
        "55555555-5555-4555-8555-555555555555",
        8,
        3,
    )
    .expect("new session");
    let replay =
        CloudLinkCodec::delivery_envelope(&second_session, &record, TimestampMs::new(13), None)
            .expect("replay envelope");

    assert_eq!(first.delivery().digest(), replay.delivery().digest());
    assert_eq!(first.delivery().batch_id(), replay.delivery().batch_id());
    assert_eq!(first.delivery().position(), replay.delivery().position());

    let mut changed: Value = serde_json::to_value(payload).expect("payload JSON");
    changed["samples"][0]["value"] = json!(99.0);
    let changed = CloudLinkCodec::prepare_value(
        CloudLinkMessageKind::TelemetryBatch,
        "batch-1",
        changed,
        TimestampMs::new(10),
        Some(TimestampMs::new(20)),
    )
    .expect("changed record");
    assert_ne!(prepared.digest(), changed.digest());
}

#[test]
fn invalid_digest_wrong_epoch_and_conflicting_replay_fixtures_are_rejected() {
    for name in [
        "invalid-digest.json",
        "oversized-payload.json",
        "conflicting-replay.json",
    ] {
        assert!(
            CloudLinkCodec::decode(&fixture(name)).is_err(),
            "{name} must fail closed"
        );
    }
}

#[test]
fn runtime_manifest_report_uses_the_existing_verified_checksum() {
    let manifest =
        aether_runtime_catalog::shipped_distribution_manifest("aarch64-unknown-linux-musl")
            .expect("generated manifest");
    let payload = CloudLinkCodec::runtime_manifest_report(
        manifest.to_pretty_json().expect("manifest JSON").as_bytes(),
        TimestampMs::new(1_721_000_000_123),
    )
    .expect("verified report");

    assert_eq!(
        payload.manifest()["checksum"]["digest"],
        manifest.checksum().digest()
    );
}

#[test]
fn session_accepted_and_heartbeat_ack_validate_the_current_epoch() {
    let accepted = match CloudLinkCodec::decode(&fixture("session-accepted.valid.json"))
        .expect("accepted fixture")
    {
        CandidateMessage::SessionAccepted(value) => value,
        other => panic!("wrong message: {other:?}"),
    };
    let bound = accepted
        .bind("33333333-3333-4333-8333-333333333333", 3, &["1.0"], 6)
        .expect("accepted session");
    assert_eq!(bound.session_epoch(), 7);
    assert_eq!(accepted.heartbeat_interval_ms(), 30_000);
    assert_eq!(accepted.resume_cursors().len(), 1);
    assert_eq!(accepted.resume_cursors()[0].stream_id(), "telemetry");
    assert_eq!(accepted.resume_cursors()[0].stream_epoch(), 4);
    assert_eq!(accepted.resume_cursors()[0].acknowledged_position(), 18);
    assert!(matches!(
        accepted.bind("33333333-3333-4333-8333-333333333333", 4, &["1.0"], 6),
        Err(CloudLinkCodecError::SessionMismatch)
    ));

    let heartbeat_ack =
        CloudLinkCodec::decode(&fixture("heartbeat-ack.valid.json")).expect("heartbeat ack");
    heartbeat_ack
        .validate_session(&bound)
        .expect("current heartbeat ACK");

    let stale_heartbeat = CloudLinkCodec::decode(&fixture("wrong-session-epoch.json"))
        .expect("stale heartbeat remains structurally valid");
    assert!(matches!(
        stale_heartbeat.validate_session(&bound),
        Err(CloudLinkCodecError::SessionMismatch)
    ));

    let stale_ack = CloudLinkCodec::decode(&fixture("stale-ack.json"))
        .expect("stale ACK remains structurally valid");
    assert!(matches!(
        stale_ack.validate_session(&bound),
        Err(CloudLinkCodecError::SessionMismatch)
    ));
}
