//! Error handling for Communication Service
//!
//! This module provides error type definitions and conversions for the Communication Service.
//! Error types have been consolidated from 27 variants to 15 for maintainability.

use errors::AetherError;
use thiserror::Error;

/// Communication Service Error Type (Simplified: 15 variants)
#[derive(Error, Debug, Clone)]
pub enum IoError {
    /// Configuration-related errors
    #[error("Configuration error: {0}")]
    ConfigError(String),

    /// Input/Output operation errors
    #[error("IO error: {0}")]
    IoError(String),

    /// Protocol communication errors (includes Modbus)
    #[error("Protocol error: {0}")]
    ProtocolError(String),

    /// Connection establishment and maintenance errors (includes NotConnected)
    #[error("Connection error: {0}")]
    ConnectionError(String),

    /// Data handling errors (serialization, parsing, conversion, validation)
    #[error("Data error: {0}")]
    DataError(String),

    /// Operation timeout errors
    #[error("Timeout error: {0}")]
    TimeoutError(String),

    /// Storage errors (SHM, SQLite)
    #[error("Storage error: {0}")]
    StorageError(String),

    /// Resource errors (exhaustion, busy)
    #[error("Resource error: {0}")]
    ResourceError(String),

    /// Channel errors (not found, exists, operation failed)
    #[error("Channel error: {0}")]
    ChannelError(String),

    /// Point errors (not found, table error)
    #[error("Point error: {0}")]
    PointError(String),

    /// Validation errors (invalid parameter, operation, not supported)
    #[error("Validation error: {0}")]
    ValidationError(String),

    /// Permission errors
    #[error("Permission error: {0}")]
    PermissionError(String),

    /// State and synchronization errors (lock, sync)
    #[error("State error: {0}")]
    StateError(String),

    /// Batch operation errors
    #[error("Batch error: {0}")]
    BatchError(String),

    /// Internal errors (unknown, API, general)
    #[error("Internal error: {0}")]
    InternalError(String),
}

/// Result type alias for Communication Service
pub type Result<T> = std::result::Result<T, IoError>;

impl IoError {
    pub fn config(msg: impl Into<String>) -> Self {
        IoError::ConfigError(msg.into())
    }

    pub fn io(msg: impl Into<String>) -> Self {
        IoError::IoError(msg.into())
    }

    pub fn protocol(msg: impl Into<String>) -> Self {
        IoError::ProtocolError(msg.into())
    }

    pub fn connection(msg: impl Into<String>) -> Self {
        IoError::ConnectionError(msg.into())
    }

    pub fn data(msg: impl Into<String>) -> Self {
        IoError::DataError(msg.into())
    }

    pub fn timeout(msg: impl Into<String>) -> Self {
        IoError::TimeoutError(msg.into())
    }

    pub fn storage(msg: impl Into<String>) -> Self {
        IoError::StorageError(msg.into())
    }

    pub fn resource(msg: impl Into<String>) -> Self {
        IoError::ResourceError(msg.into())
    }

    pub fn channel(msg: impl Into<String>) -> Self {
        IoError::ChannelError(msg.into())
    }

    pub fn point(msg: impl Into<String>) -> Self {
        IoError::PointError(msg.into())
    }

    pub fn validation(msg: impl Into<String>) -> Self {
        IoError::ValidationError(msg.into())
    }

    pub fn permission(msg: impl Into<String>) -> Self {
        IoError::PermissionError(msg.into())
    }

    pub fn state(msg: impl Into<String>) -> Self {
        IoError::StateError(msg.into())
    }

    pub fn batch(msg: impl Into<String>) -> Self {
        IoError::BatchError(msg.into())
    }

    pub fn internal(msg: impl Into<String>) -> Self {
        IoError::InternalError(msg.into())
    }

    // Convenience constructors for specific cases
    pub fn channel_not_found(id: impl std::fmt::Display) -> Self {
        IoError::ChannelError(format!("Channel not found: {}", id))
    }

    pub fn channel_exists(id: u32) -> Self {
        IoError::ChannelError(format!("Channel already exists: {}", id))
    }

    /// Invalid channel ID (out of bounds for pre-allocated Vec)
    pub fn invalid_channel_id(id: u32) -> Self {
        IoError::ChannelError(format!("Invalid channel ID: {} (must be < 10000)", id))
    }

