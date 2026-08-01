//! Authenticated invocation context shared by every transport.

use std::collections::BTreeSet;

use aether_domain::{PointKind, TimestampMs};
use sha2::{Digest, Sha256};

/// Authenticated human, service, or AI actor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Actor {
    id: String,
    permissions: BTreeSet<String>,
}

impl Actor {
    /// Creates an actor with no permissions.
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            permissions: BTreeSet::new(),
        }
    }

    /// Adds one permission to an actor.
    #[must_use]
    pub fn with_permission(mut self, permission: impl Into<String>) -> Self {
        self.permissions.insert(permission.into());
        self
    }

    /// Returns the stable actor identifier.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Checks an exact permission.
    #[must_use]
    pub fn has_permission(&self, permission: &str) -> bool {
        self.permissions.contains(permission)
    }
}

/// Context attached to one command or query invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestContext {
    request_id: String,
    actor: Actor,
    confirmed: bool,
    timestamp: TimestampMs,
}

impl RequestContext {
    /// Creates an invocation context.
    pub fn new(
        request_id: impl Into<String>,
        actor: Actor,
        confirmed: bool,
        timestamp: TimestampMs,
    ) -> Self {
        Self {
            request_id: request_id.into(),
            actor,
            confirmed,
            timestamp,
        }
    }

    /// Returns the correlation identifier used for tracing and audit.
    ///
    /// It is not an idempotency key unless the selected capability explicitly
    /// declares and implements replay semantics.
    #[must_use]
    pub fn request_id(&self) -> &str {
        &self.request_id
    }

    /// Returns the authenticated actor.
    #[must_use]
    pub const fn actor(&self) -> &Actor {
        &self.actor
    }

    /// Returns whether the caller explicitly confirmed a high-risk operation.
    #[must_use]
    pub const fn confirmed(&self) -> bool {
        self.confirmed
    }

    /// Returns when the request was accepted by the interface.
    #[must_use]
    pub const fn timestamp(&self) -> TimestampMs {
        self.timestamp
    }
}

/// Renders the lowercase hexadecimal SHA-256 of an audit detail value.
pub(crate) fn digest(value: &str) -> String {
    format!("{:x}", Sha256::digest(value.as_bytes()))
}

/// Renders the audit-stable name of a point kind.
pub(crate) const fn point_kind_name(kind: PointKind) -> &'static str {
    match kind {
        PointKind::Telemetry => "telemetry",
        PointKind::Status => "status",
        PointKind::Command => "command",
        PointKind::Action => "action",
    }
}
