use std::sync::Arc;

use aether_application::{
    Actor, CapabilityDescriptor, MANAGE_HISTORY_CONFIG_CAPABILITY, MANAGE_UPLINK_CONFIG_CAPABILITY,
    PUBLISH_UPLINK_MESSAGE_CAPABILITY, RequestContext,
};
use aether_domain::TimestampMs;
use aether_ports::{AuditOutcome, AuditRecord, AuditSink, PortResult};
use axum::http::{HeaderMap, Method, StatusCode};
use tracing::error;
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

    #[cfg(test)]
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

pub(crate) fn uplink_command(path: &str, method: &Method) -> GatewayCommand {
    let config = MANAGE_UPLINK_CONFIG_CAPABILITY;
    let publish = PUBLISH_UPLINK_MESSAGE_CAPABILITY;
    let (capability, operation) = match (method.as_str(), path) {
        ("POST", "mqtt/config") => (config, "mqtt.config.update"),
        ("POST", "mqtt/disconnect") => (config, "mqtt.disconnect"),
        ("POST", "mqtt/reconnect") => (config, "mqtt.reconnect"),
        ("POST", "certificate/upload") => (config, "certificate.upload"),
        ("DELETE", path) if names_one_certificate(path) => (config, "certificate.delete"),
        ("POST", "alarm/broadcast") => (publish, "alarm.broadcast"),
        ("POST", "inst-sync") => (publish, "instance.sync"),
        // An unlisted route may change anything, so it needs the wider capability.
        _ => (config, "unrecognized"),
    };
    GatewayCommand {
        capability,
        operation,
    }
}

fn names_one_certificate(path: &str) -> bool {
    path.strip_prefix("certificate/")
        .is_some_and(|role| !role.is_empty() && !role.contains('/'))
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

pub(crate) struct CommandAudit {
    sink: Arc<dyn AuditSink>,
    context: RequestContext,
    command: GatewayCommand,
}

impl CommandAudit {
    pub(crate) const fn new(
        sink: Arc<dyn AuditSink>,
        context: RequestContext,
        command: GatewayCommand,
    ) -> Self {
        Self {
            sink,
            context,
            command,
        }
    }

    pub(crate) const fn context(&self) -> &RequestContext {
        &self.context
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

    /// Records how the forward ended and returns the `x-aether-audit` value.
    pub(crate) async fn record_terminal(&self, status: StatusCode) -> &'static str {
        let recorded = if status.is_success() {
            self.record(AuditOutcome::Succeeded, None).await
        } else {
            self.record(AuditOutcome::Failed, Some(&failure_detail(status)))
                .await
        };
        match recorded {
            Ok(()) => "recorded",
            Err(failure) => {
                error!(
                    request_id = self.context.request_id(),
                    operation = self.command.operation,
                    response_status = status.as_u16(),
                    "terminal command audit was not recorded: {failure}"
                );
                "incomplete"
            },
        }
    }
}

fn failure_detail(status: StatusCode) -> String {
    let code = status.as_u16();
    // 502 is the gateway's own answer when the forward breaks in transit, so
    // the service may or may not have applied the change.
    if status == StatusCode::BAD_GATEWAY {
        format!("response_status={code}; upstream_outcome=unknown")
    } else {
        format!("response_status={code}")
    }
}
