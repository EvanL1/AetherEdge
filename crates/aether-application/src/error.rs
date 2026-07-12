//! Application-level errors exposed consistently by every transport.

use aether_data_processing::CodecError;
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
    /// A caller request or assembled processing frame violated a domain invariant.
    #[error("invalid data-processing request: {0}")]
    InvalidProcessingRequest(DomainError),
    /// Assembled source data violated task-owned quality or semantic limits.
    #[error("data-processing input quality rejected: {0}")]
    InputQualityRejected(DomainError),
    /// The selected task, binding, processor, or limits cannot form a safe route.
    #[error("invalid data-processing configuration: {0}")]
    InvalidProcessingConfiguration(String),
    /// An I/O channel mutation violated a transport-independent invariant.
    #[error("invalid channel mutation: {0}")]
    InvalidChannelMutation(String),
    /// An untrusted processor response failed application validation.
    #[error("invalid processor result: {0}")]
    InvalidProcessorResult(String),
    /// A valid processing request completed without usable derived data.
    #[error("processing unavailable: {reason}")]
    ProcessingUnavailable {
        /// Stable processor reason code.
        reason: String,
        /// Whether retry after a bounded delay can succeed.
        retryable: bool,
        /// Optional retry delay in milliseconds.
        retry_after_ms: Option<u64>,
    },
    /// The bounded logical history source failed.
    #[error("history query failed: {0}")]
    HistoryQueryFailed(PortError),
    /// The task-declared covariate source failed.
    #[error("covariate source failed: {0}")]
    CovariateSourceFailed(PortError),
    /// The selected data processor failed before returning a typed result.
    #[error("data processor failed: {0}")]
    ProcessorFailed(PortError),
    /// Canonical processing request encoding or digest calculation failed.
    #[error("data-processing codec failed: {0}")]
    ProcessingCodec(#[source] CodecError),
    /// The exact v1 processor request exceeds the commissioned adapter limit.
    #[error("encoded processor request is {encoded_bytes} bytes; limit is {max_bytes} bytes")]
    ProcessingRequestTooLarge {
        /// Exact encoded request size.
        encoded_bytes: usize,
        /// Processor-advertised maximum size.
        max_bytes: usize,
    },
    /// A required audit event that gates execution could not be persisted.
    ///
    /// Terminal audit degradation after a successful non-idempotent operation
    /// is represented by `AcceptedOutcome`, never by this retryable failure.
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
