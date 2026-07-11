//! Unified application-control adapter contracts.

use aether_domain::TimestampMs;
use aether_ports::{AuditOutcome, AuditRecord, AuditSink};
use aether_store_local::SqliteAuditSink;
use axum::http::{HeaderMap, HeaderValue};
use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};
use serde::Serialize;

use aether_automation::infra::application_control::{
    ControlAuthenticator, command_invocation_from_headers,
};

const JWT_SECRET: &str = "0123456789abcdef0123456789abcdef";
const UPLINK_TOKEN: &str = "abcdef0123456789abcdef0123456789";

#[derive(Serialize)]
struct AccessClaims<'a> {
    user_id: i64,
    username: &'a str,
    role: Option<&'a str>,
    exp: usize,
    iat: usize,
    #[serde(rename = "type")]
    token_type: &'a str,
}

fn access_token(role: &str, expires_in_seconds: i64) -> String {
    let now = chrono::Utc::now().timestamp();
    encode(
        &Header::new(Algorithm::HS256),
        &AccessClaims {
            user_id: 7,
            username: "operator",
            role: Some(role),
            exp: (now + expires_in_seconds) as usize,
            iat: now as usize,
            token_type: "access",
        },
        &EncodingKey::from_secret(JWT_SECRET.as_bytes()),
    )
    .expect("encode access token")
}

fn authenticator() -> ControlAuthenticator {
    ControlAuthenticator::new(JWT_SECRET, Some(UPLINK_TOKEN)).expect("valid test credentials")
}

#[tokio::test]
async fn sqlite_audit_sink_persists_ordered_security_events() {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .expect("open audit database");
    let audit = SqliteAuditSink::initialize(pool.clone())
        .await
        .expect("initialize audit sink");

    audit
        .record(AuditRecord::new(
            "018f0000-0000-7000-8000-000000000001",
            "user:7",
            "device.write_point",
            AuditOutcome::Attempted,
            TimestampMs::new(2_000),
            None,
        ))
        .await
        .expect("persist attempted event");
    audit
        .record(AuditRecord::new(
            "018f0000-0000-7000-8000-000000000001",
            "user:7",
            "device.write_point",
            AuditOutcome::Succeeded,
            TimestampMs::new(2_000),
            None,
        ))
        .await
        .expect("persist succeeded event");

    let rows = sqlx::query_as::<_, (String, String, String, i64)>(
        "SELECT actor_id, capability, outcome, occurred_at_ms
         FROM command_audit_events ORDER BY id",
    )
    .fetch_all(&pool)
    .await
    .expect("read audit events");
    assert_eq!(
        rows,
        vec![
            (
                "user:7".to_string(),
                "device.write_point".to_string(),
                "attempted".to_string(),
                2_000,
            ),
            (
                "user:7".to_string(),
                "device.write_point".to_string(),
                "succeeded".to_string(),
                2_000,
            ),
        ]
    );
}

#[test]
fn forged_identity_headers_are_rejected() {
    let mut headers = HeaderMap::new();
    headers.insert(
        "x-aether-auth-source",
        HeaderValue::from_static("gateway-jwt"),
    );
    headers.insert("x-aether-actor-id", HeaderValue::from_static("user:7"));
    headers.insert("x-aether-actor-role", HeaderValue::from_static("Engineer"));
    headers.insert(
        "x-request-id",
        HeaderValue::from_static("018f0000-0000-7000-8000-000000000001"),
    );

    let invocation =
        command_invocation_from_headers(&authenticator(), &headers, true, TimestampMs::new(2_000));

    assert_eq!(invocation.context().actor().id(), "unauthenticated");
    assert!(
        !invocation
            .context()
            .actor()
            .has_permission("device.control")
    );
}

#[test]
fn signed_engineer_access_token_becomes_control_actor() {
    let mut headers = HeaderMap::new();
    let authorization = format!("Bearer {}", access_token("Engineer", 3_600));
    headers.insert(
        "authorization",
        HeaderValue::from_str(&authorization).expect("valid authorization header"),
    );
    headers.insert(
        "x-request-id",
        HeaderValue::from_static("018f0000-0000-7000-8000-000000000001"),
    );

    let invocation = command_invocation_from_headers(
        &authenticator(),
        &headers,
        true,
        TimestampMs::new(2_000_000),
    );

    assert_eq!(invocation.context().actor().id(), "user:7");
    assert!(
        invocation
            .context()
            .actor()
            .has_permission("device.control")
    );
    assert!(invocation.context().confirmed());
    assert_eq!(
        invocation.context().request_id(),
        "018f0000-0000-7000-8000-000000000001"
    );
}

#[test]
fn viewer_and_expired_access_tokens_cannot_control_devices() {
    let mut viewer_headers = HeaderMap::new();
    let viewer = format!("Bearer {}", access_token("Viewer", 3_600));
    viewer_headers.insert(
        "authorization",
        HeaderValue::from_str(&viewer).expect("valid authorization header"),
    );
    let viewer = command_invocation_from_headers(
        &authenticator(),
        &viewer_headers,
        true,
        TimestampMs::new(2_000_000),
    );
    assert!(!viewer.context().actor().has_permission("device.control"));

    let mut expired_headers = HeaderMap::new();
    let expired = format!("Bearer {}", access_token("Engineer", -60));
    expired_headers.insert(
        "authorization",
        HeaderValue::from_str(&expired).expect("valid authorization header"),
    );
    let expired = command_invocation_from_headers(
        &authenticator(),
        &expired_headers,
        true,
        TimestampMs::new(2_000_000),
    );
    assert_eq!(expired.context().actor().id(), "unauthenticated");
    assert!(!expired.context().actor().has_permission("device.control"));
}

#[test]
fn authenticated_uplink_gets_a_fixed_identity() {
    let mut headers = HeaderMap::new();
    headers.insert(
        "authorization",
        HeaderValue::from_static("AetherService abcdef0123456789abcdef0123456789"),
    );
    headers.insert("x-aether-actor-id", HeaderValue::from_static("user:forged"));
    headers.insert("x-aether-actor-role", HeaderValue::from_static("Admin"));

    let invocation =
        command_invocation_from_headers(&authenticator(), &headers, true, TimestampMs::new(2_000));

    assert_eq!(invocation.context().actor().id(), "local:aether-uplink");
    assert!(
        invocation
            .context()
            .actor()
            .has_permission("device.control")
    );
    assert!(invocation.context().confirmed());

    headers.insert(
        "authorization",
        HeaderValue::from_static("AetherService wrong-token"),
    );
    let invalid =
        command_invocation_from_headers(&authenticator(), &headers, true, TimestampMs::new(2_000));
    assert_eq!(invalid.context().actor().id(), "unauthenticated");
    assert!(!invalid.context().actor().has_permission("device.control"));
}
