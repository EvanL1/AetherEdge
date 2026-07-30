use std::time::Duration;

use aether_cloud_enrollment_http::{
    HttpCloudEnrollmentClient, HttpCloudEnrollmentConfig, MAX_CLAIM_RESPONSE_BYTES,
};
use aether_ports::{
    CloudEndpointPolicy, CloudEnrollmentClaim, CloudEnrollmentClient, CloudEnrollmentClientError,
    EnrollmentIdempotencyKey, GatewayEnrollmentTarget, GatewayPublicKey, SecretMaterial,
};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use uuid::Uuid;
use wiremock::matchers::{body_json, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const TENANT_ID: &str = "11111111-1111-4111-8111-111111111111";
const PROJECT_ID: &str = "22222222-2222-4222-8222-222222222222";
const GATEWAY_ID: &str = "33333333-3333-4333-8333-333333333333";
const IDEMPOTENCY_KEY: &str = "44444444-4444-4444-8444-444444444444";
const TOKEN: &str = "opaque-enrollment-token-never-log";
const FINGERPRINT: &str = "66687aadf862bd776c8fc18b8e9f8e20089714856ee233b3902a591d0d5f2925";
const CLAIM_PATH: &str = "/api/v1/fleet/enrollment-claims:claim";

fn config(
    request_timeout: Duration,
    total_timeout: Duration,
    max_response_bytes: usize,
) -> HttpCloudEnrollmentConfig {
    HttpCloudEnrollmentConfig::new(
        Duration::from_secs(1),
        request_timeout,
        total_timeout,
        max_response_bytes,
    )
    .expect("valid HTTP configuration")
}

fn client() -> HttpCloudEnrollmentClient {
    HttpCloudEnrollmentClient::new(config(
        Duration::from_secs(2),
        Duration::from_secs(3),
        MAX_CLAIM_RESPONSE_BYTES,
    ))
    .expect("HTTP client")
}

fn claim(server: &MockServer) -> CloudEnrollmentClaim {
    claim_for_origin(&server.uri())
}

fn claim_for_origin(cloud_origin: &str) -> CloudEnrollmentClaim {
    let target = GatewayEnrollmentTarget::new(
        cloud_origin,
        TENANT_ID,
        PROJECT_ID,
        GATEWAY_ID,
        CloudEndpointPolicy::AllowLoopbackHttp,
    )
    .expect("loopback target");
    let idempotency_key =
        EnrollmentIdempotencyKey::new(Uuid::parse_str(IDEMPOTENCY_KEY).expect("idempotency UUID"))
            .expect("non-nil idempotency UUID");
    CloudEnrollmentClaim::new(
        target,
        GatewayPublicKey::from_bytes([0_u8; 32]),
        idempotency_key,
        SecretMaterial::new(TOKEN).expect("enrollment token"),
    )
}

fn claimed_response(revision: u64) -> Value {
    json!({
        "schema": "aether.cloud.gateway-enrollment-claimed.v1",
        "gatewayId": GATEWAY_ID,
        "state": "claimed",
        "revision": revision,
    })
}

#[tokio::test]
async fn sends_the_exact_claim_contract_and_decodes_claimed_receipt() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(CLAIM_PATH))
        .and(header("content-type", "application/json"))
        .and(header("accept", "application/json"))
        .and(header("idempotency-key", IDEMPOTENCY_KEY))
        .and(body_json(json!({
            "schema": "aether.cloud.gateway-enrollment-claim.v1",
            "tenantId": TENANT_ID,
            "projectId": PROJECT_ID,
            "gatewayId": GATEWAY_ID,
            "enrollmentToken": TOKEN,
            "credentialRequest": {
                "algorithm": "ed25519",
                "publicKey": "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
                "fingerprint": FINGERPRINT,
            },
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(claimed_response(7)))
        .expect(1)
        .mount(&server)
        .await;

    let receipt = client()
        .claim(claim(&server))
        .await
        .expect("Claim succeeds");

    assert_eq!(receipt.gateway_id(), GATEWAY_ID);
    assert_eq!(receipt.revision(), 7);
    let requests = server.received_requests().await.expect("received requests");
    assert_eq!(requests.len(), 1);
    assert!(!requests[0].headers.contains_key("authorization"));
}

#[tokio::test]
async fn response_contract_rejects_schema_identity_state_revision_and_unknown_fields() {
    let server = MockServer::start().await;
    let invalid_responses = [
        json!({
            "schema": "aether.cloud.gateway-enrollment-claimed.v2",
            "gatewayId": GATEWAY_ID,
            "state": "claimed",
            "revision": 1,
        }),
        json!({
            "schema": "aether.cloud.gateway-enrollment-claimed.v1",
            "gatewayId": "55555555-5555-4555-8555-555555555555",
            "state": "claimed",
            "revision": 1,
        }),
        json!({
            "schema": "aether.cloud.gateway-enrollment-claimed.v1",
            "gatewayId": GATEWAY_ID,
            "state": "credential-active",
            "revision": 1,
        }),
        json!({
            "schema": "aether.cloud.gateway-enrollment-claimed.v1",
            "gatewayId": GATEWAY_ID,
            "state": "claimed",
            "revision": 0,
        }),
        json!({
            "schema": "aether.cloud.gateway-enrollment-claimed.v1",
            "gatewayId": GATEWAY_ID,
            "state": "claimed",
            "revision": 9_007_199_254_740_992_u64,
        }),
        json!({
            "schema": "aether.cloud.gateway-enrollment-claimed.v1",
            "gatewayId": GATEWAY_ID,
            "state": "claimed",
            "revision": 1,
            "credential": "must-not-be-accepted",
        }),
    ];

    for invalid_response in invalid_responses {
        server.reset().await;
        Mock::given(method("POST"))
            .and(path(CLAIM_PATH))
            .respond_with(ResponseTemplate::new(200).set_body_json(invalid_response))
            .expect(1)
            .mount(&server)
            .await;

        let error = client()
            .claim(claim(&server))
            .await
            .expect_err("invalid response must fail closed");
        assert_eq!(error, CloudEnrollmentClientError::InvalidResponse);
    }
}

#[tokio::test]
async fn non_json_malformed_and_oversized_success_responses_fail_closed() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            serde_json::to_vec(&claimed_response(1)).expect("response JSON"),
            "text/plain",
        ))
        .expect(1)
        .mount(&server)
        .await;
    let error = client()
        .claim(claim(&server))
        .await
        .expect_err("non-JSON media type");
    assert_eq!(error, CloudEnrollmentClientError::InvalidResponse);

    server.reset().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "application/json")
                .set_body_bytes(b"{not-json".to_vec()),
        )
        .expect(1)
        .mount(&server)
        .await;
    let error = client()
        .claim(claim(&server))
        .await
        .expect_err("malformed JSON");
    assert_eq!(error, CloudEnrollmentClientError::InvalidResponse);

    server.reset().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "application/json")
                .set_body_bytes(vec![b'x'; 1_025]),
        )
        .expect(1)
        .mount(&server)
        .await;
    let bounded_client = HttpCloudEnrollmentClient::new(config(
        Duration::from_secs(2),
        Duration::from_secs(3),
        1_024,
    ))
    .expect("bounded client");
    let error = bounded_client
        .claim(claim(&server))
        .await
        .expect_err("oversized response");
    assert_eq!(error, CloudEnrollmentClientError::InvalidResponse);
}

