//! HTTP-only authentication and response projection.

use aether_application::{Actor, CompletionAuditStatus, RequestContext};
use aether_domain::{CommandId, TimestampMs};
use axum::http::HeaderMap;

use crate::infra::application_control::ControlAuthenticator;

const AUTHORIZATION_HEADER: &str = "authorization";
const REQUEST_ID_HEADER: &str = "x-request-id";

/// Authenticated HTTP invocation plus its binary command identifier.
pub struct CommandInvocation {
    context: RequestContext,
    command_id: CommandId,
}

impl CommandInvocation {
    #[must_use]
    pub const fn context(&self) -> &RequestContext {
        &self.context
    }

    #[must_use]
    pub const fn command_id(&self) -> CommandId {
        self.command_id
    }
}

/// Converts HTTP credentials into a transport-neutral application context.
pub fn command_invocation_from_headers(
    authenticator: &ControlAuthenticator,
    headers: &HeaderMap,
    confirmed: bool,
    timestamp: TimestampMs,
) -> CommandInvocation {
    let request_uuid = header_text(headers, REQUEST_ID_HEADER)
        .and_then(|value| uuid::Uuid::parse_str(value).ok())
        .unwrap_or_else(uuid::Uuid::new_v4);
    // Authentication failures still enter the application as a denied actor
    // so the mandatory audit sink records the rejected attempt.
    let actor = header_text(headers, AUTHORIZATION_HEADER)
        .and_then(|authorization| authenticator.authenticate(authorization).ok())
        .unwrap_or_else(|| Actor::new("unauthenticated"));

    CommandInvocation {
        context: RequestContext::new(request_uuid.to_string(), actor, confirmed, timestamp),
        command_id: CommandId::new(request_uuid.as_u128()),
    }
}

fn header_text<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name)?.to_str().ok().map(str::trim)
}

/// Stable HTTP representation of terminal audit state.
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
