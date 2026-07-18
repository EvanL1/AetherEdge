use aether_cloudlink::{
    CandidateMessage, CloudLinkCodec, GatewaySessionAuthenticator, ResumeCursor, SessionChallenge,
    SessionChallengeRequest,
};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use ed25519_dalek::{Signature, Signer as _, SigningKey};
use serde_json::{Value, json};

const GATEWAY_ID: &str = "33333333-3333-4333-8333-333333333333";
const CHALLENGE_ID: &str = "22222222-2222-4222-8222-222222222222";
const CREDENTIAL_ID: &str = "development-binding-17";
const CLOUD_KEY_ID: &str = "development-cloud-key-1";
const GATEWAY_KEY_ID: &str = "development-gateway-key-17";
const CLIENT_NONCE: &str = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
const CLOUD_NONCE: &str = "CCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCC";

fn request() -> SessionChallengeRequest {
    SessionChallengeRequest::new(
        GATEWAY_ID,
        CREDENTIAL_ID,
        3,
        vec!["1.0".to_owned()],
        CLIENT_NONCE,
        vec![ResumeCursor::new("telemetry", 4, 18).expect("resume cursor")],
    )
    .expect("challenge request")
}

fn signed_challenge(key: &SigningKey, issued_at_ms: u64, expires_at_ms: u64) -> SessionChallenge {
    let signing_projection = json!({
        "schema": "aether.cloudlink.session-challenge-signing.v1alpha1",
        "gateway_id": GATEWAY_ID,
        "challenge_id": CHALLENGE_ID,
        "cloud_nonce": CLOUD_NONCE,
        "issued_at_ms": issued_at_ms.to_string(),
        "expires_at_ms": expires_at_ms.to_string(),
    });
    let signing_bytes =
        serde_json_canonicalizer::to_vec(&signing_projection).expect("canonical challenge");
    let signature = URL_SAFE_NO_PAD.encode(key.sign(&signing_bytes).to_bytes());
    let wire = json!({
        "schema": "aether.cloudlink.session-challenge.v1",
        "protocol": "aether.cloudlink",
        "message_kind": "session-challenge",
        "gateway_id": GATEWAY_ID,
        "challenge_id": CHALLENGE_ID,
        "cloud_nonce": CLOUD_NONCE,
        "issued_at_ms": issued_at_ms.to_string(),
        "expires_at_ms": expires_at_ms.to_string(),
        "cloud_signature": {
            "key_id": CLOUD_KEY_ID,
            "algorithm": "Ed25519",
            "signature": signature,
        },
    });
    match CloudLinkCodec::decode(&serde_json::to_vec(&wire).expect("challenge JSON"))
        .expect("signed challenge")
    {
        CandidateMessage::SessionChallenge(challenge) => challenge,
        other => panic!("unexpected candidate: {other:?}"),
    }
}

fn authenticator(cloud: &SigningKey, gateway: &SigningKey) -> GatewaySessionAuthenticator {
    GatewaySessionAuthenticator::new(
        CLOUD_KEY_ID,
        cloud.verifying_key().to_bytes(),
        GATEWAY_KEY_ID,
        gateway.to_bytes(),
    )
    .expect("session authenticator")
}

#[test]
fn challenge_request_matches_the_closed_contract_and_is_strictly_decoded() {
    let request = request();
    let encoded = CloudLinkCodec::encode(&request).expect("request JSON");
    let actual: Value = serde_json::from_slice(&encoded).expect("request value");
    assert_eq!(
        actual,
        json!({
            "schema": "aether.cloudlink.session-challenge-request.v1",
            "protocol": "aether.cloudlink",
            "message_kind": "session-challenge-request",
            "gateway_id": GATEWAY_ID,
            "credential_binding": {
                "credential_id": CREDENTIAL_ID,
                "generation": "3",
            },
            "offered_protocol_versions": ["1.0"],
            "client_nonce": CLIENT_NONCE,
            "resume": [{
                "stream_id": "telemetry",
                "stream_epoch": "4",
                "acknowledged_position": "18",
            }],
        })
    );
    assert!(matches!(
        CloudLinkCodec::decode(&encoded).expect("decode request"),
        CandidateMessage::SessionChallengeRequest(_)
    ));

    let mut unknown = actual;
    unknown["unexpected"] = json!(true);
    assert!(CloudLinkCodec::decode(&serde_json::to_vec(&unknown).expect("unknown JSON")).is_err());
}

#[test]
fn cloud_challenge_verification_uses_the_exact_jcs_projection_and_strict_expiry() {
    let cloud = SigningKey::from_bytes(&[7_u8; 32]);
    let gateway = SigningKey::from_bytes(&[9_u8; 32]);
    let authenticator = authenticator(&cloud, &gateway);
    let challenge = signed_challenge(&cloud, 1_721_000_000_000, 1_721_000_060_000);

    authenticator
        .verify_challenge(&challenge, GATEWAY_ID, 1_721_000_000_001)
        .expect("valid challenge");

    let boundary = authenticator
        .verify_challenge(&challenge, GATEWAY_ID, 1_721_000_060_000)
        .expect_err("now == expires_at_ms must fail");
    assert_eq!(boundary.failure_code(), "MESSAGE_EXPIRED");

    let mut altered: Value =
        serde_json::from_slice(&CloudLinkCodec::encode(&challenge).expect("challenge JSON"))
            .expect("challenge value");
    altered["expires_at_ms"] = json!("1721000060001");
    let altered = match CloudLinkCodec::decode(
        &serde_json::to_vec(&altered).expect("altered challenge JSON"),
    )
    .expect("structurally valid altered challenge")
    {
        CandidateMessage::SessionChallenge(challenge) => challenge,
        other => panic!("unexpected candidate: {other:?}"),
    };
    assert!(
        authenticator
            .verify_challenge(&altered, GATEWAY_ID, 1_721_000_000_001)
            .is_err(),
        "a field mutation must invalidate the Cloud signature"
    );
}

