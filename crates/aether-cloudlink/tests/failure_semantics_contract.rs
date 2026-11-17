use std::fs;
use std::path::Path;

use aether_cloudlink::{
    CloudLinkCodec, SessionBinding, UplinkAuthentication, UplinkSigningProjection,
};
use aether_domain::TimestampMs;
use aether_ports::{
    CloudLinkEnqueue, CloudLinkMessageKind, CloudLinkRecord, CloudLinkRecordIdentity,
};
use serde_json::Value;

const GATEWAY_ID: &str = "33333333-3333-4333-8333-333333333333";
const SESSION_ID: &str = "44444444-4444-4444-8444-444444444444";

fn fixture(name: &str) -> Value {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../contracts/cloudlink/v1/fixtures")
        .join(name);
    serde_json::from_slice(&fs::read(path).expect("checked-in CloudLink fixture"))
        .expect("CloudLink fixture JSON")
}

#[test]
fn an_inverted_expiry_window_is_reported_wherever_one_is_sealed_offered_or_signed() {
    let telemetry = fixture("telemetry-batch.valid.json");
    let sent = TimestampMs::new(20);
    let earlier = TimestampMs::new(19);
    let seal = |expires| {
        CloudLinkCodec::prepare_value(
            CloudLinkMessageKind::TelemetryBatch,
            "batch-1",
            telemetry["payload"].clone(),
            sent,
            expires,
        )
    };

    let sealing = seal(Some(earlier)).expect_err("an inverted window must not be sealed");
    assert_eq!(sealing.failure_code(), "INVALID_EXPIRY_WINDOW");

    let sealed = seal(None).expect("sealed telemetry");
    let record = CloudLinkRecord::from_enqueue(
        CloudLinkRecordIdentity::new("telemetry", 4, 19),
        CloudLinkEnqueue::new(
            sealed.message_kind(),
            sealed.batch_id(),
            sealed.digest(),
            sealed.payload(),
            sent,
            Some(earlier),
        ),
    );
    let offering = CloudLinkCodec::delivery_envelope(
        &SessionBinding::new(GATEWAY_ID, SESSION_ID, 7, 3).expect("session"),
        &record,
        None,
        &UplinkAuthentication::trusted_connector_broker_attestation(),
    )
    .expect_err("an inverted window must not be offered");
    assert_eq!(offering.failure_code(), "INVALID_EXPIRY_WINDOW");

    let signing = UplinkSigningProjection::delivery(
        GATEWAY_ID,
        "3",
        SESSION_ID,
        "7",
        "telemetry-batch",
        "20",
        Some("19"),
        "telemetry",
        "4",
        "19",
        "batch-1",
        sealed.digest(),
    )
    .expect_err("an inverted window must not be signed");
    assert_eq!(signing.failure_code(), "INVALID_EXPIRY_WINDOW");
}
