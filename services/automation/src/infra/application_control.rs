//! Application-layer adapters for authenticated device control.

use std::sync::Arc;

use aether_application::{
    Actor, ApplicationError, CompletionAuditStatus, ControlApplication, RequestContext,
};
use aether_auth_jwt::{
    AccessTokenAuthenticator, AuthenticationError as AccessTokenAuthenticationError,
};
use aether_domain::{
    ChannelCommandAddress, ChannelId, CommandConstraints, CommandId, ControlCommand,
    PhysicalDeviceCommand, PointId, PointKind, TimestampMs,
};
use aether_ports::{
    CommandDispatcher, CommandReceipt, CommandTopologyFence, DeviceCommandSink, PortError,
    PortErrorKind, PortResult,
};
use aether_rules::{RuleActionCommand, RuleActionCommandFacade};
use async_trait::async_trait;
use axum::http::HeaderMap;
use subtle::ConstantTimeEq;
use thiserror::Error;

use crate::instance_manager::InstanceManager;

const AUTHORIZATION_HEADER: &str = "authorization";
const REQUEST_ID_HEADER: &str = "x-request-id";
const MIN_SERVICE_TOKEN_BYTES: usize = 32;

/// Fixed identity commissioned for deterministic rule-engine device actions.
pub const COMMISSIONED_RULE_ACTOR_ID: &str = "local:aether-automation-rule-engine";

/// Verifies control callers at automation's HTTP trust boundary.
///
/// Browser/gateway and CLI callers present a signed access JWT. The uplink
/// presents a separate service credential and receives a fixed server-side
/// identity. Caller-provided actor or role headers are never consulted.
#[derive(Clone)]
pub struct ControlAuthenticator {
    access_tokens: AccessTokenAuthenticator,
    uplink_token: Option<Arc<str>>,
}

