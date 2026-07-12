//! JWT authentication adapter for application-facing transports.

use std::sync::Arc;

use aether_application::{Actor, RequestContext};
use aether_domain::TimestampMs;
use jsonwebtoken::{Algorithm, DecodingKey, Validation, decode};
use serde::Deserialize;
use thiserror::Error;

const MIN_SECRET_BYTES: usize = 32;

#[derive(Debug, Deserialize)]
struct AccessClaims {
    user_id: i64,
    role: Option<String>,
    #[serde(rename = "type")]
    token_type: String,
    exp: usize,
    iat: usize,
}

/// Verifies access JWTs issued by Aether's gateway authentication API.
///
/// The secret is deliberately retained behind an `Arc<str>` and this type
/// does not implement `Debug`, so diagnostics cannot accidentally reveal it.
#[derive(Clone)]
pub struct AccessTokenAuthenticator {
    secret: Arc<str>,
}

impl AccessTokenAuthenticator {
    /// Creates an authenticator after enforcing the deployment secret policy.
    pub fn new(secret: &str) -> Result<Self, AuthenticationError> {
        validate_secret(secret)?;
        Ok(Self {
            secret: Arc::from(secret),
        })
    }

    /// Loads `JWT_SECRET_KEY` from the current process environment.
    pub fn from_env() -> Result<Self, AuthenticationError> {
        let secret = std::env::var("JWT_SECRET_KEY")
            .map_err(|_| AuthenticationError::Configuration("JWT_SECRET_KEY is required"))?;
        Self::new(&secret)
    }

    /// Authenticates an HTTP `Authorization` value containing one Bearer JWT.
    pub fn authenticate(&self, authorization: &str) -> Result<Actor, AuthenticationError> {
        if authorization.trim() != authorization {
            return Err(AuthenticationError::InvalidCredentials);
        }
        let (scheme, credential) = authorization
            .split_once(' ')
            .ok_or(AuthenticationError::InvalidCredentials)?;
        if !scheme.eq_ignore_ascii_case("Bearer")
            || credential.is_empty()
            || credential.bytes().any(|byte| byte.is_ascii_whitespace())
        {
            return Err(AuthenticationError::InvalidCredentials);
        }

        let mut validation = Validation::new(Algorithm::HS256);
        validation.validate_exp = true;
        validation.set_required_spec_claims(&["exp", "iat", "type", "user_id"]);
        let claims = decode::<AccessClaims>(
            credential,
            &DecodingKey::from_secret(self.secret.as_bytes()),
            &validation,
        )
        .map_err(|_| AuthenticationError::InvalidCredentials)?
        .claims;
        if claims.token_type != "access" || claims.user_id <= 0 || claims.iat > claims.exp {
            return Err(AuthenticationError::InvalidCredentials);
        }

        Ok(actor_for_role(
            &format!("user:{}", claims.user_id),
            claims.role.as_deref(),
        ))
    }

    /// Builds an auditable application invocation from transport values.
    ///
    /// Missing or invalid credentials deliberately become an unprivileged
    /// actor instead of bypassing the application layer. This lets mandatory
    /// audit record the rejected command attempt without trusting caller-
    /// supplied identity headers.
    #[must_use]
    pub fn invocation(
        &self,
        authorization: Option<&str>,
        request_id: Option<&str>,
        confirmed: bool,
        timestamp: TimestampMs,
    ) -> AuthenticatedInvocation {
        let actor = authorization
            .and_then(|value| self.authenticate(value).ok())
            .unwrap_or_else(|| Actor::new("unauthenticated"));
        let request_id = request_id
            .and_then(|value| uuid::Uuid::parse_str(value).ok())
            .unwrap_or_else(uuid::Uuid::new_v4);
        AuthenticatedInvocation {
            context: RequestContext::new(request_id.to_string(), actor, confirmed, timestamp),
        }
    }
}

/// Authentication failures intentionally do not reveal token-verification details.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum AuthenticationError {
    /// Credential syntax, signature, expiry, claims, or token type was invalid.
    #[error("invalid access-token credentials")]
    InvalidCredentials,
    /// Authentication cannot be initialized safely.
    #[error("invalid access-token configuration: {0}")]
    Configuration(&'static str),
}

/// Authenticated context ready for a transport-neutral application command.
pub struct AuthenticatedInvocation {
    context: RequestContext,
}

impl AuthenticatedInvocation {
    /// Returns the application request context.
    #[must_use]
    pub const fn context(&self) -> &RequestContext {
        &self.context
    }
}

fn actor_for_role(actor_id: &str, role: Option<&str>) -> Actor {
    let actor = Actor::new(actor_id);
    if matches!(role, Some("Admin" | "Engineer")) {
        actor
            .with_permission("device.control")
            .with_permission("automation.rule.execute")
            .with_permission("automation.rule.manage")
            .with_permission("automation.routing.manage")
            .with_permission("io.channel.manage")
            .with_permission("alarm.rule.manage")
            .with_permission("alarm.alert.resolve")
    } else {
        actor
    }
}

fn validate_secret(secret: &str) -> Result<(), AuthenticationError> {
    if secret.len() < MIN_SECRET_BYTES || secret.trim() != secret {
        return Err(AuthenticationError::Configuration(
            "JWT_SECRET_KEY must contain at least 32 bytes without surrounding whitespace",
        ));
    }
    Ok(())
}
