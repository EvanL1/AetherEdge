use aether_application::{
    Actor, CapabilityDescriptor, MANAGE_HISTORY_CONFIG_CAPABILITY, RequestContext,
};
use aether_domain::TimestampMs;
use aether_ports::{AuditOutcome, AuditRecord, AuditSink, PortResult};
use axum::http::{HeaderMap, Method};
use uuid::Uuid;

use crate::auth::Claims;

/// A mutation the gateway audits because its service has no command boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct GatewayCommand {
    capability: CapabilityDescriptor,
    operation: &'static str,
}

impl GatewayCommand {
    pub(crate) const fn capability(self) -> CapabilityDescriptor {
        self.capability
    }

    pub(crate) const fn operation(self) -> &'static str {
        self.operation
    }
}

pub(crate) fn history_command(path: &str, method: &Method) -> GatewayCommand {
    let operation = match (method.as_str(), path) {
        ("PUT", "config") => "config.update",
        ("PUT", "storage") => "storage.update",
        ("POST", "storage/test") => "storage.test",
        ("POST", "storage/reconnect") => "storage.reconnect",
        _ => "unrecognized",
    };
    GatewayCommand {
        capability: MANAGE_HISTORY_CONFIG_CAPABILITY,
        operation,
    }
}

pub(crate) fn command_context(claims: &Claims, headers: &HeaderMap) -> RequestContext {
    let actor = aether_auth_jwt::permissions_for_role(claims.role.as_deref())
        .into_iter()
        .filter(|permission| aether_auth_jwt::scope_allows(&claims.scope, permission))
        .fold(
            Actor::new(format!("user:{}", claims.user_id)),
            Actor::with_permission,
        );
    let header = |name: &str| headers.get(name).and_then(|value| value.to_str().ok());
    let request_id = header("x-request-id")
        .and_then(|value| Uuid::parse_str(value).ok())
        .unwrap_or_else(Uuid::new_v4);
    let confirmed = header("x-aether-confirmed") == Some("true");
    let timestamp = u64::try_from(chrono::Utc::now().timestamp_millis()).unwrap_or_default();
    RequestContext::new(
        request_id.to_string(),
        actor,
        confirmed,
        TimestampMs::new(timestamp),
    )
}

pub(crate) struct CommandAudit<'a> {
    sink: &'a dyn AuditSink,
    context: &'a RequestContext,
    command: GatewayCommand,
}

impl<'a> CommandAudit<'a> {
    pub(crate) const fn new(
        sink: &'a dyn AuditSink,
        context: &'a RequestContext,
        command: GatewayCommand,
    ) -> Self {
        Self {
            sink,
            context,
            command,
        }
    }

    /// Records one outcome. The request body never reaches the audit trail.
    pub(crate) async fn record(
        &self,
        outcome: AuditOutcome,
        failure: Option<&str>,
    ) -> PortResult<()> {
        let operation = self.command.operation;
        let detail = failure.map_or_else(
            || format!("operation={operation}"),
            |failure| format!("operation={operation}; {failure}"),
        );
        self.sink
            .record(AuditRecord::new(
                self.context.request_id(),
                self.context.actor().id(),
                self.command.capability.name(),
                outcome,
                self.context.timestamp(),
                Some(detail),
            ))
            .await
    }
}
