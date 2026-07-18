use aether_cloudlink::{
    CandidateMessage, CloudLinkCodec, GatewaySessionAuthenticator, HeartbeatMessage,
    SessionBinding, TopologyBinding, UplinkAuthentication, UplinkSigningProjection,
};
use aether_domain::{
    InstanceId, PointAddress, PointId, PointKind, PointQuality, PointSample, TimestampMs,
};
use aether_ports::{CloudLinkMessageKind, CloudLinkRecord, CloudLinkRecordIdentity};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use ed25519_dalek::{Signature, SigningKey};
use serde_json::{Value, json};

const GATEWAY_ID: &str = "33333333-3333-4333-8333-333333333333";
const SESSION_ID: &str = "44444444-4444-4444-8444-444444444444";
const GATEWAY_KEY_ID: &str = "development-gateway-key-17";

fn session() -> SessionBinding {
    SessionBinding::new(GATEWAY_ID, SESSION_ID, 7, 3).expect("verified session")
}

fn gateway_authentication(key: &SigningKey) -> UplinkAuthentication {
    GatewaySessionAuthenticator::new(
        "development-cloud-key-1",
        SigningKey::from_bytes(&[7_u8; 32])
            .verifying_key()
            .to_bytes(),
        GATEWAY_KEY_ID,
        key.to_bytes(),
    )
    .expect("authenticator")
    .uplink_authentication()
}

fn telemetry_record() -> CloudLinkRecord {
    let sample = PointSample::new(
        PointAddress::new(InstanceId::new(42), PointKind::Telemetry, PointId::new(8)),
        12.5,
        TimestampMs::new(1_784_217_600_100),
        PointQuality::Good,
    );
    let payload = CloudLinkCodec::telemetry_batch(
        TopologyBinding::new(1, "fx64:0123456789abcdef").expect("topology"),
        &[sample],
    )
    .expect("telemetry");
    let enqueue = CloudLinkCodec::prepare(
        CloudLinkMessageKind::TelemetryBatch,
        "telemetry-1",
        &payload,
        TimestampMs::new(1_784_217_600_100),
        None,
    )
    .expect("enqueue");
    CloudLinkRecord::from_enqueue(CloudLinkRecordIdentity::new("telemetry", 1, 9), enqueue)
}

#[test]
fn real_contract_fixture_projects_exactly_thirteen_language_neutral_fields() {
    let fixture: Value = serde_json::from_slice(include_bytes!(
        "fixtures/action-receipt-provider-accepted.valid.json"
    ))
    .expect("frozen AetherContracts fixture");
    let delivery = &fixture["delivery"];
    let projection = UplinkSigningProjection::delivery(
        fixture["gateway_id"].as_str().expect("gateway"),
        fixture["credential_generation"]
            .as_str()
            .expect("generation"),
        fixture["session_id"].as_str().expect("session"),
        fixture["session_epoch"].as_str().expect("epoch"),
        fixture["message_kind"].as_str().expect("kind"),
        fixture["sent_at_ms"].as_str().expect("sent time"),
        fixture.get("expires_at_ms").and_then(Value::as_str),
        delivery["stream_id"].as_str().expect("stream"),
        delivery["stream_epoch"].as_str().expect("stream epoch"),
        delivery["position"].as_str().expect("position"),
        delivery["batch_id"].as_str().expect("batch"),
        delivery["digest"].as_str().expect("business digest"),
    )
    .expect("exact delivery projection");
    let bytes = projection.canonical_bytes().expect("JCS projection");

    assert_eq!(
        serde_json::from_slice::<Value>(&bytes).expect("projection JSON"),
        json!({
            "schema": "aether.cloudlink.uplink-signing.v1alpha1",
            "gateway_id": "33333333-3333-4333-8333-333333333333",
            "credential_generation": "3",
            "session_id": "44444444-4444-4444-8444-444444444444",
            "session_epoch": "7",
            "message_kind": "integration-action-receipt",
            "sent_at_ms": "1784217600500",
            "expires_at_ms": null,
            "stream_id": "integration-control-receipts",
            "stream_epoch": "1",
            "position": "1",
            "batch_id": "job-55555555-receipt-1",
            "business_digest": "sha256:f42bb6dfcd28ca27a7c1079569ffcd0f6144f741461cd362c3c679f471af80a7"
        })
    );
}

