//! Shared failure semantics for extension ports.

use std::error::Error;
use std::fmt;

/// Stable category used by runtime recovery policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PortErrorKind {
    /// A dependency is temporarily unavailable.
    Unavailable,
    /// An operation exceeded its deadline.
    Timeout,
    /// A requested commissioned resource does not exist.
    NotFound,
    /// A device or policy explicitly rejected an operation.
    Rejected,
    /// External data violated its contract.
    InvalidData,
    /// Concurrent state prevented the operation.
    Conflict,
    /// Retrying without configuration or code changes cannot succeed.
    Permanent,
}

/// Error returned by a capability port.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortError {
    kind: PortErrorKind,
    message: String,
}

impl PortError {
    /// Creates a port error with recovery semantics.
    pub fn new(kind: PortErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    /// Returns the stable error category.
    #[must_use]
    pub const fn kind(&self) -> PortErrorKind {
        self.kind
    }

    /// Returns the diagnostic message.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }

    /// Returns whether bounded retry is meaningful.
    #[must_use]
    pub const fn is_retryable(&self) -> bool {
        matches!(
            self.kind,
            PortErrorKind::Unavailable | PortErrorKind::Timeout | PortErrorKind::Conflict
        )
    }
}

impl fmt::Display for PortError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{:?}: {}", self.kind, self.message)
    }
}

impl Error for PortError {}

/// Result returned by an extension port.
pub type PortResult<T> = Result<T, PortError>;
