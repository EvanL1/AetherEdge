//! Unified error handling for AetherEdge services
//!
//! This module provides a comprehensive error system that all services can use,
//! eliminating the need for service-specific error types.

use thiserror::Error;

// ============================================================================
// AetherError - Main error type
// ============================================================================

/// Main error type for all AetherEdge services
#[derive(Debug, Error)]
pub enum AetherError {
    // ======================================
    // Configuration Errors
    // ======================================
    #[error("Configuration error: {0}")]
    Configuration(String),

    #[error("Invalid configuration: {field}: {reason}")]
    InvalidConfig { field: String, reason: String },

    #[error("Missing required configuration: {0}")]
    MissingConfig(String),

    #[error("Configuration database not found at {path}. Run 'aether sync {service}' first")]
    DatabaseNotFound { path: String, service: String },

    // ======================================
    // Database Errors
    // ======================================
    #[error("Database error: {0}")]
    Database(String),

    #[error("SQLite error: {0}")]
    Sqlite(#[from] sqlx::Error),

    #[error("Query failed: {query}: {error}")]
    QueryFailed { query: String, error: String },

    // ======================================
    // Protocol & Communication Errors
    // ======================================
    #[error("Protocol error: {protocol}: {message}")]
    Protocol { protocol: String, message: String },

    #[error("Communication error: {0}")]
    Communication(String),

    #[error("Connection failed: {endpoint}: {reason}")]
    ConnectionFailed { endpoint: String, reason: String },

    #[error("Timeout waiting for response from {0}")]
    Timeout(String),

    #[error("Modbus error: {0}")]
    Modbus(String),

    // ======================================
    // Calculation & Processing Errors
    // ======================================
    #[error("Calculation error: {0}")]
    Calculation(String),

    #[error("Invalid expression: {expression}: {error}")]
    InvalidExpression { expression: String, error: String },

    #[error("Data type mismatch: expected {expected}, got {actual}")]
    TypeMismatch { expected: String, actual: String },

    #[error("Processing error: {0}")]
    Processing(String),

    // ======================================
    // API & HTTP Errors
    // ======================================
    #[error("Not found: {resource}")]
    NotFound { resource: String },

    #[error("Conflict: {resource} already exists")]
    Conflict { resource: String },

    #[error("Unauthorized: {0}")]
    Unauthorized(String),

    #[error("Forbidden: {0}")]
    Forbidden(String),

    // ======================================
    // Validation Errors
    // ======================================
    #[error("Validation failed: {0}")]
    Validation(String),

    #[error("Invalid parameter: {param}: {reason}")]
    InvalidParameter { param: String, reason: String },

    // ======================================
    // Resource & Instance Errors
    // ======================================
    #[error("Instance not found: {0}")]
    InstanceNotFound(String),

    #[error("Product not found: {0}")]
    ProductNotFound(String),

    #[error("Channel not found: {0}")]
    ChannelNotFound(String),

    #[error("Point not found: {point_type}:{point_id}")]
    PointNotFound { point_type: String, point_id: i32 },

    #[error("Rule not found: {0}")]
    RuleNotFound(String),

    #[error("Resource busy: {0}")]
    ResourceBusy(String),

    #[error("Resource already exists: {0}")]
    AlreadyExists(String),

    // ======================================
    // File & I/O Errors
    // ======================================
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Parse error: {file}: {error}")]
    ParseError { file: String, error: String },

    #[error("Serialization error: {0}")]
    Serialization(String),

    #[error("Deserialization error: {0}")]
    Deserialization(String),

    // ======================================
    // Service & Runtime Errors
    // ======================================
    #[error("Service unavailable: {0}")]
    ServiceUnavailable(String),

    #[error("Service startup failed: {0}")]
    StartupFailed(String),

    #[error("Shutdown error: {0}")]
    ShutdownError(String),

    #[error("Runtime error: {0}")]
    Runtime(String),

    #[error("Internal error: {0}")]
    Internal(String),

    // ======================================
    // External Service Errors
    // ======================================
    #[error("HTTP client error: {0}")]
    HttpClient(#[from] reqwest::Error),

    // ======================================
    // Catch-all for other errors
    // ======================================
    #[error("Unknown error: {0}")]
    Unknown(String),

    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

/// Result type alias using AetherError
pub type AetherResult<T> = Result<T, AetherError>;

impl AetherError {
    /// Check if this error is retryable
    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            Self::Timeout(_)
                | Self::ServiceUnavailable(_)
                | Self::ResourceBusy(_)
                | Self::ConnectionFailed { .. }
                | Self::Communication(_)
        )
    }
}

// Conversion traits for common error types
impl From<serde_json::Error> for AetherError {
    fn from(err: serde_json::Error) -> Self {
        Self::Serialization(err.to_string())
    }
}

impl From<serde_yml::Error> for AetherError {
    fn from(err: serde_yml::Error) -> Self {
        Self::Deserialization(err.to_string())
    }
}

impl From<std::num::ParseIntError> for AetherError {
    fn from(err: std::num::ParseIntError) -> Self {
        Self::Validation(format!("Invalid integer: {}", err))
    }
}

impl From<std::num::ParseFloatError> for AetherError {
    fn from(err: std::num::ParseFloatError) -> Self {
        Self::Validation(format!("Invalid float: {}", err))
    }
}

// Helper macros for creating errors
#[macro_export]
macro_rules! config_error {
    ($msg:expr_2021) => {
        $crate::AetherError::Configuration($msg.to_string())
    };
    ($fmt:expr_2021, $($arg:tt)*) => {
        $crate::AetherError::Configuration(format!($fmt, $($arg)*))
    };
}

#[macro_export]
macro_rules! validation_error {
    ($msg:expr_2021) => {
        $crate::AetherError::Validation($msg.to_string())
    };
    ($fmt:expr_2021, $($arg:tt)*) => {
        $crate::AetherError::Validation(format!($fmt, $($arg)*))
    };
}

#[macro_export]
macro_rules! protocol_error {
    ($protocol:expr_2021, $msg:expr_2021) => {
        $crate::AetherError::Protocol {
            protocol: $protocol.to_string(),
            message: $msg.to_string(),
        }
    };
}

// ============================================================================
// AetherError implements AetherErrorTrait
// ============================================================================

impl AetherErrorTrait for AetherError {
    fn error_code(&self) -> &'static str {
        match self {
            // Configuration Errors
            Self::Configuration(_) => "CONFIGURATION_ERROR",
            Self::InvalidConfig { .. } => "INVALID_CONFIG",
            Self::MissingConfig(_) => "MISSING_CONFIG",
            Self::DatabaseNotFound { .. } => "DATABASE_NOT_FOUND",

            // Database Errors
            Self::Database(_) => "DATABASE_ERROR",
            Self::Sqlite(_) => "SQLITE_ERROR",
            Self::QueryFailed { .. } => "QUERY_FAILED",

            // Protocol & Communication Errors
            Self::Protocol { .. } => "PROTOCOL_ERROR",
            Self::Communication(_) => "COMMUNICATION_ERROR",
            Self::ConnectionFailed { .. } => "CONNECTION_FAILED",
            Self::Timeout(_) => "TIMEOUT",
            Self::Modbus(_) => "MODBUS_ERROR",

            // Calculation & Processing
            Self::Calculation(_) => "CALCULATION_ERROR",
            Self::InvalidExpression { .. } => "INVALID_EXPRESSION",
            Self::TypeMismatch { .. } => "TYPE_MISMATCH",
            Self::Processing(_) => "PROCESSING_ERROR",

            // API & HTTP
            Self::NotFound { .. } => "NOT_FOUND",
            Self::Conflict { .. } => "CONFLICT",
            Self::Unauthorized(_) => "UNAUTHORIZED",
            Self::Forbidden(_) => "FORBIDDEN",

            // Validation
            Self::Validation(_) => "VALIDATION_ERROR",
            Self::InvalidParameter { .. } => "INVALID_PARAMETER",

            // Resources
            Self::InstanceNotFound(_) => "INSTANCE_NOT_FOUND",
            Self::ProductNotFound(_) => "PRODUCT_NOT_FOUND",
            Self::ChannelNotFound(_) => "CHANNEL_NOT_FOUND",
            Self::PointNotFound { .. } => "POINT_NOT_FOUND",
            Self::RuleNotFound(_) => "RULE_NOT_FOUND",
            Self::ResourceBusy(_) => "RESOURCE_BUSY",
            Self::AlreadyExists(_) => "ALREADY_EXISTS",

            // File & I/O
            Self::Io(_) => "IO_ERROR",
            Self::ParseError { .. } => "PARSE_ERROR",
            Self::Serialization(_) => "SERIALIZATION_ERROR",
            Self::Deserialization(_) => "DESERIALIZATION_ERROR",

            // Service & Runtime
            Self::ServiceUnavailable(_) => "SERVICE_UNAVAILABLE",
            Self::StartupFailed(_) => "STARTUP_FAILED",
            Self::ShutdownError(_) => "SHUTDOWN_ERROR",
            Self::Runtime(_) => "RUNTIME_ERROR",
            Self::Internal(_) => "INTERNAL_ERROR",

            // External Services
            Self::HttpClient(_) => "HTTP_CLIENT_ERROR",

            // Other
            Self::Unknown(_) => "UNKNOWN_ERROR",
            Self::Other(_) => "OTHER_ERROR",
        }
    }