#[test]
fn gateway_signed_delivery_and_heartbeat_use_the_session_gateway_key() {
    let gateway_key = SigningKey::from_bytes(&[9_u8; 32]);
    let authentication = gateway_authentication(&gateway_key);
    let envelope =
        CloudLinkCodec::delivery_envelope(&session(), &telemetry_record(), None, &authentication)
            .expect("signed delivery");
    let encoded = CloudLinkCodec::encode(&envelope).expect("delivery JSON");
    let value: Value = serde_json::from_slice(&encoded).expect("delivery value");

    assert_eq!(value["message_authentication"]["key_id"], GATEWAY_KEY_ID);
    verify_wire_signature(&value, &gateway_key);

    let heartbeat = HeartbeatMessage::new(
        &session(),
        TimestampMs::new(1_784_217_600_600),
        Vec::new(),
        &authentication,
    )
    .expect("signed heartbeat");
    let heartbeat: Value =
        serde_json::from_slice(&CloudLinkCodec::encode(&heartbeat).expect("heartbeat JSON"))
            .expect("heartbeat value");
    assert_eq!(
        heartbeat["message_authentication"]["key_id"],
        GATEWAY_KEY_ID
    );
    verify_wire_signature(&heartbeat, &gateway_key);
}

#[test]
fn trusted_connector_has_no_payload_signature_and_acks_remain_unsigned() {
    let authentication = UplinkAuthentication::trusted_connector_broker_attestation();
    let delivery =
        CloudLinkCodec::delivery_envelope(&session(), &telemetry_record(), None, &authentication)
            .expect("trusted-connector delivery");
    let delivery = CloudLinkCodec::encode(&delivery).expect("delivery JSON");
    assert!(
        serde_json::from_slice::<Value>(&delivery)
            .expect("delivery value")
            .get("message_authentication")
            .is_none()
    );
    assert!(matches!(
        CloudLinkCodec::decode(&delivery).expect("strict unsigned delivery"),
        CandidateMessage::Delivery(_)
    ));

    let heartbeat = HeartbeatMessage::new(
        &session(),
        TimestampMs::new(1_784_217_600_600),
        Vec::new(),
        &authentication,
    )
    .expect("trusted-connector heartbeat");
    let heartbeat = CloudLinkCodec::encode(&heartbeat).expect("heartbeat JSON");
    assert!(
        serde_json::from_slice::<Value>(&heartbeat)
            .expect("heartbeat value")
            .get("message_authentication")
            .is_none()
    );

    let ack = HeartbeatMessage::ack(&session(), TimestampMs::new(1_784_217_600_601), Vec::new())
        .expect("heartbeat ACK");
    let ack: Value = serde_json::from_slice(&CloudLinkCodec::encode(&ack).expect("ACK JSON"))
        .expect("ACK value");
    assert!(ack.get("message_authentication").is_none());

    let mut authenticated_ack = ack;
    authenticated_ack["message_authentication"] = json!({
        "key_id": GATEWAY_KEY_ID,
        "algorithm": "Ed25519",
        "signature": URL_SAFE_NO_PAD.encode([0_u8; 64]),
    });
    assert!(
        CloudLinkCodec::decode(
            &serde_json::to_vec(&authenticated_ack).expect("authenticated ACK JSON")
        )
        .is_err(),
        "the frozen profile defines no heartbeat-ack signing projection"
    );

    let record = telemetry_record();
    let authenticated_durable_ack = json!({
        "schema": "aether.cloudlink.durable-ack.v1",
        "protocol": "aether.cloudlink",
        "protocol_version": "1.0",
        "message_kind": "durable-ack",
        "gateway_id": session().gateway_id(),
        "session_id": session().session_id(),
        "session_epoch": session().session_epoch().to_string(),
        "credential_generation": session().credential_generation().to_string(),
        "stream_id": record.identity().stream_id(),
        "stream_epoch": record.identity().stream_epoch().to_string(),
        "acknowledged_position": record.identity().position().to_string(),
        "batch_id": record.batch_id(),
        "digest": record.digest(),
        "receipt_id": "cloud-receipt-1",
        "acknowledged_at_ms": "1784217600700",
        "message_authentication": {
            "key_id": GATEWAY_KEY_ID,
            "algorithm": "Ed25519",
            "signature": URL_SAFE_NO_PAD.encode([0_u8; 64]),
        }
    });
    assert!(
        CloudLinkCodec::decode(
            &serde_json::to_vec(&authenticated_durable_ack)
                .expect("authenticated durable ACK JSON")
        )
        .is_err(),
        "the frozen durable ACK is closed and unsigned"
    );
}

