//! Application-layer adapters for authenticated device control.

use std::sync::Arc;

use aether_application::{Actor, RequestContext};
use aether_domain::{CommandId, ControlCommand, PointKind, TimestampMs};
use aether_ports::{CommandDispatcher, CommandReceipt, PortError, PortErrorKind, PortResult};
use async_trait::async_trait;
use axum::http::HeaderMap;
use jsonwebtoken::{Algorithm, DecodingKey, Validation, decode};
use serde::Deserialize;
use subtle::ConstantTimeEq;
use thiserror::Error;

use crate::error::AutomationError;
use crate::instance_manager::InstanceManager;

const AUTHORIZATION_HEADER: &str = "authorization";
const REQUEST_ID_HEADER: &str = "x-request-id";
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

/// Verifies control callers at automation's HTTP trust boundary.
///
/// Browser/gateway and CLI callers present a signed access JWT. The uplink
/// presents a separate service credential and receives a fixed server-side
/// identity. Caller-provided actor or role headers are never consulted.
#[derive(Clone)]
pub struct ControlAuthenticator {
    jwt_secret: Arc<str>,
    uplink_token: Option<Arc<str>>,
}

impl ControlAuthenticator {
    /// Creates an authenticator from already-resolved secrets.
    pub fn new(jwt_secret: &str, uplink_token: Option<&str>) -> Result<Self, AuthenticationError> {
        validate_secret("JWT_SECRET_KEY", jwt_secret)?;
        if let Some(token) = uplink_token {
            validate_secret("AETHER_UPLINK_CONTROL_TOKEN", token)?;
        }
        Ok(Self {
            jwt_secret: Arc::from(jwt_secret),
            uplink_token: uplink_token.map(Arc::from),
        })
    }

    /// Loads authentication material from the process environment.
    pub fn from_env() -> Result<Self, AuthenticationError> {
        let jwt_secret = std::env::var("JWT_SECRET_KEY")
            .map_err(|_| AuthenticationError::Configuration("JWT_SECRET_KEY is required"))?;
        let uplink_token = std::env::var("AETHER_UPLINK_CONTROL_TOKEN")
            .ok()
            .filter(|token| !token.trim().is_empty());
        Self::new(&jwt_secret, uplink_token.as_deref())
    }

    fn authenticate(&self, headers: &HeaderMap) -> Result<Actor, AuthenticationError> {
        let authorization = header_text(headers, AUTHORIZATION_HEADER)
            .ok_or(AuthenticationError::MissingCredentials)?;
        let (scheme, credential) = authorization
            .split_once(' ')
            .ok_or(AuthenticationError::InvalidCredentials)?;
        if credential.is_empty() || credential.bytes().any(|byte| byte.is_ascii_whitespace()) {
            return Err(AuthenticationError::InvalidCredentials);
        }

        if scheme.eq_ignore_ascii_case("Bearer") {
            return self.authenticate_access_token(credential);
        }
        if scheme.eq_ignore_ascii_case("AetherService") {
            return self.authenticate_uplink(credential);
        }
        Err(AuthenticationError::InvalidCredentials)
    }