    pub fn point_not_found(id: impl std::fmt::Display) -> Self {
        IoError::PointError(format!("Point not found: {}", id))
    }

    pub fn not_connected() -> Self {
        IoError::ConnectionError("Not connected".to_string())
    }
}

// ============================================================================
// From implementations for external error types
// ============================================================================

impl From<std::io::Error> for IoError {
    fn from(err: std::io::Error) -> Self {
        IoError::IoError(err.to_string())
    }
}

impl From<serde_json::Error> for IoError {
    fn from(err: serde_json::Error) -> Self {
        IoError::DataError(format!("JSON: {err}"))
    }
}

impl From<serde_yml::Error> for IoError {
    fn from(err: serde_yml::Error) -> Self {
        IoError::DataError(format!("YAML: {err}"))
    }
}

impl From<anyhow::Error> for IoError {
    fn from(err: anyhow::Error) -> Self {
        IoError::ConfigError(format!("Validation: {err}"))
    }
}

impl From<crate::protocols::GatewayError> for IoError {
    fn from(err: crate::protocols::GatewayError) -> Self {
        use crate::protocols::GatewayError;
        match err {
            // Connection errors
            GatewayError::Connection(msg) => IoError::ConnectionError(msg),
            GatewayError::NotConnected => IoError::ConnectionError("Not connected".into()),
            GatewayError::ConnectionTimeout(ms) => {
                IoError::TimeoutError(format!("Connection timeout: {}ms", ms))
            },
            GatewayError::ChannelClosed => IoError::ChannelError("Channel closed".into()),

            // Protocol errors
            GatewayError::Protocol(msg) => IoError::ProtocolError(msg),
            GatewayError::InvalidResponse(msg) => {
                IoError::ProtocolError(format!("Invalid response: {}", msg))
            },
            GatewayError::Modbus(msg) => IoError::ProtocolError(format!("Modbus: {}", msg)),
            GatewayError::Iec104(msg) => IoError::ProtocolError(format!("IEC 104: {}", msg)),
            GatewayError::Dnp3(msg) => IoError::ProtocolError(format!("DNP3: {}", msg)),
            GatewayError::OpcUa(msg) => IoError::ProtocolError(format!("OPC UA: {}", msg)),

            // Data errors
            GatewayError::InvalidData(msg) => IoError::DataError(msg),
            GatewayError::DataConversion(msg) => IoError::DataError(format!("Conversion: {}", msg)),
            GatewayError::PointNotFound(id) => IoError::PointError(format!("Not found: {}", id)),

            // Configuration errors
            GatewayError::Config(msg) => IoError::ConfigError(msg),
            GatewayError::InvalidAddress(msg) => {
                IoError::ConfigError(format!("Invalid address: {}", msg))
            },
            GatewayError::Unsupported(msg) => {
                IoError::ValidationError(format!("Unsupported: {}", msg))
            },

            // IO/Timeout errors
            GatewayError::Io(io_err) => IoError::IoError(io_err.to_string()),
            GatewayError::ReadTimeout => IoError::TimeoutError("Read timeout".into()),
            GatewayError::WriteTimeout => IoError::TimeoutError("Write timeout".into()),

            // Internal errors
            GatewayError::Internal(msg) => IoError::InternalError(msg),
        }
    }
}

// ============================================================================
// Extension trait for adding context to errors
// ============================================================================

/// Extension trait for adding context to errors
pub trait ErrorExt<T> {
    fn config_error(self, msg: &str) -> Result<T>;
    fn io_error(self, msg: &str) -> Result<T>;
    fn protocol_error(self, msg: &str) -> Result<T>;
    fn connection_error(self, msg: &str) -> Result<T>;
    fn data_error(self, msg: &str) -> Result<T>;
    fn context(self, msg: &str) -> Result<T>;
}

