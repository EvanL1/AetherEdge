//! Error types for aether-rtdb

use thiserror::Error;

#[derive(Error, Debug)]
pub enum RtdbError {
    #[error("Key not found: {0}")]
    KeyNotFound(String),

    #[error("Serialization error: {0}")]
    SerializationError(String),

    #[error("Connection error: {0}")]
    ConnectionError(String),

    #[error("Invalid data type: expected {expected}, got {got}")]
    InvalidDataType { expected: String, got: String },

    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

pub type Result<T> = std::result::Result<T, RtdbError>;

#[cfg(test)]
#[allow(clippy::disallowed_methods)] // Test code - unwrap is acceptable
mod tests {
    use super::*;

    #[test]
    fn test_key_not_found_error() {
        let err = RtdbError::KeyNotFound("test:key".to_string());
        assert_eq!(err.to_string(), "Key not found: test:key");
    }

    #[test]
    fn test_serialization_error() {
        let err = RtdbError::SerializationError("invalid JSON".to_string());
        assert_eq!(err.to_string(), "Serialization error: invalid JSON");
    }

    #[test]
    fn test_connection_error() {
        let err = RtdbError::ConnectionError("timeout".to_string());
        assert_eq!(err.to_string(), "Connection error: timeout");
    }

    #[test]
    fn test_invalid_data_type_error() {
        let err = RtdbError::InvalidDataType {
            expected: "f64".to_string(),
            got: "string".to_string(),
        };
        assert_eq!(
            err.to_string(),
            "Invalid data type: expected f64, got string"
        );
    }

    #[test]
    fn test_from_anyhow_error() {
        let anyhow_err = anyhow::anyhow!("test error");
        let rtdb_err: RtdbError = anyhow_err.into();
        assert!(matches!(rtdb_err, RtdbError::Other(_)));
        assert!(rtdb_err.to_string().contains("test error"));
    }

    #[test]
    fn test_error_debug_format() {
        let err = RtdbError::KeyNotFound("test".to_string());
        let debug_str = format!("{:?}", err);
        assert!(debug_str.contains("KeyNotFound"));
        assert!(debug_str.contains("test"));
    }
}
