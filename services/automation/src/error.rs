//! Automation Error Types
//!
//! Domain-specific error handling for Model Service.
//!
//! Transport-neutral failures used by Automation application and adapters.

use thiserror::Error;

/// Automation Result type alias
pub type Result<T> = std::result::Result<T, AutomationError>;

/// Model Service errors with domain-specific semantics
///
/// Simplified to core error categories that callers can meaningfully handle.
#[derive(Error, Debug, Clone)]
pub enum AutomationError {
    // ============================================================================
    // Configuration Errors
    // ============================================================================
    #[error("Configuration error: {0}")]
    ConfigError(String),

    #[error("Invalid configuration: {0}")]
    InvalidConfig(String),

    #[error("Missing configuration: {0}")]
    MissingConfig(String),

    // ============================================================================
    // Database Errors
    // ============================================================================
    #[error("Database error: {0}")]
    DatabaseError(String),

    // ============================================================================
    // Instance Management Errors
    // ============================================================================
    #[error("Instance not found: {0}")]
    InstanceNotFound(String),

    #[error("Instance already exists: {0}")]
    InstanceExists(String),

    // ============================================================================
    // Rule Engine Errors
    // ============================================================================
    #[error("Rule not found: {0}")]
    RuleNotFound(String),

    #[error("Rule already exists: {0}")]
    RuleExists(String),

    #[error("Invalid rule: {0}")]
    InvalidRule(String),

    #[error("Parse error: {0}")]
    ParseError(String),

    #[error("Execution error: {0}")]
    ExecutionError(String),

    #[error("Scheduler error: {0}")]
    SchedulerError(String),

    // ============================================================================
    // Validation Errors
    // ============================================================================
    #[error("Invalid data: {0}")]
    InvalidData(String),

    #[error("Invalid routing: {0}")]
    InvalidRouting(String),

    /// The caller fenced a configuration mutation against a stale authority revision.
    #[error("Routing conflict: {0}")]
    RoutingConflict(String),

    /// The caller fenced an instance/configuration mutation against a stale
    /// aggregate revision, or attempted to remove an instance still used by
    /// logical routing.
    #[error("Instance configuration conflict: {0}")]
    ConfigurationConflict(String),

    /// Authenticated actor lacks the application permission required to issue
    /// a device command.
    #[error("Authorization denied: {0}")]
    AuthorizationDenied(String),

    /// A mandatory pre-execution audit could not be persisted, so execution did
    /// not begin. Terminal-audit degradation after an accepted operation is an
    /// explicit non-retryable acceptance outcome instead of this error.
    #[error("Command audit unavailable: {0}")]
    AuditUnavailable(String),

    // ============================================================================
    // Data Operation Errors
    // ============================================================================
    #[error("Serialization error: {0}")]
    SerializationError(String),

    // ============================================================================
    // Dispatch Errors
    // ============================================================================
    /// Dispatch path is degraded — SHM written but UDS notification failed,
    /// or SHM writer is unavailable (e.g. io restarted).
    /// Maps to HTTP 502: downstream service (io) is unreachable.
    #[error("Dispatch degraded: {0}")]
    DispatchDegraded(String),

    /// Target channel is offline — M2C control write rejected before reaching
    /// the device. Connectivity is read from the SHM health plane.
    /// Maps to HTTP 503: device is currently unreachable, retry may succeed
    /// after the channel comes back online.
    #[error("Channel {channel_id} unreachable: device is offline")]
    ChannelUnreachable { channel_id: u32 },

    // ============================================================================
    // Internal Errors
    // ============================================================================
    #[error("Internal error: {0}")]
    InternalError(String),
}

