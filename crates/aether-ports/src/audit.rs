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

/// Default page size applied when a filter does not request one.
const DEFAULT_AUDIT_PAGE: u32 = 100;

/// Largest page an audit reader will return in one call.
pub const MAX_AUDIT_PAGE: u32 = 1_000;

/// Selects which recorded events an audit read returns.
///
/// Every field narrows the result; an unset field does not constrain it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuditQueryFilter {
    request_id: Option<String>,
    actor_id: Option<String>,
    capability: Option<String>,
    outcome: Option<AuditOutcome>,
    since: Option<TimestampMs>,
    until: Option<TimestampMs>,
    limit: u32,
    offset: u32,
}

impl Default for AuditQueryFilter {
    fn default() -> Self {
        Self {
            request_id: None,
            actor_id: None,
            capability: None,
            outcome: None,
            since: None,
            until: None,
            limit: DEFAULT_AUDIT_PAGE,
            offset: 0,
        }
    }
}

impl AuditQueryFilter {
    /// Creates an unconstrained filter using the default page size.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Restricts the read to one request identifier.
    #[must_use]
    pub fn with_request_id(mut self, request_id: impl Into<String>) -> Self {
        self.request_id = Some(request_id.into());
        self
    }

    /// Restricts the read to one actor.
    #[must_use]
    pub fn with_actor_id(mut self, actor_id: impl Into<String>) -> Self {
        self.actor_id = Some(actor_id.into());
        self
    }

    /// Restricts the read to one capability.
    #[must_use]
    pub fn with_capability(mut self, capability: impl Into<String>) -> Self {
        self.capability = Some(capability.into());
        self
    }

    /// Restricts the read to one outcome.
    #[must_use]
    pub const fn with_outcome(mut self, outcome: AuditOutcome) -> Self {
        self.outcome = Some(outcome);
        self
    }

    /// Excludes events recorded before `since`, inclusive.
    #[must_use]
    pub const fn with_since(mut self, since: TimestampMs) -> Self {
        self.since = Some(since);
        self
    }

    /// Excludes events recorded after `until`, inclusive.
    #[must_use]
    pub const fn with_until(mut self, until: TimestampMs) -> Self {
        self.until = Some(until);
        self
    }

    /// Sets the page bounds, clamping the size to [`MAX_AUDIT_PAGE`].
    #[must_use]
    pub const fn with_page(mut self, limit: u32, offset: u32) -> Self {
        self.limit = if limit == 0 {
            DEFAULT_AUDIT_PAGE
        } else if limit > MAX_AUDIT_PAGE {
            MAX_AUDIT_PAGE
        } else {
            limit
        };
        self.offset = offset;
        self
    }

    /// Returns the request-identifier constraint.
    #[must_use]
    pub fn request_id(&self) -> Option<&str> {
        self.request_id.as_deref()
    }

    /// Returns the actor constraint.
    #[must_use]
    pub fn actor_id(&self) -> Option<&str> {
        self.actor_id.as_deref()
    }

    /// Returns the capability constraint.
    #[must_use]
    pub fn capability(&self) -> Option<&str> {
        self.capability.as_deref()
    }

    /// Returns the outcome constraint.
    #[must_use]
    pub const fn outcome(&self) -> Option<AuditOutcome> {
        self.outcome
    }

    /// Returns the inclusive lower time bound.
    #[must_use]
    pub const fn since(&self) -> Option<TimestampMs> {
        self.since
    }

    /// Returns the inclusive upper time bound.
    #[must_use]
    pub const fn until(&self) -> Option<TimestampMs> {
        self.until
    }

    /// Returns the page size.
    #[must_use]
    pub const fn limit(&self) -> u32 {
        self.limit
    }

    /// Returns the page offset.
    #[must_use]
    pub const fn offset(&self) -> u32 {
        self.offset
    }
}

/// Read access to recorded audit events.
///
/// Kept separate from [`AuditSink`] so a forwarding or in-memory sink is not
/// obliged to become a queryable store, and so the write path that every
/// command depends on stays as small as it is.
#[async_trait]
pub trait AuditQuery: Send + Sync + 'static {
    /// Reads matching events newest first, with the total number that matched.
    ///
    /// The total counts every match, not just the returned page, so a caller
    /// can tell whether more remain.
    async fn query(&self, filter: &AuditQueryFilter) -> PortResult<(Vec<AuditRecord>, u64)>;
}