impl<T, E> ErrorExt<T> for std::result::Result<T, E>
where
    E: std::fmt::Display,
{
    fn config_error(self, msg: &str) -> Result<T> {
        self.map_err(|e| IoError::ConfigError(format!("{msg}: {e}")))
    }

    fn io_error(self, msg: &str) -> Result<T> {
        self.map_err(|e| IoError::IoError(format!("{msg}: {e}")))
    }

    fn protocol_error(self, msg: &str) -> Result<T> {
        self.map_err(|e| IoError::ProtocolError(format!("{msg}: {e}")))
    }

    fn connection_error(self, msg: &str) -> Result<T> {
        self.map_err(|e| IoError::ConnectionError(format!("{msg}: {e}")))
    }

    fn data_error(self, msg: &str) -> Result<T> {
        self.map_err(|e| IoError::DataError(format!("{msg}: {e}")))
    }

    fn context(self, msg: &str) -> Result<T> {
        self.map_err(|e| IoError::InternalError(format!("{msg}: {e}")))
    }
}

// ============================================================================
// Conversion from IoError to AetherError for API boundaries
// ============================================================================

impl From<IoError> for AetherError {
    fn from(err: IoError) -> Self {
        match err {
            IoError::ConfigError(msg) => AetherError::Configuration(msg),
            IoError::IoError(msg) => AetherError::Io(std::io::Error::other(msg)),
            IoError::ProtocolError(msg) => AetherError::Protocol {
                protocol: "io".to_string(),
                message: msg,
            },
            IoError::ConnectionError(msg) => AetherError::Communication(msg),
            IoError::DataError(msg) => AetherError::Validation(msg),
            IoError::TimeoutError(msg) => AetherError::Timeout(msg),
            IoError::StorageError(msg) => AetherError::Database(msg),
            IoError::ResourceError(msg) => AetherError::ResourceBusy(msg),
            IoError::ChannelError(msg) => {
                if msg.contains("not found") {
                    AetherError::ChannelNotFound(msg)
                } else if msg.contains("exists") {
                    AetherError::AlreadyExists(msg)
                } else {
                    AetherError::Processing(msg)
                }
            },
            IoError::PointError(msg) => AetherError::NotFound {
                resource: format!("Point: {}", msg),
            },
            IoError::ValidationError(msg) => AetherError::Validation(msg),
            IoError::PermissionError(msg) => AetherError::Forbidden(msg),
            IoError::StateError(msg) => AetherError::Internal(msg),
            IoError::BatchError(msg) => AetherError::Internal(msg),
            IoError::InternalError(msg) => AetherError::Internal(msg),
        }
    }
}

// ============================================================================
// IoError implements AetherErrorTrait
// ============================================================================

use errors::{AetherErrorTrait, ErrorCategory};

