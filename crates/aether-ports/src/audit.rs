//! Auditing capability shared by every control-plane interface.

use aether_domain::TimestampMs;
use async_trait::async_trait;

use crate::PortResult;

/// Outcome of an audited capability invocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuditOutcome {
    /// Policy rejected the request before execution.
    Rejected,
    /// Policy accepted the request and execution is about to begin.
    Attempted,
    /// The operation completed successfully.
    Succeeded,
    /// Execution was allowed but failed.
    Failed,
}

/// Immutable audit event emitted by the application layer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuditRecord {
    request_id: String,
    actor_id: String,
    capability: String,
    outcome: AuditOutcome,
    timestamp: TimestampMs,
    detail: Option<String>,
}

impl AuditRecord {
    /// Creates an audit record.
    pub fn new(
        request_id: impl Into<String>,
        actor_id: impl Into<String>,
        capability: impl Into<String>,
        outcome: AuditOutcome,
        timestamp: TimestampMs,
        detail: Option<String>,
    ) -> Self {
        Self {
            request_id: request_id.into(),
            actor_id: actor_id.into(),
            capability: capability.into(),
            outcome,
            timestamp,
            detail,
        }
    }

    /// Returns the request identifier.
    #[must_use]
    pub fn request_id(&self) -> &str {
        &self.request_id
    }

    /// Returns the actor identifier.
    #[must_use]
    pub fn actor_id(&self) -> &str {
        &self.actor_id
    }

    /// Returns the capability name.
    #[must_use]
    pub fn capability(&self) -> &str {
        &self.capability
    }

    /// Returns the operation outcome.
    #[must_use]
    pub const fn outcome(&self) -> AuditOutcome {
        self.outcome
    }

    /// Returns when the event occurred.
    #[must_use]
    pub const fn timestamp(&self) -> TimestampMs {
        self.timestamp
    }

    /// Returns optional diagnostic detail.
    #[must_use]
    pub fn detail(&self) -> Option<&str> {
        self.detail.as_deref()
    }
}

/// Durable destination for security and control audit events.
#[async_trait]
pub trait AuditSink: Send + Sync + 'static {
    /// Records one event before the application reports completion.
    async fn record(&self, record: AuditRecord) -> PortResult<()>;
}
