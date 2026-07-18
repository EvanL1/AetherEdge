//! Stable failures at the governed Integration-control boundary.

use std::fmt;

/// Machine-readable Integration-control failure category.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntegrationControlErrorCode {
    /// The experimental extension was not explicitly enabled.
    Disabled,
    /// The bounded JSON message does not match the frozen closed contract.
    InvalidMessage,
    /// The offer is not bound to the current authenticated CloudLink session.
    SessionMismatch,
    /// The offer was issued in the future or has expired.
    Expired,
    /// The canonical intent digest differs from the signed digest.
    IntentDigestMismatch,
    /// The injected cloud-key verifier rejected the signed projection.
    SignatureRejected,
    /// The same job identity was reused for different business intent.
    DigestConflict,
    /// The job is already executing in this runtime.
    JobInProgress,
    /// A local dependency failed before a safe terminal receipt was persisted.
    DependencyUnavailable,
    /// The durable ledger could not complete the requested transition.
    LedgerFailure,
}

/// Redacted typed Integration-control failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntegrationControlError {
    code: IntegrationControlErrorCode,
    message: &'static str,
}

impl IntegrationControlError {
    pub(crate) const fn new(code: IntegrationControlErrorCode, message: &'static str) -> Self {
        Self { code, message }
    }

    /// Returns the stable failure category.
    #[must_use]
    pub const fn code(&self) -> IntegrationControlErrorCode {
        self.code
    }
}

impl fmt::Display for IntegrationControlError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message)
    }
}

impl std::error::Error for IntegrationControlError {}

pub(crate) type ControlResult<T> = Result<T, IntegrationControlError>;