    fn authenticate_access_token(&self, token: &str) -> Result<Actor, AuthenticationError> {
        let mut validation = Validation::new(Algorithm::HS256);
        validation.validate_exp = true;
        validation.set_required_spec_claims(&["exp", "iat", "type", "user_id"]);
        let claims = decode::<AccessClaims>(
            token,
            &DecodingKey::from_secret(self.jwt_secret.as_bytes()),
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

    fn authenticate_uplink(&self, token: &str) -> Result<Actor, AuthenticationError> {
        let expected = self
            .uplink_token
            .as_deref()
            .ok_or(AuthenticationError::InvalidCredentials)?;
        if token.as_bytes().ct_eq(expected.as_bytes()).unwrap_u8() != 1 {
            return Err(AuthenticationError::InvalidCredentials);
        }
        Ok(Actor::new("local:aether-uplink").with_permission("device.control"))
    }
}

/// Authentication failures deliberately avoid exposing which credential check
/// failed.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum AuthenticationError {
    #[error("control authentication credentials are required")]
    MissingCredentials,
    #[error("invalid control authentication credentials")]
    InvalidCredentials,
    #[error("invalid control authentication configuration: {0}")]
    Configuration(&'static str),
}

fn validate_secret(name: &'static str, secret: &str) -> Result<(), AuthenticationError> {
    if secret.len() < MIN_SECRET_BYTES || secret.trim() != secret {
        return Err(AuthenticationError::Configuration(match name {
            "JWT_SECRET_KEY" => {
                "JWT_SECRET_KEY must contain at least 32 bytes without surrounding whitespace"
            },
            _ => {
                "AETHER_UPLINK_CONTROL_TOKEN must contain at least 32 bytes without surrounding whitespace"
            },
        }));
    }
    Ok(())
}

/// Authenticated application invocation plus its binary command identifier.
pub struct CommandInvocation {
    context: RequestContext,
    command_id: CommandId,
}

impl CommandInvocation {
    /// Returns the transport-neutral application request context.
    #[must_use]
    pub const fn context(&self) -> &RequestContext {
        &self.context
    }

    /// Returns the command identifier derived from the request UUID.
    #[must_use]
    pub const fn command_id(&self) -> CommandId {
        self.command_id
    }
}

/// Converts authenticated transport credentials into an application context.
///
/// Identity and permissions are derived exclusively from a verified JWT or
/// configured service credential. `x-aether-actor-*` headers are ignored.
pub fn command_invocation_from_headers(
    authenticator: &ControlAuthenticator,
    headers: &HeaderMap,
    confirmed: bool,
    timestamp: TimestampMs,
) -> CommandInvocation {
    let request_uuid = header_text(headers, REQUEST_ID_HEADER)
        .and_then(|value| uuid::Uuid::parse_str(value).ok())
        .unwrap_or_else(uuid::Uuid::new_v4);
    // Authentication failures still enter ControlApplication as a denied
    // actor so the mandatory audit sink records the rejected command attempt.
    let actor = authenticator
        .authenticate(headers)
        .unwrap_or_else(|_| Actor::new("unauthenticated"));

    CommandInvocation {
        context: RequestContext::new(request_uuid.to_string(), actor, confirmed, timestamp),
        command_id: CommandId::new(request_uuid.as_u128()),
    }
}

fn header_text<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name)?.to_str().ok().map(str::trim)
}

fn actor_for_role(actor_id: &str, role: Option<&str>) -> Actor {
    let actor = Actor::new(actor_id);
    if matches!(role, Some("Admin" | "Engineer")) {
        actor.with_permission("device.control")
    } else {
        actor
    }
}

/// Bridges validated application commands into automation's SHM dispatcher.
pub struct AutomationCommandDispatcher {
    manager: Arc<InstanceManager>,
}

impl AutomationCommandDispatcher {
    /// Creates a dispatcher over the existing instance manager.
    #[must_use]
    pub fn new(manager: Arc<InstanceManager>) -> Self {
        Self { manager }
    }
}

#[async_trait]
impl CommandDispatcher for AutomationCommandDispatcher {
    async fn dispatch(&self, command: ControlCommand) -> PortResult<CommandReceipt> {
        let target = command.target();
        if target.kind() != PointKind::Action {
            return Err(PortError::new(
                PortErrorKind::Rejected,
                "automation dispatcher accepts only instance action points",
            ));
        }
        self.manager
            .execute_action(
                target.instance_id().get(),
                &target.point_id().get().to_string(),
                command.value(),
            )
            .await
            .map_err(automation_port_error)?;
        let completed_at = chrono::Utc::now().timestamp_millis().max(0) as u64;
        Ok(CommandReceipt::new(
            command.id(),
            TimestampMs::new(completed_at),
        ))
    }
}

fn automation_port_error(error: AutomationError) -> PortError {
    let kind = match &error {
        AutomationError::InvalidData(_) | AutomationError::InvalidRouting(_) => {
            PortErrorKind::Rejected
        },
        AutomationError::ChannelUnreachable { .. } | AutomationError::DispatchDegraded(_) => {
            PortErrorKind::Unavailable
        },
        AutomationError::DatabaseError(_) => PortErrorKind::Unavailable,
        _ => PortErrorKind::Permanent,
    };
    PortError::new(kind, error.to_string())
}
