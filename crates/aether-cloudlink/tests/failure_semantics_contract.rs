use std::fs;
use std::path::Path;

use aether_cloudlink::{
    CloudLinkCodec, SessionBinding, UplinkAuthentication, UplinkSigningProjection,
};
use aether_domain::TimestampMs;
use aether_ports::{
    CloudLinkEnqueue, CloudLinkMessageKind, CloudLinkRecord, CloudLinkRecordIdentity,
};
use serde_json::{Value, json};

const GATEWAY_ID: &str = "33333333-3333-4333-8333-333333333333";
const SESSION_ID: &str = "44444444-4444-4444-8444-444444444444";

fn fixture(name: &str) -> Value {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../contracts/cloudlink/v1/fixtures")
        .join(name);
    serde_json::from_slice(&fs::read(path).expect("checked-in CloudLink fixture"))
        .expect("CloudLink fixture JSON")
}

fn decode_failure(document: &Value) -> &'static str {
    CloudLinkCodec::decode(&serde_json::to_vec(document).expect("document JSON"))
        .expect_err("the document must not decode")
        .failure_code()
}

fn distinct_cursors(count: u64) -> Value {
    (1..=count)
        .map(|epoch| {
            json!({
                "stream_id": "telemetry",
                "stream_epoch": epoch.to_string(),
                "acknowledged_position": "18",
            })
        })
        .collect()
}

const CURSOR_ARRAYS: [(&str, &str); 5] = [
    ("session-challenge-request.valid.json", "resume"),
    ("session-hello.valid.json", "resume"),
    ("session-accepted.valid.json", "resume"),
    ("heartbeat.valid.json", "cursors"),
    ("heartbeat-ack.valid.json", "cursors"),
];

#[test]
fn more_cursors_than_the_schema_allows_exceed_a_field_bound() {
    for (name, array) in CURSOR_ARRAYS {
        let mut document = fixture(name);
        document[array] = distinct_cursors(32);
        CloudLinkCodec::decode(&serde_json::to_vec(&document).expect("document JSON"))
            .unwrap_or_else(|error| panic!("{name} allows 32 cursors: {error}"));

        document[array] = distinct_cursors(33);
        assert_eq!(decode_failure(&document), "FIELD_BOUND", "{name}");
    }
}

#[test]
fn a_repeated_stream_and_epoch_conflicts_in_every_cursor_array() {
    for (name, array) in CURSOR_ARRAYS {
        let mut document = fixture(name);
        let cursors = document[array].as_array_mut().expect("cursor array");
        let mut repeated = cursors[0].clone();
        repeated["acknowledged_position"] = json!("19");
        cursors.push(repeated);

        assert_eq!(decode_failure(&document), "CURSOR_CONFLICT", "{name}");
    }
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

#[test]
fn a_data_loss_range_that_is_unordered_or_reaches_retained_data_is_reported_when_sealed() {
    for (member, position) in [
        ("first_lost_position", "4"),
        ("earliest_retained_position", "3"),
    ] {
        let mut payload = fixture("data-loss.valid.json")["payload"].clone();
        payload[member] = Value::String(position.to_owned());

        let error = CloudLinkCodec::prepare_value(
            CloudLinkMessageKind::DataLoss,
            "loss-telemetry-2-1-3",
            payload,
            TimestampMs::new(20),
            None,
        )
        .expect_err("an invalid data-loss range must not be sealed");

        assert_eq!(error.failure_code(), "DATA_LOSS_RANGE_INVALID", "{member}");
    }
}