impl AetherErrorTrait for IoError {
    fn error_code(&self) -> &'static str {
        match self {
            Self::ConfigError(_) => "IO_CONFIG_ERROR",
            Self::IoError(_) => "IO_IO_ERROR",
            Self::ProtocolError(_) => "IO_PROTOCOL_ERROR",
            Self::ConnectionError(_) => "IO_CONNECTION_ERROR",
            Self::DataError(_) => "IO_DATA_ERROR",
            Self::TimeoutError(_) => "IO_TIMEOUT",
            Self::StorageError(_) => "IO_STORAGE_ERROR",
            Self::ResourceError(_) => "IO_RESOURCE_ERROR",
            Self::ChannelError(_) => "IO_CHANNEL_ERROR",
            Self::PointError(_) => "IO_POINT_ERROR",
            Self::ValidationError(_) => "IO_VALIDATION_ERROR",
            Self::PermissionError(_) => "IO_PERMISSION_ERROR",
            Self::StateError(_) => "IO_STATE_ERROR",
            Self::BatchError(_) => "IO_BATCH_ERROR",
            Self::InternalError(_) => "IO_INTERNAL_ERROR",
        }
    }

    fn category(&self) -> ErrorCategory {
        match self {
            Self::ConfigError(_) => ErrorCategory::Configuration,
            Self::IoError(_) => ErrorCategory::Internal,
            Self::ProtocolError(_) => ErrorCategory::Protocol,
            Self::ConnectionError(_) => ErrorCategory::Connection,
            Self::DataError(_) => ErrorCategory::Validation,
            Self::TimeoutError(_) => ErrorCategory::Timeout,
            Self::StorageError(_) => ErrorCategory::Database,
            Self::ResourceError(_) => ErrorCategory::ResourceExhausted,
            Self::ChannelError(_) => ErrorCategory::NotFound,
            Self::PointError(_) => ErrorCategory::NotFound,
            Self::ValidationError(_) => ErrorCategory::Validation,
            Self::PermissionError(_) => ErrorCategory::Permission,
            Self::StateError(_) => ErrorCategory::ResourceBusy,
            Self::BatchError(_) => ErrorCategory::Internal,
            Self::InternalError(_) => ErrorCategory::Internal,
        }
    }

    fn suggestion(&self) -> Option<String> {
        match self {
            Self::ConfigError(_) => Some(
                "Check aether-io configuration in config/io/ and run 'aether sync'".to_string()
            ),
            Self::ChannelError(msg) => {
                if msg.contains("not found") {
                    Some("Use 'aether channels list' to see available channels".to_string())
                } else if msg.contains("exists") {
                    Some("Channel already exists. Use a different ID or update the existing channel".to_string())
                } else {
                    Some("Check channel configuration and status with 'aether channels status <id>'".to_string())
                }
            },
            Self::PointError(msg) => {
                if msg.contains("not found") {
                    Some("Verify the point exists in the channel configuration. Use GET /api/channels/{id}/points to list points".to_string())
                } else {
                    Some("Check point configuration in the channel's CSV files".to_string())
                }
            },
            Self::ConnectionError(_) => Some(
                "Verify the device is reachable and check network/serial port settings".to_string()
            ),
            Self::ProtocolError(_) => Some(
                "Check protocol configuration (Modbus slave ID, function codes, register addresses)".to_string()
            ),
            Self::TimeoutError(_) => Some(
                "Increase timeout settings or check device responsiveness".to_string()
            ),
            Self::StorageError(_) => Some(
                "Check the SHM mount, writer health, and available slot capacity".to_string()
            ),
            Self::ValidationError(_) => None, // Validation errors should be specific in the message
            Self::DataError(_) => Some(
                "Check data format and types. Verify scale/offset configuration in point definitions".to_string()
            ),
            _ => None,
        }
    }
}

// ============================================================================
// API Adaptation: IoError → AppError conversion
// ============================================================================