    fn category(&self) -> ErrorCategory {
        match self {
            // Configuration -> Configuration
            Self::Configuration(_)
            | Self::InvalidConfig { .. }
            | Self::MissingConfig(_)
            | Self::DatabaseNotFound { .. } => ErrorCategory::Configuration,

            // Database -> Database
            Self::Database(_) | Self::Sqlite(_) | Self::QueryFailed { .. } => {
                ErrorCategory::Database
            },

            // Protocol -> Protocol
            Self::Protocol { .. } | Self::Modbus(_) => ErrorCategory::Protocol,

            // Connection -> Connection
            Self::ConnectionFailed { .. } => ErrorCategory::Connection,

            // Communication/Network -> Network
            Self::Communication(_) | Self::ServiceUnavailable(_) | Self::HttpClient(_) => {
                ErrorCategory::Network
            },

            // Timeout -> Timeout
            Self::Timeout(_) => ErrorCategory::Timeout,

            // Calculation -> Calculation
            Self::Calculation(_)
            | Self::InvalidExpression { .. }
            | Self::TypeMismatch { .. }
            | Self::Processing(_) => ErrorCategory::Calculation,

            // Validation -> Validation
            Self::Validation(_) | Self::InvalidParameter { .. } => ErrorCategory::Validation,

            // NotFound -> NotFound
            Self::NotFound { .. }
            | Self::InstanceNotFound(_)
            | Self::ProductNotFound(_)
            | Self::ChannelNotFound(_)
            | Self::PointNotFound { .. }
            | Self::RuleNotFound(_) => ErrorCategory::NotFound,

            // Conflict -> Conflict
            Self::Conflict { .. } | Self::AlreadyExists(_) => ErrorCategory::Conflict,

            // Permission -> Permission
            Self::Unauthorized(_) | Self::Forbidden(_) => ErrorCategory::Permission,

            // ResourceBusy -> ResourceBusy
            Self::ResourceBusy(_) => ErrorCategory::ResourceBusy,

            // Internal -> Internal
            Self::Internal(_)
            | Self::Runtime(_)
            | Self::StartupFailed(_)
            | Self::ShutdownError(_) => ErrorCategory::Internal,

            // Serialization/IO -> Internal
            Self::Io(_)
            | Self::ParseError { .. }
            | Self::Serialization(_)
            | Self::Deserialization(_) => ErrorCategory::Internal,

            // Unknown -> Unknown
            Self::Unknown(_) | Self::Other(_) => ErrorCategory::Unknown,
        }
    }
}

