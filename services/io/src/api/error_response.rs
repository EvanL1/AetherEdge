//! HTTP error projection for the IO service.

use errors::{AetherErrorTrait, ErrorCategory};

use crate::error::IoError;

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
            Self::DataError(_) | Self::ValidationError(_) => ErrorCategory::Validation,
            Self::TimeoutError(_) => ErrorCategory::Timeout,
            Self::StorageError(_) => ErrorCategory::Database,
            Self::ResourceError(_) => ErrorCategory::ResourceExhausted,
            Self::ChannelError(_) | Self::PointError(_) => ErrorCategory::NotFound,
            Self::PermissionError(_) => ErrorCategory::Permission,
            Self::StateError(_) => ErrorCategory::ResourceBusy,
            Self::BatchError(_) | Self::InternalError(_) => ErrorCategory::Internal,
        }
    }

    fn suggestion(&self) -> Option<String> {
        match self {
            Self::ConfigError(_) => Some(
                "Check aether-io configuration in config/io/ and run 'aether sync'".to_string(),
            ),
            Self::ChannelError(message) if message.contains("not found") => {
                Some("Use 'aether channels list' to see available channels".to_string())
            },
            Self::ChannelError(message) if message.contains("exists") => Some(
                "Channel already exists. Use a different ID or update the existing channel"
                    .to_string(),
            ),
            Self::ChannelError(_) => Some(
                "Check channel configuration and status with 'aether channels status <id>'"
                    .to_string(),
            ),
            Self::PointError(message) if message.contains("not found") => {
                Some(
                    "Verify the point exists in the channel configuration. Use GET /api/channels/{id}/points to list points"
                        .to_string(),
                )
            },
            Self::PointError(_) => {
                Some("Check point configuration in the channel's CSV files".to_string())
            },
            Self::ConnectionError(_) => Some(
                "Verify the device is reachable and check network/serial port settings".to_string(),
            ),
            Self::ProtocolError(_) => Some(
                "Check protocol configuration (Modbus slave ID, function codes, register addresses)"
                    .to_string(),
            ),
            Self::TimeoutError(_) => {
                Some("Increase timeout settings or check device responsiveness".to_string())
            },
            Self::StorageError(_) => Some(
                "Check the SHM mount, writer health, and available slot capacity".to_string(),
            ),
            Self::DataError(_) => Some(
                "Check data format and types. Verify scale/offset configuration in point definitions"
                    .to_string(),
            ),
            _ => None,
        }
    }
}

impl From<IoError> for common::AppError {
    fn from(error: IoError) -> Self {
        common::app_error_from(&error)
    }
}