#[tokio::test]
async fn chunked_response_without_content_length_is_still_bounded() {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("loopback listener");
    let address = listener.local_addr().expect("listener address");
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.expect("Claim connection");
        let mut request = vec![0_u8; 8 * 1024];
        let _ = stream.read(&mut request).await.expect("Claim request");
        stream
            .write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\
                  Transfer-Encoding: chunked\r\nConnection: close\r\n\r\n401\r\n",
            )
            .await
            .expect("response headers");
        stream
            .write_all(&vec![b'x'; 1_025])
            .await
            .expect("oversized chunk");
        stream
            .write_all(b"\r\n0\r\n\r\n")
            .await
            .expect("chunk terminator");
    });
    let bounded_client = HttpCloudEnrollmentClient::new(config(
        Duration::from_secs(2),
        Duration::from_secs(3),
        1_024,
    ))
    .expect("bounded client");

    let error = bounded_client
        .claim(claim_for_origin(&format!("http://{address}")))
        .await
        .expect_err("chunked response exceeds the streaming bound");

    assert_eq!(error, CloudEnrollmentClientError::InvalidResponse);
    server.await.expect("server task");
}

#[tokio::test]
async fn timeout_is_typed_and_safe_to_retry_with_the_same_claim_state() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(claimed_response(1))
                .set_delay(Duration::from_millis(250)),
        )
        .expect(1)
        .mount(&server)
        .await;
    let timeout_client = HttpCloudEnrollmentClient::new(config(
        Duration::from_millis(40),
        Duration::from_millis(80),
        MAX_CLAIM_RESPONSE_BYTES,
    ))
    .expect("timeout client");

    let error = timeout_client
        .claim(claim(&server))
        .await
        .expect_err("Claim times out");

    assert_eq!(error, CloudEnrollmentClientError::Timeout);
    assert!(error.is_retryable());
}

#[tokio::test]
async fn redirect_is_never_followed_even_across_origins() {
    let source = MockServer::start().await;
    let destination = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(CLAIM_PATH))
        .respond_with(
            ResponseTemplate::new(307)
                .insert_header("location", format!("{}{CLAIM_PATH}", destination.uri())),
        )
        .expect(1)
        .mount(&source)
        .await;
    Mock::given(method("POST"))
        .and(path(CLAIM_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_json(claimed_response(1)))
        .expect(0)
        .mount(&destination)
        .await;

    let error = client()
        .claim(claim(&source))
        .await
        .expect_err("redirect is rejected");

    assert_eq!(error, CloudEnrollmentClientError::InvalidResponse);
}

#[tokio::test]
async fn http_failures_are_typed_without_echoing_remote_bodies_or_secrets() {
    let server = MockServer::start().await;
    for (status, expected) in [
        (400, CloudEnrollmentClientError::Rejected),
        (409, CloudEnrollmentClientError::Conflict),
        (503, CloudEnrollmentClientError::Unavailable),
    ] {
        server.reset().await;
        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(status)
                    .insert_header("content-type", "application/json")
                    .set_body_string(format!(
                        "server echoed forbidden secret: {TOKEN}; Authorization: private"
                    )),
            )
            .expect(1)
            .mount(&server)
            .await;

        let error = client()
            .claim(claim(&server))
            .await
            .expect_err("HTTP failure");
        assert_eq!(error, expected);
        let rendered = format!("{error:?}: {error}");
        assert!(!rendered.contains(TOKEN));
        assert!(!rendered.contains("Authorization"));
        assert!(!rendered.contains("private"));
    }
}

#[test]
fn client_debug_contains_only_safe_limits() {
    let rendered = format!("{:?}", client());
    assert!(rendered.contains("HttpCloudEnrollmentClient"));
    assert!(rendered.contains("max_response_bytes"));
    assert!(!rendered.contains(TOKEN));
    assert!(!rendered.contains("Authorization"));
}
