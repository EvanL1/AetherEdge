use aether_auth_jwt::{AccessTokenAuthenticator, AuthenticationError};
use aether_domain::TimestampMs;
use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};
use serde::Serialize;

const SECRET: &str = "test-only-jwt-secret-32-bytes-minimum";

#[derive(Serialize)]
struct Claims<'a> {
    user_id: i64,
    role: &'a str,
    scope: Vec<&'a str>,
    #[serde(rename = "type")]
    token_type: &'a str,
    exp: usize,
    iat: usize,
}

/// A token carrying everything its role may hold, which is what login issues.
fn token(role: &str, token_type: &str) -> String {
    encode(
        &Header::new(Algorithm::HS256),
        &Claims {
            user_id: 17,
            role,
            scope: aether_auth_jwt::permissions_for_role(Some(role)),
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
            aether_auth_jwt::DEVICE_READ,
            "device.control",
            "automation.rule.execute",
            "automation.rule.manage",
            "automation.routing.manage",
            "automation.instance.manage",
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
    assert!(!viewer.has_permission("automation.instance.manage"));
    assert!(!viewer.has_permission("io.channel.manage"));
    assert!(viewer.has_permission(aether_auth_jwt::DEVICE_READ));

    assert_eq!(
        authenticator.authenticate(&format!("Bearer {}", token("Admin", "refresh"))),
        Err(AuthenticationError::InvalidCredentials)
    );
    assert_eq!(
        authenticator.authenticate("Basic abc"),
        Err(AuthenticationError::InvalidCredentials)
    );
}

fn scoped_token(role: &str, scope: Vec<&str>) -> String {
    encode(
        &Header::new(Algorithm::HS256),
        &Claims {
            user_id: 17,
            role,
            scope,
            token_type: "access",
            exp: 4_102_444_800,
            iat: 1,
        },
        &EncodingKey::from_secret(SECRET.as_bytes()),
    )
    .expect("encode scoped access token")
}

/// A token shaped like one issued before scopes existed.
#[derive(Serialize)]
struct ScopelessClaims<'a> {
    user_id: i64,
    role: &'a str,
    #[serde(rename = "type")]
    token_type: &'a str,
    exp: usize,
    iat: usize,
}

#[test]
fn a_token_that_never_says_what_it_may_do_is_refused_outright() {
    // Reading an absent scope as "everything the role carries" would reinstate
    // the implicit grant this claim exists to remove, and would do it for
    // precisely the tokens that predate the check.
    let authenticator = AccessTokenAuthenticator::new(SECRET).expect("valid secret");
    let scopeless = encode(
        &Header::new(Algorithm::HS256),
        &ScopelessClaims {
            user_id: 17,
            role: "Admin",
            token_type: "access",
            exp: 4_102_444_800,
            iat: 1,
        },
        &EncodingKey::from_secret(SECRET.as_bytes()),
    )
    .expect("encode scopeless access token");

    assert_eq!(
        authenticator.authenticate(&format!("Bearer {scopeless}")),
        Err(AuthenticationError::InvalidCredentials)
    );
}

#[test]
fn a_scope_narrows_an_administrative_token_to_the_listed_permissions() {
    // "May read, and may resolve alarms, but may never control a device" is the
    // identity an AI assistant needs. Without scope the only choices were full
    // command authority or none, so every AI integration had to be handed a
    // token that could stop a channel.
    let authenticator = AccessTokenAuthenticator::new(SECRET).expect("valid secret");

    let actor = authenticator
        .authenticate(&format!(
            "Bearer {}",
            scoped_token("Admin", vec!["alarm.alert.resolve"])
        ))
        .expect("valid scoped access token");

    assert!(actor.has_permission("alarm.alert.resolve"));
    assert!(!actor.has_permission("device.control"));
    assert!(!actor.has_permission("io.channel.manage"));
}

#[test]
fn a_scope_never_grants_what_the_role_itself_lacks() {
    // Scope narrows; it must not become a self-service escalation path for a
    // token whose role was never trusted with command permissions.
    let authenticator = AccessTokenAuthenticator::new(SECRET).expect("valid secret");

    let actor = authenticator
        .authenticate(&format!(
            "Bearer {}",
            scoped_token("Viewer", vec!["device.control", "io.channel.manage"])
        ))
        .expect("valid scoped access token");

    assert!(!actor.has_permission("device.control"));
    assert!(!actor.has_permission("io.channel.manage"));
}

#[test]
fn device_read_is_shared_by_all_supported_roles_but_remains_scope_bounded() {
    let authenticator = AccessTokenAuthenticator::new(SECRET).expect("valid secret");

    for role in ["Admin", "Engineer", "Viewer"] {
        let actor = authenticator
            .authenticate(&format!("Bearer {}", token(role, "access")))
            .expect("valid role token");
        assert!(actor.has_permission(aether_auth_jwt::DEVICE_READ));
    }

    let narrowed = authenticator
        .authenticate(&format!(
            "Bearer {}",
            scoped_token("Viewer", vec![aether_auth_jwt::DATA_PROCESSING_READ])
        ))
        .expect("valid narrowed viewer token");
    assert!(!narrowed.has_permission(aether_auth_jwt::DEVICE_READ));
}

#[test]
fn an_empty_scope_is_a_read_only_token_rather_than_an_unrestricted_one() {
    let authenticator = AccessTokenAuthenticator::new(SECRET).expect("valid secret");

    let actor = authenticator
        .authenticate(&format!("Bearer {}", scoped_token("Admin", Vec::new())))
        .expect("valid scoped access token");

    for permission in [
        "device.control",
        "automation.rule.execute",
        "io.channel.manage",
        "alarm.alert.resolve",
    ] {
        assert!(!actor.has_permission(permission), "granted {permission}");
    }
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