#[test]
fn gateway_signer_emits_a_canonical_ed25519_base64url_signature() {
    let key = SigningKey::from_bytes(&[9_u8; 32]);
    let signature = signed_delivery_signature(
        &session(),
        &telemetry_record(),
        &gateway_authentication(&key),
    );
    let decoded = URL_SAFE_NO_PAD
        .decode(&signature)
        .expect("canonical base64url");
    assert_eq!(decoded.len(), 64);
    assert_eq!(URL_SAFE_NO_PAD.encode(decoded), signature);
}

#[test]
fn session_epoch_generation_and_gateway_key_changes_never_reuse_a_signature() {
    let key = SigningKey::from_bytes(&[9_u8; 32]);
    let authentication = gateway_authentication(&key);
    let record = telemetry_record();
    let baseline = signed_delivery_signature(&session(), &record, &authentication);
    let next_epoch = SessionBinding::new(GATEWAY_ID, "55555555-5555-4555-8555-555555555555", 8, 3)
        .expect("next epoch");
    let next_generation =
        SessionBinding::new(GATEWAY_ID, "66666666-6666-4666-8666-666666666666", 9, 4)
            .expect("next generation");
    let next_key = gateway_authentication(&SigningKey::from_bytes(&[10_u8; 32]));

    assert_ne!(
        baseline,
        signed_delivery_signature(&next_epoch, &record, &authentication)
    );
    assert_ne!(
        baseline,
        signed_delivery_signature(&next_generation, &record, &authentication)
    );
    assert_ne!(
        baseline,
        signed_delivery_signature(&session(), &record, &next_key)
    );
}

fn signed_delivery_signature(
    session: &SessionBinding,
    record: &CloudLinkRecord,
    authentication: &UplinkAuthentication,
) -> String {
    let envelope = CloudLinkCodec::delivery_envelope(session, record, None, authentication)
        .expect("signed envelope");
    envelope
        .message_authentication()
        .expect("Gateway signature")
        .signature()
        .to_owned()
}

fn verify_wire_signature(value: &Value, key: &SigningKey) {
    let delivery = value.get("delivery");
    let projection = match delivery {
        Some(delivery) => UplinkSigningProjection::delivery(
            value["gateway_id"].as_str().expect("gateway"),
            value["credential_generation"].as_str().expect("generation"),
            value["session_id"].as_str().expect("session"),
            value["session_epoch"].as_str().expect("epoch"),
            value["message_kind"].as_str().expect("kind"),
            value["sent_at_ms"].as_str().expect("sent time"),
            value.get("expires_at_ms").and_then(Value::as_str),
            delivery["stream_id"].as_str().expect("stream"),
            delivery["stream_epoch"].as_str().expect("stream epoch"),
            delivery["position"].as_str().expect("position"),
            delivery["batch_id"].as_str().expect("batch"),
            delivery["digest"].as_str().expect("business digest"),
        ),
        None => UplinkSigningProjection::heartbeat(
            value["gateway_id"].as_str().expect("gateway"),
            value["credential_generation"].as_str().expect("generation"),
            value["session_id"].as_str().expect("session"),
            value["session_epoch"].as_str().expect("epoch"),
            value["message_kind"].as_str().expect("kind"),
            value["observed_at_ms"].as_str().expect("observed time"),
        ),
    }
    .expect("signing projection");
    let encoded = value["message_authentication"]["signature"]
        .as_str()
        .expect("signature");
    let signature = Signature::from_slice(
        &URL_SAFE_NO_PAD
            .decode(encoded)
            .expect("base64url signature"),
    )
    .expect("Ed25519 signature");
    key.verifying_key()
        .verify_strict(
            &projection.canonical_bytes().expect("signing bytes"),
            &signature,
        )
        .expect("exact projection signature");
}