// ============================================================================
// AetherEdge Error Trait - Architectural layer
// ============================================================================

/// Error category enum - used for classification and metrics
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorCategory {
    // Infrastructure layer
    Configuration,
    Database,
    Network,
    Timeout,

    // Business logic layer
    Validation,
    NotFound,
    Conflict,
    Permission,

    // Protocol/communication layer (io-specific)
    Protocol,
    Connection,

    // Calculation layer (automation-specific)
    Calculation,

    // System level
    Internal,
    ResourceBusy,
    ResourceExhausted,

    // Others
    Unknown,
}

/// AetherEdge error capability trait
///
/// Defines a unified interface that all AetherEdge service error types should implement.
/// Each service can keep its own domain-specific error type (e.g., IoError) and gain a common
/// interface by implementing this trait.
///
/// # Design principles
///
/// 1. Domain preservation: keep service-specific error variants
/// 2. Unified interface: present a common outward-facing interface via the trait
/// 3. Sensible defaults: provide default behavior to reduce boilerplate
/// 4. Extensible: allow services to override defaults for special logic
pub trait AetherErrorTrait: std::error::Error + Send + Sync + 'static {
    /// Get error code (for API, logs, monitoring)
    fn error_code(&self) -> &'static str;

    /// Get error category (for classification/metrics)
    fn category(&self) -> ErrorCategory;

    /// Whether the error is retryable (default implementation is category-based)
    fn is_retryable(&self) -> bool {
        matches!(
            self.category(),
            ErrorCategory::Network | ErrorCategory::Timeout | ErrorCategory::ResourceBusy
        )
    }

    /// Get a suggestion for how to fix this error (default is category-based)
    fn suggestion(&self) -> Option<String> {
        match self.category() {
            ErrorCategory::Configuration => {
                Some("Check your configuration files and environment variables".to_string())
            },
            ErrorCategory::Database => {
                Some("Verify database connection and run 'aether doctor' to check system health".to_string())
            },
            ErrorCategory::Network => {
                Some("Check network connectivity and service availability".to_string())
            },
            ErrorCategory::Timeout => {
                Some("The operation timed out. Try again or increase timeout settings".to_string())
            },
            ErrorCategory::NotFound => None, // Specific not found suggestions should be provided by implementations
            ErrorCategory::Validation => None, // Validation errors should include specific field guidance
            ErrorCategory::Permission => {
                Some("Check your permissions and authentication credentials".to_string())
            },
            ErrorCategory::Conflict => {
                Some("The resource already exists. Use update instead of create, or choose a different identifier".to_string())
            },
            ErrorCategory::Protocol => {
                Some("Check device connection and protocol configuration".to_string())
            },
            ErrorCategory::Connection => {
                Some("Verify the target host is reachable and the port is correct".to_string())
            },
            ErrorCategory::ResourceBusy => {
                Some("The resource is currently in use. Wait and retry the operation".to_string())
            },
            ErrorCategory::ResourceExhausted => {
                Some("System resources are exhausted. Wait before retrying or scale up resources".to_string())
            },
            _ => None,
        }
    }
}

// Tests
#[cfg(test)]
#[allow(clippy::disallowed_methods)] // Test code - unwrap is acceptable
mod tests {
    use super::*;

    #[test]
    fn test_error_retryable() {
        assert!(AetherError::Timeout("test".into()).is_retryable());
        assert!(AetherError::ServiceUnavailable("test".into()).is_retryable());
        assert!(!AetherError::Validation("test".into()).is_retryable());
        assert!(
            !AetherError::NotFound {
                resource: "test".into()
            }
            .is_retryable()
        );
    }
}