impl From<IoError> for common::AppError {
    fn from(err: IoError) -> Self {
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

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
#[allow(clippy::disallowed_methods)] // Test code - unwrap is acceptable
mod tests {
    use super::*;
    use errors::{AetherErrorTrait, ErrorCategory};

    /// Helper macro to test constructor + display + error_code + category in one shot
    macro_rules! test_error_variant {
        ($constructor:ident, $variant:pat, $msg:expr_2021, $code:expr_2021, $category:expr_2021) => {
            let err = IoError::$constructor($msg);
            assert!(matches!(err, $variant));
            assert!(err.to_string().contains($msg));
            assert_eq!(err.error_code(), $code);
            assert_eq!(err.category(), $category);
        };
    }

    #[test]
    fn test_all_constructors_and_traits() {
        test_error_variant!(
            config,
            IoError::ConfigError(_),
            "missing field",
            "IO_CONFIG_ERROR",
            ErrorCategory::Configuration
        );
        test_error_variant!(
            io,
            IoError::IoError(_),
            "read failed",
            "IO_IO_ERROR",
            ErrorCategory::Internal
        );
        test_error_variant!(
            protocol,
            IoError::ProtocolError(_),
            "invalid frame",
            "IO_PROTOCOL_ERROR",
            ErrorCategory::Protocol
        );
        test_error_variant!(
            connection,
            IoError::ConnectionError(_),
            "refused",
            "IO_CONNECTION_ERROR",
            ErrorCategory::Connection
        );
        test_error_variant!(
            data,
            IoError::DataError(_),
            "invalid format",
            "IO_DATA_ERROR",
            ErrorCategory::Validation
        );
        test_error_variant!(
            timeout,
            IoError::TimeoutError(_),
            "5000ms",
            "IO_TIMEOUT",
            ErrorCategory::Timeout
        );
        test_error_variant!(
            storage,
            IoError::StorageError(_),
            "SHM unavailable",
            "IO_STORAGE_ERROR",
            ErrorCategory::Database
        );
        test_error_variant!(
            resource,
            IoError::ResourceError(_),
            "pool exhausted",
            "IO_RESOURCE_ERROR",
            ErrorCategory::ResourceExhausted
        );
        test_error_variant!(
            channel,
            IoError::ChannelError(_),
            "closed",
            "IO_CHANNEL_ERROR",
            ErrorCategory::NotFound
        );
        test_error_variant!(
            point,
            IoError::PointError(_),
            "bad address",
            "IO_POINT_ERROR",
            ErrorCategory::NotFound
        );
        test_error_variant!(
            validation,
            IoError::ValidationError(_),
            "out of range",
            "IO_VALIDATION_ERROR",
            ErrorCategory::Validation
        );
        test_error_variant!(
            permission,
            IoError::PermissionError(_),
            "access denied",
            "IO_PERMISSION_ERROR",
            ErrorCategory::Permission
        );
        test_error_variant!(
            state,
            IoError::StateError(_),
            "lock poisoned",
            "IO_STATE_ERROR",
            ErrorCategory::ResourceBusy
        );
        test_error_variant!(
            batch,
            IoError::BatchError(_),
            "3 failed",
            "IO_BATCH_ERROR",
            ErrorCategory::Internal
        );
        test_error_variant!(
            internal,
            IoError::InternalError(_),
            "unexpected",
            "IO_INTERNAL_ERROR",
            ErrorCategory::Internal
        );
    }

    #[test]
    fn test_convenience_constructors() {
        let err = IoError::channel_not_found(1001);
        assert!(err.to_string().contains("not found") && err.to_string().contains("1001"));

        let err = IoError::channel_exists(1002);
        assert!(err.to_string().contains("already exists") && err.to_string().contains("1002"));

        let err = IoError::invalid_channel_id(99999);
        assert!(err.to_string().contains("Invalid channel ID"));

        let err = IoError::point_not_found("T:100");
        assert!(err.to_string().contains("not found") && err.to_string().contains("T:100"));

        let err = IoError::not_connected();
        assert!(matches!(err, IoError::ConnectionError(_)));
    }

    #[test]
    fn test_from_external_errors() {
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "file not found");
        let err: IoError = io_err.into();
        assert!(matches!(err, IoError::IoError(_)));

        let json_err = serde_json::from_str::<serde_json::Value>("bad").unwrap_err();
        let err: IoError = json_err.into();
        assert!(matches!(err, IoError::DataError(_)) && err.to_string().contains("JSON"));

        let yaml_err = serde_yml::from_str::<serde_yml::Value>("invalid: yaml: :").unwrap_err();
        let err: IoError = yaml_err.into();
        assert!(matches!(err, IoError::DataError(_)) && err.to_string().contains("YAML"));

        let anyhow_err = anyhow::anyhow!("something went wrong");
        let err: IoError = anyhow_err.into();
        assert!(matches!(err, IoError::ConfigError(_)));
    }

    #[test]
    fn test_is_retryable() {
        assert!(IoError::TimeoutError("".into()).is_retryable());
        assert!(IoError::StateError("".into()).is_retryable());
        assert!(!IoError::ConfigError("".into()).is_retryable());
        assert!(!IoError::ValidationError("".into()).is_retryable());
    }

    #[test]
    fn test_suggestions() {
        assert!(
            IoError::ConfigError("t".into())
                .suggestion()
                .unwrap()
                .contains("aether sync")
        );
        assert!(
            IoError::channel_not_found(1)
                .suggestion()
                .unwrap()
                .contains("aether channels")
        );
        assert!(
            IoError::channel_exists(1)
                .suggestion()
                .unwrap()
                .contains("already exists")
        );
        assert!(
            IoError::point_not_found("T:1")
                .suggestion()
                .unwrap()
                .contains("/api/channels")
        );
        assert!(
            IoError::ConnectionError("t".into())
                .suggestion()
                .unwrap()
                .contains("reachable")
        );
        assert!(
            IoError::ProtocolError("t".into())
                .suggestion()
                .unwrap()
                .contains("Modbus")
        );
        assert!(
            IoError::TimeoutError("t".into())
                .suggestion()
                .unwrap()
                .contains("timeout")
        );
        assert!(
            IoError::StorageError("t".into())
                .suggestion()
                .unwrap()
                .contains("SHM")
        );
        assert!(
            IoError::DataError("t".into())
                .suggestion()
                .unwrap()
                .contains("scale/offset")
        );
        assert!(IoError::ValidationError("t".into()).suggestion().is_none());
    }

    #[test]
    fn test_to_voltage_error_conversions() {
        assert!(matches!(
            AetherError::from(IoError::ConfigError("t".into())),
            AetherError::Configuration(_)
        ));
        assert!(matches!(
            AetherError::from(IoError::IoError("t".into())),
            AetherError::Io(_)
        ));
        assert!(matches!(
            AetherError::from(IoError::ProtocolError("t".into())),
            AetherError::Protocol { .. }
        ));
        assert!(matches!(
            AetherError::from(IoError::ConnectionError("t".into())),
            AetherError::Communication(_)
        ));
        assert!(matches!(
            AetherError::from(IoError::DataError("t".into())),
            AetherError::Validation(_)
        ));
        assert!(matches!(
            AetherError::from(IoError::TimeoutError("t".into())),
            AetherError::Timeout(_)
        ));
        assert!(matches!(
            AetherError::from(IoError::StorageError("t".into())),
            AetherError::Database(_)
        ));
        assert!(matches!(
            AetherError::from(IoError::ResourceError("t".into())),
            AetherError::ResourceBusy(_)
        ));
        assert!(matches!(
            AetherError::from(IoError::channel_not_found(1)),
            AetherError::ChannelNotFound(_)
        ));
        assert!(matches!(
            AetherError::from(IoError::channel_exists(1)),
            AetherError::AlreadyExists(_)
        ));
        assert!(matches!(
            AetherError::from(IoError::ChannelError("other".into())),
            AetherError::Processing(_)
        ));
        assert!(matches!(
            AetherError::from(IoError::PointError("t".into())),
            AetherError::NotFound { .. }
        ));
        assert!(matches!(
            AetherError::from(IoError::ValidationError("t".into())),
            AetherError::Validation(_)
        ));
        assert!(matches!(
            AetherError::from(IoError::PermissionError("t".into())),
            AetherError::Forbidden(_)
        ));
        assert!(matches!(
            AetherError::from(IoError::StateError("t".into())),
            AetherError::Internal(_)
        ));
        assert!(matches!(
            AetherError::from(IoError::BatchError("t".into())),
            AetherError::Internal(_)
        ));
        assert!(matches!(
            AetherError::from(IoError::InternalError("t".into())),
            AetherError::Internal(_)
        ));
    }

    #[test]
    fn test_error_ext_trait() {
        // Test each conversion method
        let err: Result<()> = Err::<(), &str>("test").config_error("cfg");
        assert!(matches!(err.unwrap_err(), IoError::ConfigError(_)));

        let err: Result<()> = Err::<(), &str>("test").io_error("io");
        assert!(matches!(err.unwrap_err(), IoError::IoError(_)));

        let err: Result<()> = Err::<(), &str>("test").protocol_error("proto");
        assert!(matches!(err.unwrap_err(), IoError::ProtocolError(_)));

        let err: Result<()> = Err::<(), &str>("test").connection_error("conn");
        assert!(matches!(err.unwrap_err(), IoError::ConnectionError(_)));

        let err: Result<()> = Err::<(), &str>("test").data_error("data");
        assert!(matches!(err.unwrap_err(), IoError::DataError(_)));

        let err: Result<()> = Err::<(), &str>("test").context("ctx");
        assert!(matches!(err.unwrap_err(), IoError::InternalError(_)));

        // Ok values pass through
        assert_eq!(Ok::<i32, &str>(42).config_error("nope").unwrap(), 42);
    }

    #[test]
    fn test_error_display_format() {
        assert_eq!(
            IoError::ConfigError("missing key".into()).to_string(),
            "Configuration error: missing key"
        );
        assert_eq!(
            IoError::IoError("read failed".into()).to_string(),
            "IO error: read failed"
        );
    }

    #[test]
    fn test_debug() {
        let err = IoError::ConfigError("test".into());
        assert!(format!("{:?}", err).contains("ConfigError"));
    }
}
