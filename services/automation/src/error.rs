//! Automation Error Types
//!
//! Domain-specific error handling for Model Service.
//!
//! Simplified error types (15 variants) - reduced from 40+ to improve maintainability.
//! All errors map to ErrorCategory for HTTP status codes.

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

// ============================================================================
// AetherErrorTrait Implementation
// ============================================================================

impl errors::AetherErrorTrait for AutomationError {
    fn error_code(&self) -> &'static str {
        match self {
            // Configuration
            Self::ConfigError(_) => "AUTOMATION_CONFIG_ERROR",
            Self::InvalidConfig(_) => "AUTOMATION_INVALID_CONFIG",
            Self::MissingConfig(_) => "AUTOMATION_MISSING_CONFIG",

            // Database
            Self::DatabaseError(_) => "AUTOMATION_DATABASE_ERROR",

            // Instance
            Self::InstanceNotFound(_) => "AUTOMATION_INSTANCE_NOT_FOUND",
            Self::InstanceExists(_) => "AUTOMATION_INSTANCE_EXISTS",

            // Rule Engine
            Self::RuleNotFound(_) => "AUTOMATION_RULE_NOT_FOUND",
            Self::RuleExists(_) => "AUTOMATION_RULE_EXISTS",
            Self::InvalidRule(_) => "AUTOMATION_INVALID_RULE",
            Self::ParseError(_) => "AUTOMATION_PARSE_ERROR",
            Self::ExecutionError(_) => "AUTOMATION_EXECUTION_ERROR",
            Self::SchedulerError(_) => "AUTOMATION_SCHEDULER_ERROR",

            // Validation
            Self::InvalidData(_) => "AUTOMATION_INVALID_DATA",
            Self::InvalidRouting(_) => "AUTOMATION_INVALID_ROUTING",
            Self::RoutingConflict(_) => "AUTOMATION_ROUTING_CONFLICT",
            Self::ConfigurationConflict(_) => "AUTOMATION_CONFIGURATION_CONFLICT",
            Self::AuthorizationDenied(_) => "AUTOMATION_AUTHORIZATION_DENIED",
            Self::AuditUnavailable(_) => "AUTOMATION_AUDIT_UNAVAILABLE",

            // Data
            Self::SerializationError(_) => "AUTOMATION_SERIALIZATION_ERROR",

            // Dispatch
            Self::DispatchDegraded(_) => "AUTOMATION_DISPATCH_DEGRADED",
            Self::ChannelUnreachable { .. } => "AUTOMATION_CHANNEL_UNREACHABLE",

            // Internal
            Self::InternalError(_) => "AUTOMATION_INTERNAL_ERROR",
        }
    }

    fn category(&self) -> errors::ErrorCategory {
        use errors::ErrorCategory;

        match self {
            // Configuration → Configuration
            Self::ConfigError(_) | Self::InvalidConfig(_) | Self::MissingConfig(_) => {
                ErrorCategory::Configuration
            },

            // Database → Database
            Self::DatabaseError(_) => ErrorCategory::Database,

            // NotFound
            Self::InstanceNotFound(_) | Self::RuleNotFound(_) => ErrorCategory::NotFound,

            // Conflict
            Self::InstanceExists(_)
            | Self::RuleExists(_)
            | Self::RoutingConflict(_)
            | Self::ConfigurationConflict(_) => ErrorCategory::Conflict,

            // Validation
            Self::InvalidData(_)
            | Self::InvalidRouting(_)
            | Self::InvalidRule(_)
            | Self::ParseError(_) => ErrorCategory::Validation,

            Self::AuthorizationDenied(_) => ErrorCategory::Permission,
            Self::AuditUnavailable(_) => ErrorCategory::ResourceBusy,

            // Dispatch degraded → Network (HTTP 502: downstream io unreachable)
            Self::DispatchDegraded(_) => ErrorCategory::Network,

            // Channel offline → ResourceBusy (HTTP 503: temporarily unavailable, retry-able)
            Self::ChannelUnreachable { .. } => ErrorCategory::ResourceBusy,

            // Internal (execution, scheduling, serialization, etc.)
            Self::ExecutionError(_)
            | Self::SchedulerError(_)
            | Self::SerializationError(_)
            | Self::InternalError(_) => ErrorCategory::Internal,
        }
    }

    fn suggestion(&self) -> Option<String> {
        match self {
            Self::ConfigError(_) | Self::InvalidConfig(_) => Some(
                "Check aether-automation configuration in config/automation/ and run 'aether sync'".to_string()
            ),
            Self::MissingConfig(_) => Some(
                "Add the missing configuration to config/automation/ and run 'aether sync'".to_string()
            ),
            Self::DatabaseError(_) => Some(
                "Run 'aether doctor' to check database status. Try 'aether init' if database is missing".to_string()
            ),
            Self::InstanceNotFound(_) => Some(
                "Use GET /api/instances to list available instances, or create a new one with POST /api/instances".to_string()
            ),
            Self::InstanceExists(_) => Some(
                "Instance already exists. Use PUT /api/instances/{id} to update, or choose a different ID".to_string()
            ),
            Self::RuleNotFound(_) => Some(
                "Use GET /api/rules to list available rules, or create a new one with POST /api/rules".to_string()
            ),
            Self::RuleExists(_) => Some(
                "Rule already exists. Use PUT /api/rules/{id} to update, or choose a different ID".to_string()
            ),
            Self::InvalidRule(_) | Self::ParseError(_) => Some(
                "Check rule syntax. See docs/API_REFERENCE.md for rule format documentation".to_string()
            ),
            Self::InvalidRouting(_) => Some(
                "Verify routing configuration. Check that source and target channels/instances exist".to_string()
            ),
            Self::RoutingConflict(_) => Some(
                "Reload the current logical-routing revision and review the newer topology before submitting a fresh command".to_string()
            ),
            Self::ConfigurationConflict(_) => Some(
                "Reload the current instances revision and review routed descendants before submitting a fresh command".to_string()
            ),
            Self::AuthorizationDenied(_) => Some(
                "Use a signed Admin/Engineer session or the local aether CLI to issue device commands".to_string()
            ),
            Self::AuditUnavailable(_) => Some(
                "Check the local automation SQLite database; this request was not executed and may be submitted after audit persistence recovers".to_string()
            ),
            Self::ExecutionError(_) => Some(
                "Check rule conditions and actions. Use 'aether rules execute <id>' and inspect \
                 the local rule_history table for execution_path and error details"
                    .to_string()
            ),
            Self::SchedulerError(_) => Some(
                "Check scheduler status with GET /api/scheduler/status".to_string()
            ),
            _ => None,
        }
    }
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
// API Adaptation: AutomationError → AppError conversion
// ============================================================================

/// Automatically convert AutomationError to AppError using AetherErrorTrait for HTTP status mapping
impl From<AutomationError> for common::AppError {
    fn from(err: AutomationError) -> Self {
        use common::{AppError, ErrorInfo};
        use errors::AetherErrorTrait;

        let status = err.http_status();
        let mut error_info = ErrorInfo::new(err.to_string())
            .with_code(status.as_u16())
            .with_details(format!(
                "error_code: {}, category: {:?}, retryable: {}",
                err.error_code(),
                err.category(),
                err.is_retryable()
            ));

        // Add suggestion if available
        if let Some(suggestion) = err.suggestion() {
            error_info = error_info.with_suggestion(suggestion);
        }

        AppError::new(status, error_info)
    }
}

/// Implement IntoResponse so AutomationError can be returned directly from Axum handlers
impl axum::response::IntoResponse for AutomationError {
    fn into_response(self) -> axum::response::Response {
        let app_error: common::AppError = self.into();
        app_error.into_response()
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