#[test]
fn gateway_hello_signs_the_exact_transcript_with_the_persisted_cloud_nonce() {
    let cloud = SigningKey::from_bytes(&[7_u8; 32]);
    let gateway = SigningKey::from_bytes(&[9_u8; 32]);
    let authenticator = authenticator(&cloud, &gateway);
    let challenge = signed_challenge(&cloud, 1_721_000_000_000, 1_721_000_060_000);
    let verified = authenticator
        .verify_challenge(&challenge, GATEWAY_ID, 1_721_000_000_001)
        .expect("verified challenge");

    let first = authenticator
        .sign_hello(&verified, &request())
        .expect("signed hello");
    let second = authenticator
        .sign_hello(&verified, &request())
        .expect("deterministic retry");
    let first_bytes = CloudLinkCodec::encode(&first).expect("first hello JSON");
    assert_eq!(
        first_bytes,
        CloudLinkCodec::encode(&second).expect("second hello JSON"),
        "retrying one challenge and request must reproduce the same hello"
    );

    let hello: Value = serde_json::from_slice(&first_bytes).expect("hello value");
    let encoded_signature = hello
        .pointer("/gateway_signature/signature")
        .and_then(Value::as_str)
        .expect("Gateway signature");
    let signature_bytes = URL_SAFE_NO_PAD
        .decode(encoded_signature)
        .expect("base64url signature");
    let signature = Signature::from_slice(&signature_bytes).expect("Ed25519 signature");
    let expected_projection = json!({
        "schema": "aether.cloudlink.session-establishment-signing.v1alpha1",
        "gateway_id": GATEWAY_ID,
        "credential_id": CREDENTIAL_ID,
        "credential_generation": "3",
        "gateway_key_id": GATEWAY_KEY_ID,
        "challenge_id": CHALLENGE_ID,
        "cloud_nonce": CLOUD_NONCE,
        "client_nonce": CLIENT_NONCE,
        "offered_protocol_versions": ["1.0"],
        "resume": [{
            "stream_id": "telemetry",
            "stream_epoch": "4",
            "acknowledged_position": "18",
        }],
    });
    let signing_bytes =
        serde_json_canonicalizer::to_vec(&expected_projection).expect("expected hello JCS");
    gateway
        .verifying_key()
        .verify_strict(&signing_bytes, &signature)
        .expect("Gateway signature covers the exact contract projection");

    assert_eq!(hello["challenge_id"], CHALLENGE_ID);
    assert_eq!(
        hello["credential_binding"]["origin_model"],
        "gateway-signed"
    );
    assert!(
        hello.get("cloud_nonce").is_none(),
        "cloud_nonce is signed but is not a session-hello wire field"
    );
}

#[test]
fn authentication_diagnostics_redact_the_complete_transcript() {
    let request = request();
    let diagnostics = format!("{request:?}");
    for sensitive in [GATEWAY_ID, CREDENTIAL_ID, CLIENT_NONCE] {
        assert!(!diagnostics.contains(sensitive));
    }

    let cloud = SigningKey::from_bytes(&[7_u8; 32]);
    let gateway = SigningKey::from_bytes(&[9_u8; 32]);
    let diagnostics = format!("{:?}", authenticator(&cloud, &gateway));
    for sensitive in [CLOUD_KEY_ID, GATEWAY_KEY_ID] {
        assert!(!diagnostics.contains(sensitive));
    }
}

#[test]
fn gateway_private_key_type_is_zeroized_on_drop() {
    fn assert_zeroize_on_drop<T: zeroize::ZeroizeOnDrop>() {}

    assert_zeroize_on_drop::<SigningKey>();
}

#[test]
fn configured_key_material_is_canonical_and_a_wrong_cloud_key_fails_closed() {
    let cloud = SigningKey::from_bytes(&[7_u8; 32]);
    let wrong_cloud = SigningKey::from_bytes(&[8_u8; 32]);
    let gateway = SigningKey::from_bytes(&[9_u8; 32]);
    let authenticator = GatewaySessionAuthenticator::from_base64url(
        CLOUD_KEY_ID,
        URL_SAFE_NO_PAD.encode(wrong_cloud.verifying_key().to_bytes()),
        GATEWAY_KEY_ID,
        URL_SAFE_NO_PAD.encode(gateway.to_bytes()),
    )
    .expect("canonical configured keys");
    let challenge = signed_challenge(&cloud, 1_721_000_000_000, 1_721_000_060_000);
    assert_eq!(
        authenticator
            .verify_challenge(&challenge, GATEWAY_ID, 1_721_000_000_001)
            .expect_err("wrong configured Cloud key must reject the challenge")
            .failure_code(),
        "AUTHENTICATION_INVALID"
    );

    assert_eq!(
        GatewaySessionAuthenticator::from_base64url(
            CLOUD_KEY_ID,
            "not-base64url".to_owned(),
            GATEWAY_KEY_ID,
            "also-invalid".to_owned(),
        )
        .expect_err("malformed key configuration")
        .failure_code(),
        "AUTHENTICATION_INVALID"
    );
}
