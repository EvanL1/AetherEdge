use aether_integration_control::{ControlSession, IntegrationControlCodec, ReceiptDelivery};

#[test]
fn receipt_signing_projection_matches_the_frozen_cloudlink_authentication_profile() {
    let session = ControlSession::new(
        "33333333-3333-4333-8333-333333333333",
        "44444444-4444-4444-8444-444444444444",
        7,
        3,
    )
    .expect("session");
    let delivery = ReceiptDelivery::new(
        1,
        9,
        "integration-action-receipt:77777777-7777-4777-8777-777777777777",
        "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    )
    .expect("delivery");

    let bytes =
        IntegrationControlCodec::receipt_signing_bytes(&session, 1_784_217_600_500, &delivery)
            .expect("signing projection");
    let value: serde_json::Value = serde_json::from_slice(&bytes).expect("canonical JSON");

    assert_eq!(
        value,
        serde_json::json!({
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
            "position": "9",
            "batch_id": "integration-action-receipt:77777777-7777-4777-8777-777777777777",
            "business_digest": "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        })
    );
}
