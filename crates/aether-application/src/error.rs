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
    /// A required audit event could not be persisted.
    #[error("mandatory audit unavailable: {0}")]
    AuditUnavailable(PortError),
    /// An extension port failed while executing the use case.
    #[error("extension failure: {0}")]
    Port(PortError),
}

impl From<DomainError> for ApplicationError {
    fn from(error: DomainError) -> Self {
        Self::InvalidCommand(error)
    }
}
