//! Application-level errors exposed consistently by every transport.

use aether_domain::DomainError;
use aether_ports::PortError;
use thiserror::Error;

/// Failure returned by an Aether command or query.
#[derive(Debug, Error)]
pub enum ApplicationError {
    /// Actor lacks the permission required by the capability.
    #[error("capability {capability} requires permission {permission}")]
    PermissionDenied {
        /// Capability that was denied.
        capability: &'static str,
        /// Missing permission.
        permission: &'static str,
    },
    /// High-risk command lacks explicit confirmation.
    #[error("capability {capability} requires explicit confirmation")]
    ConfirmationRequired {
        /// Capability requiring confirmation.
        capability: &'static str,
    },
    /// Command violated a domain invariant.
    #[error("invalid command: {0}")]
    InvalidCommand(DomainError),
    /// An I/O channel mutation violated a transport-independent invariant.
    #[error("invalid channel mutation: {0}")]
    InvalidChannelMutation(String),
    /// A required audit event that gates execution could not be persisted.
    ///
    /// Terminal audit degradation after a successful non-idempotent operation
    /// is represented by `AcceptedOutcome`, never by this retryable failure.
    #[error("mandatory audit unavailable: {0}")]
    AuditUnavailable(PortError),
    /// A required port failed while executing the use case.
    #[error("port failure: {0}")]
    Port(PortError),
}

impl From<DomainError> for ApplicationError {
    fn from(error: DomainError) -> Self {
        Self::InvalidCommand(error)
    }
}
