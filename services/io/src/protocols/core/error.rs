//! Error types for the Industrial Gateway.

use thiserror::Error;

/// A specialized Result type for gateway operations.
pub type Result<T> = std::result::Result<T, GatewayError>;

/// The main error type for all gateway operations.
#[derive(Debug, Error)]
pub enum GatewayError {
    // === Connection Errors ===
    /// Connection failed
    #[error("Connection error: {0}")]
    Connection(String),

    /// Not connected to the target
    #[error("Not connected")]
    NotConnected,

    /// Connection timeout
    #[error("Connection timeout after {0}ms")]
    ConnectionTimeout(u64),

    // === Protocol Errors ===
    /// Protocol-level error
    #[error("Protocol error: {0}")]
    Protocol(String),

    /// Invalid response from device
    #[error("Invalid response: {0}")]
    InvalidResponse(String),

    /// Unsupported function or feature
    #[error("Unsupported: {0}")]
    Unsupported(String),

    // === Data Errors ===
    /// Invalid data format
    #[error("Invalid data: {0}")]
    InvalidData(String),

    /// Data conversion failed
    #[error("Data conversion error: {0}")]
    DataConversion(String),

    /// Point not found
    #[error("Point not found: {0}")]
    PointNotFound(String),

    // === Configuration Errors ===
    /// Configuration error
    #[error("Configuration error: {0}")]
    Config(String),

    /// Invalid address format
    #[error("Invalid address: {0}")]
    InvalidAddress(String),

    // === IO Errors ===
    /// IO operation failed
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    /// Read operation timeout
    #[error("Read timeout")]
    ReadTimeout,

    /// Write operation timeout
    #[error("Write timeout")]
    WriteTimeout,

    // === Protocol-Specific Errors ===
    /// Modbus protocol error
    #[error("Modbus error: {0}")]
    Modbus(String),

    /// IEC 104 protocol error
    #[error("IEC 104 error: {0}")]
    Iec104(String),

    /// DNP3 protocol error
    #[error("DNP3 error: {0}")]
    Dnp3(String),

    /// OPC UA protocol error
    #[error("OPC UA error: {0}")]
    OpcUa(String),

    // === Internal Errors ===
    /// Internal error (bug)
    #[error("Internal error: {0}")]
    Internal(String),

    /// Channel closed
    #[error("Channel closed")]
    ChannelClosed,
}

impl GatewayError {
    /// Check if this error indicates that reconnection is needed.
    pub fn needs_reconnect(&self) -> bool {
        matches!(
            self,
            Self::Connection(_)
                | Self::NotConnected
                | Self::ConnectionTimeout(_)
                | Self::Io(_)
                | Self::ChannelClosed
        )
    }

    /// Check if this error is retryable.
    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            Self::ReadTimeout | Self::WriteTimeout | Self::Connection(_)
        )
    }

    /// Create a protocol error.
    pub fn protocol(msg: impl Into<String>) -> Self {
        Self::Protocol(msg.into())
    }

    /// Create a connection error.
    pub fn connection(msg: impl Into<String>) -> Self {
        Self::Connection(msg.into())
    }

    /// Create an IO error from a message.
    pub fn io(msg: impl Into<String>) -> Self {
        Self::Io(std::io::Error::other(msg.into()))
    }

    /// Create a configuration error.
    pub fn config(msg: impl Into<String>) -> Self {
        Self::Config(msg.into())
    }

    /// Create an invalid data error.
    pub fn invalid_data(msg: impl Into<String>) -> Self {
        Self::InvalidData(msg.into())
    }

    /// Create a Modbus error.
    pub fn modbus(msg: impl Into<String>) -> Self {
        Self::Modbus(msg.into())
    }

    /// Create an internal error.
    pub fn internal(msg: impl Into<String>) -> Self {
        Self::Internal(msg.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_needs_reconnect() {
        assert!(GatewayError::NotConnected.needs_reconnect());
        assert!(GatewayError::connection("test").needs_reconnect());
        assert!(!GatewayError::protocol("test").needs_reconnect());
    }

    #[test]
    fn test_is_retryable() {
        assert!(GatewayError::ReadTimeout.is_retryable());
        assert!(GatewayError::WriteTimeout.is_retryable());
        assert!(!GatewayError::NotConnected.is_retryable());
    }
}