impl ControlAuthenticator {
    /// Creates an authenticator from already-resolved secrets.
    pub fn new(jwt_secret: &str, uplink_token: Option<&str>) -> Result<Self, AuthenticationError> {
        let access_tokens =
            AccessTokenAuthenticator::new(jwt_secret).map_err(map_access_token_error)?;
        if let Some(token) = uplink_token {
            validate_uplink_token(token)?;
        }
        Ok(Self {
            access_tokens,
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
            return self
                .access_tokens
                .authenticate(authorization)
                .map_err(map_access_token_error);
        }
        if scheme.eq_ignore_ascii_case("AetherService") {
            return self.authenticate_uplink(credential);
        }
        Err(AuthenticationError::InvalidCredentials)
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

fn validate_uplink_token(token: &str) -> Result<(), AuthenticationError> {
    if token.len() < MIN_SERVICE_TOKEN_BYTES || token.trim() != token {
        return Err(AuthenticationError::Configuration(
            "AETHER_UPLINK_CONTROL_TOKEN must contain at least 32 bytes without surrounding whitespace",
        ));
    }
    Ok(())
}

fn map_access_token_error(error: AccessTokenAuthenticationError) -> AuthenticationError {
    match error {
        AccessTokenAuthenticationError::InvalidCredentials => {
            AuthenticationError::InvalidCredentials
        },
        AccessTokenAuthenticationError::Configuration(message) => {
            AuthenticationError::Configuration(message)
        },
    }
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

/// Routes deterministic rule actions through the same governed application
/// use case as HTTP, CLI, and MCP device commands.
pub struct RuleActionApplication {
    application: Arc<ControlApplication>,
}

impl RuleActionApplication {
    /// Creates a facade over automation's shared control application.
    #[must_use]
    pub fn new(application: Arc<ControlApplication>) -> Self {
        Self { application }
    }
}

#[async_trait]
impl RuleActionCommandFacade for RuleActionApplication {
    async fn write_action(&self, command: RuleActionCommand) -> PortResult<CommandReceipt> {
        let request_uuid = uuid::Uuid::new_v4();
        let timestamp = TimestampMs::new(chrono::Utc::now().timestamp_millis().max(0) as u64);
        let actor = Actor::new(COMMISSIONED_RULE_ACTOR_ID).with_permission("device.control");
        let context = RequestContext::new(request_uuid.to_string(), actor, true, timestamp);

        let command_id = CommandId::new(request_uuid.as_u128());
        let acceptance = match command.topology_fence() {
            Some(fence) => {
                self.application
                    .write_point_fenced(
                        &context,
                        command_id,
                        command.target(),
                        command.value(),
                        fence,
                    )
                    .await
            },
            None => {
                self.application
                    .write_point(&context, command_id, command.target(), command.value())
                    .await
            },
        }
        .map_err(rule_action_port_error)?;
        if let Some(failure) = acceptance.completion_audit().failure() {
            tracing::error!(
                request_id = acceptance.request_id(),
                command_id = %format_args!("{:032x}", acceptance.command_id().get()),
                error = %failure,
                "rule command was accepted but its terminal audit is incomplete; do not retry"
            );
        }
        Ok(acceptance.into_receipt())
    }
}

/// Public HTTP representation of an accepted operation's terminal audit state.
///
/// The persistence error itself remains in server logs. Clients receive only a
/// stable non-retryable status so they cannot mistake an accepted command for a
/// safe retry opportunity.
pub(crate) fn completion_audit_response(status: &CompletionAuditStatus) -> serde_json::Value {
    match status {
        CompletionAuditStatus::Recorded => serde_json::json!({
            "status": "recorded",
            "retryable": false
        }),
        CompletionAuditStatus::Incomplete { .. } => serde_json::json!({
            "status": "incomplete",
            "retryable": false,
            "message": "operation was accepted but its terminal audit is incomplete; do not retry"
        }),
    }
}

fn rule_action_port_error(error: ApplicationError) -> PortError {
    match error {
        ApplicationError::AuditUnavailable(error) | ApplicationError::Port(error) => error,
        ApplicationError::InvalidCommand(error) => {
            PortError::new(PortErrorKind::InvalidData, error.to_string())
        },
        ApplicationError::PermissionDenied { .. }
        | ApplicationError::ConfirmationRequired { .. } => {
            PortError::new(PortErrorKind::Rejected, error.to_string())
        },
        _ => PortError::new(PortErrorKind::Permanent, error.to_string()),
    }
}

/// Resolves logical instance commands and delegates one typed physical command
/// to the configured device sink.
pub struct AutomationCommandDispatcher {
    manager: Arc<InstanceManager>,
    sink: Arc<dyn DeviceCommandSink>,
}

impl AutomationCommandDispatcher {
    /// Creates a logical dispatcher over routing/configuration and a physical
    /// command sink.
    #[must_use]
    pub fn new(manager: Arc<InstanceManager>, sink: Arc<dyn DeviceCommandSink>) -> Self {
        Self { manager, sink }
    }

    async fn dispatch_inner(
        &self,
        command: ControlCommand,
        fence: Option<CommandTopologyFence>,
    ) -> PortResult<CommandReceipt> {
        let logical = command.target();
        match logical.kind() {
            PointKind::Command | PointKind::Action => {},
            PointKind::Telemetry | PointKind::Status => {
                return Err(PortError::new(
                    PortErrorKind::Rejected,
                    "automation dispatcher accepts only writable instance points",
                ));
            },
        };
        // Pin routing and health from one service generation across the whole
        // command decision. The SQLite constraint read may await, but a later
        // topology publication cannot alter this retained immutable view.
        let topology = self.manager.runtime_topology.get().ok_or_else(|| {
            PortError::new(
                PortErrorKind::Unavailable,
                "coherent command topology is not configured",
            )
        })?;
        let runtime = Arc::clone(topology).pin_command().await;
        validate_command_topology_fence(fence, Some(runtime.generation().sequence()))?;
        let routed = runtime
            .generation()
            .action_route(logical.instance_id().get(), logical.point_id().get())
            .ok_or_else(|| {
                PortError::new(
                    PortErrorKind::Rejected,
                    format!("no enabled physical route for logical target {logical:?}"),
                )
            })?;
        if !routed.kind().is_writable() {
            return Err(PortError::new(
                PortErrorKind::Rejected,
                "logical command route resolved to a read-only physical point",
            ));
        }
        let (channel_id, physical_kind, physical_point_id) = (
            routed.channel_id().get(),
            routed.kind(),
            routed.point_id().get(),
        );

        let constraints = match physical_kind {
            PointKind::Command => {
                let exists = sqlx::query_scalar::<_, i64>(
                    "SELECT 1 FROM control_points WHERE channel_id = ? AND point_id = ?",
                )
                .bind(i64::from(channel_id))
                .bind(i64::from(physical_point_id))
                .fetch_optional(&self.manager.pool)
                .await
                .map_err(database_port_error)?;
                if exists.is_none() {
                    return Err(PortError::new(
                        PortErrorKind::Rejected,
                        format!(
                            "route target C:{}:{} has no configured command point",
                            channel_id, physical_point_id
                        ),
                    ));
                }
                CommandConstraints::unbounded()
            },
            PointKind::Action => {
                let limits = sqlx::query_as::<_, (Option<f64>, Option<f64>, f64)>(
                    "SELECT min_value, max_value, step FROM adjustment_points
                     WHERE channel_id = ? AND point_id = ?",
                )
                .bind(i64::from(channel_id))
                .bind(i64::from(physical_point_id))
                .fetch_optional(&self.manager.pool)
                .await
                .map_err(database_port_error)?
                .ok_or_else(|| {
                    PortError::new(
                        PortErrorKind::Rejected,
                        format!(
                            "route target A:{}:{} has no configured action point",
                            channel_id, physical_point_id
                        ),
                    )
                })?;
                CommandConstraints::new(limits.0, limits.1, Some(limits.2)).map_err(|error| {
                    PortError::new(
                        PortErrorKind::Rejected,
                        format!(
                            "invalid limits for A:{}:{}: {error}",
                            channel_id, physical_point_id
                        ),
                    )
                })?
            },
            PointKind::Telemetry | PointKind::Status => {
                return Err(PortError::new(
                    PortErrorKind::Rejected,
                    "logical command route resolved to a read-only physical point",
                ));
            },
        };

        let now = TimestampMs::new(chrono::Utc::now().timestamp_millis().max(0) as u64);
        command
            .validate_at(now, constraints)
            .map_err(|error| PortError::new(PortErrorKind::Rejected, error.to_string()))?;

        let health = runtime.generation().channel_health(channel_id)?;
        match health {
            Some(sample) if sample.online() => {},
            Some(_) => {
                return Err(PortError::new(
                    PortErrorKind::Unavailable,
                    format!("channel {channel_id} is offline"),
                ));
            },
            None => {
                return Err(PortError::new(
                    PortErrorKind::Unavailable,
                    format!("channel {channel_id} has no health sample"),
                ));
            },
        }

        let physical_target = ChannelCommandAddress::new(
            ChannelId::new(channel_id),
            physical_kind,
            PointId::new(physical_point_id),
        )
        .map_err(|error| PortError::new(PortErrorKind::Rejected, error.to_string()))?;
        let physical = PhysicalDeviceCommand::new(
            command.id(),
            physical_target,
            command.value(),
            command.issued_at(),
            command.expires_at(),
        )
        .map_err(|error| PortError::new(PortErrorKind::Rejected, error.to_string()))?;

        let receipt = self.sink.send(physical).await;
        drop(runtime);
        receipt
    }
}

#[async_trait]
impl CommandDispatcher for AutomationCommandDispatcher {
    async fn dispatch(&self, command: ControlCommand) -> PortResult<CommandReceipt> {
        self.dispatch_inner(command, None).await
    }

    async fn dispatch_fenced(
        &self,
        command: ControlCommand,
        fence: CommandTopologyFence,
    ) -> PortResult<CommandReceipt> {
        self.dispatch_inner(command, Some(fence)).await
    }
}

fn validate_command_topology_fence(
    fence: Option<CommandTopologyFence>,
    current_sequence: Option<u64>,
) -> PortResult<()> {
    let Some(fence) = fence else {
        return Ok(());
    };
    let current_sequence = current_sequence.ok_or_else(|| {
        PortError::new(
            PortErrorKind::Unavailable,
            "topology-fenced command cannot verify a runtime generation",
        )
    })?;
    if current_sequence != fence.expected_sequence() {
        return Err(PortError::new(
            PortErrorKind::Conflict,
            format!(
                "rule command topology changed: expected sequence {}, current sequence {current_sequence}",
                fence.expected_sequence()
            ),
        ));
    }
    Ok(())
}

fn database_port_error(error: sqlx::Error) -> PortError {
    PortError::new(
        PortErrorKind::Unavailable,
        format!("command configuration database unavailable: {error}"),
    )
}

#[cfg(test)]
mod tests {
    use super::validate_command_topology_fence;
    use aether_ports::{CommandTopologyFence, PortErrorKind};

    #[test]
    fn topology_fence_accepts_only_the_exact_pinned_sequence() {
        let fence = CommandTopologyFence::new(7);

        assert!(validate_command_topology_fence(Some(fence), Some(7)).is_ok());
        let error = validate_command_topology_fence(Some(fence), Some(8))
            .expect_err("cross-generation rule command must fail closed");
        assert_eq!(error.kind(), PortErrorKind::Conflict);
    }

    #[test]
    fn topology_fence_requires_a_runtime_generation_but_manual_dispatch_does_not() {
        let error = validate_command_topology_fence(Some(CommandTopologyFence::new(7)), None)
            .expect_err("fenced command without runtime topology must fail closed");
        assert_eq!(error.kind(), PortErrorKind::Unavailable);
        assert!(validate_command_topology_fence(None, None).is_ok());
    }
}