impl From<aether_application::ApplicationError> for AutomationError {
    fn from(error: aether_application::ApplicationError) -> Self {
        use aether_application::ApplicationError;
        use aether_ports::PortErrorKind;

        match error {
            ApplicationError::PermissionDenied { .. } => {
                Self::AuthorizationDenied(error.to_string())
            },
            ApplicationError::ConfirmationRequired { .. }
            | ApplicationError::InvalidCommand(_)
            | ApplicationError::InvalidChannelMutation(_) => Self::InvalidData(error.to_string()),
            ApplicationError::InvalidProcessingRequest(_)
            | ApplicationError::InputQualityRejected(_)
            | ApplicationError::ProcessingRequestTooLarge { .. } => {
                Self::InvalidData(error.to_string())
            },
            ApplicationError::InvalidProcessingConfiguration(_) => {
                Self::InvalidConfig(error.to_string())
            },
            ApplicationError::InvalidProcessorResult(_)
            | ApplicationError::ProcessingUnavailable { .. } => {
                Self::DispatchDegraded(error.to_string())
            },
            ApplicationError::ProcessingCodec(_) => Self::InternalError(error.to_string()),
            ApplicationError::AuditUnavailable(_) => Self::AuditUnavailable(error.to_string()),
            ApplicationError::HistoryQueryFailed(port_error)
            | ApplicationError::CovariateSourceFailed(port_error)
            | ApplicationError::ProcessorFailed(port_error)
            | ApplicationError::Port(port_error) => match port_error.kind() {
                PortErrorKind::Rejected | PortErrorKind::InvalidData => {
                    Self::InvalidData(port_error.to_string())
                },
                PortErrorKind::NotFound
                | PortErrorKind::Unavailable
                | PortErrorKind::Timeout
                | PortErrorKind::Conflict => Self::DispatchDegraded(port_error.to_string()),
                PortErrorKind::Permanent => Self::InternalError(port_error.to_string()),
            },
        }
    }
}

// ============================================================================
// Interoperability conversions
// ============================================================================

/// Convert from AetherError
impl From<errors::AetherError> for AutomationError {
    fn from(err: errors::AetherError) -> Self {
        use errors::AetherError as VE;
        match err {
            VE::Configuration(msg) => Self::ConfigError(msg),
            VE::InvalidConfig { field, reason } => {
                Self::InvalidConfig(format!("{}: {}", field, reason))
            },
            VE::MissingConfig(msg) => Self::MissingConfig(msg),
            VE::Database(msg) => Self::DatabaseError(msg),
            VE::Sqlite(e) => Self::DatabaseError(format!("SQLite: {}", e)),
            VE::Io(e) => Self::InternalError(format!("IO: {}", e)),
            VE::Timeout(d) => Self::InternalError(format!("Timeout: {:?}", d)),
            VE::Serialization(e) => Self::SerializationError(e),
            _ => Self::InternalError(err.to_string()),
        }
    }
}

/// Convert from SQLx Error
impl From<sqlx::Error> for AutomationError {
    fn from(err: sqlx::Error) -> Self {
        match err {
            sqlx::Error::RowNotFound => {
                Self::InstanceNotFound("Database row not found".to_string())
            },
            sqlx::Error::Database(e) => Self::DatabaseError(e.to_string()),
            _ => Self::DatabaseError(err.to_string()),
        }
    }
}

/// Convert from IO Error
impl From<std::io::Error> for AutomationError {
    fn from(err: std::io::Error) -> Self {
        Self::InternalError(format!("IO: {}", err))
    }
}

/// Convert from serde_json Error
impl From<serde_json::Error> for AutomationError {
    fn from(err: serde_json::Error) -> Self {
        Self::SerializationError(err.to_string())
    }
}

/// Convert from anyhow Error
impl From<anyhow::Error> for AutomationError {
    fn from(err: anyhow::Error) -> Self {
        Self::InternalError(err.to_string())
    }
}

/// Convert from aether_rules::RuleError
impl From<aether_rules::RuleError> for AutomationError {
    fn from(err: aether_rules::RuleError) -> Self {
        use aether_rules::RuleError as RE;
        match err {
            RE::NotFound(id) => Self::RuleNotFound(id),
            RE::AlreadyExists(id) => Self::RuleExists(id),
            RE::InvalidFormat(msg) => Self::InvalidRule(msg),
            RE::ParseError(msg) => Self::ParseError(msg),
            RE::ExecutionError(msg) => Self::ExecutionError(msg),
            RE::ConditionError(msg) => Self::ExecutionError(format!("Condition: {}", msg)),
            RE::ActionError(msg) => Self::ExecutionError(format!("Action: {}", msg)),
            RE::DatabaseError(msg) => Self::DatabaseError(msg),
            RE::SerializationError(msg) => Self::SerializationError(msg),
            RE::SchedulerError(msg) => Self::SchedulerError(msg),
            RE::RoutingError(msg) => Self::InternalError(format!("Routing: {}", msg)),
        }
    }
}
