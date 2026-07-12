use aether_auth_jwt::{AccessTokenAuthenticator, AuthenticationError};
use aether_domain::TimestampMs;
use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};
use serde::Serialize;

const SECRET: &str = "test-only-jwt-secret-32-bytes-minimum";

#[derive(Serialize)]
struct Claims<'a> {
    user_id: i64,
    role: &'a str,
    #[serde(rename = "type")]
    token_type: &'a str,
    exp: usize,
    iat: usize,
}

fn token(role: &str, token_type: &str) -> String {
    encode(
        &Header::new(Algorithm::HS256),
        &Claims {
            user_id: 17,
            role,
            token_type,
            exp: 4_102_444_800,
            iat: 1,
        },
        &EncodingKey::from_secret(SECRET.as_bytes()),
    )
    .expect("encode test access token")
}

#[test]
fn administrative_access_tokens_receive_the_shared_command_permissions() {
    let authenticator = AccessTokenAuthenticator::new(SECRET).expect("valid secret");

    for role in ["Admin", "Engineer"] {
        let actor = authenticator
            .authenticate(&format!("Bearer {}", token(role, "access")))
            .expect("valid access token");
        assert_eq!(actor.id(), "user:17");
        for permission in [
            "device.control",
            "automation.rule.execute",
            "automation.rule.manage",
            "automation.routing.manage",
            "io.channel.manage",
            "alarm.rule.manage",
            "alarm.alert.resolve",
        ] {
            assert!(actor.has_permission(permission), "missing {permission}");
        }
    }
}

#[test]
fn viewer_refresh_and_malformed_credentials_never_gain_command_permissions() {
    let authenticator = AccessTokenAuthenticator::new(SECRET).expect("valid secret");
    let viewer = authenticator
        .authenticate(&format!("Bearer {}", token("Viewer", "access")))
        .expect("valid viewer access token");
    assert!(!viewer.has_permission("alarm.rule.manage"));
    assert!(!viewer.has_permission("automation.routing.manage"));
    assert!(!viewer.has_permission("io.channel.manage"));

    assert_eq!(
        authenticator.authenticate(&format!("Bearer {}", token("Admin", "refresh"))),
        Err(AuthenticationError::InvalidCredentials)
    );
    assert_eq!(
        authenticator.authenticate("Basic abc"),
        Err(AuthenticationError::InvalidCredentials)
    );
}

#[test]
fn unauthenticated_invocations_still_have_auditable_context_and_confirmation() {
    let authenticator = AccessTokenAuthenticator::new(SECRET).expect("valid secret");
    let request_id = "018f0000-0000-7000-8000-000000000017";
    let invocation = authenticator.invocation(
        None,
        Some(request_id),
        true,
        TimestampMs::new(1_720_000_000_000),
    );

    assert_eq!(invocation.context().request_id(), request_id);
    assert_eq!(invocation.context().actor().id(), "unauthenticated");
    assert!(invocation.context().confirmed());
}

#[test]
fn weak_or_whitespace_padded_secrets_fail_closed() {
    assert!(matches!(
        AccessTokenAuthenticator::new("short"),
        Err(AuthenticationError::Configuration(_))
    ));
    assert!(matches!(
        AccessTokenAuthenticator::new(" 012345678901234567890123456789012345 "),
        Err(AuthenticationError::Configuration(_))
    ));
}
