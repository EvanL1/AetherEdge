//! Model Layer Error Types

use thiserror::Error;

/// Result type for aether-model operations
pub type Result<T> = std::result::Result<T, ModelError>;

/// Model layer errors
#[derive(Debug, Error, Clone)]
pub enum ModelError {
    /// Expression evaluation error
    #[error("Expression error: {0}")]
    Expression(String),

    /// Statistics calculation error
    #[error("Statistics error: {0}")]
    Statistics(String),

    /// Time series processing error
    #[error("Time series error: {0}")]
    TimeSeries(String),

    /// Energy calculation error
    #[error("Energy calculation error: {0}")]
    Energy(String),

    /// Calculation engine error
    #[error("Calculation error: {0}")]
    Calculation(String),

    /// Calculation not found
    #[error("Calculation not found: {0}")]
    CalculationNotFound(String),

    /// Product not found
    #[error("Product not found: {0}")]
    ProductNotFound(String),

    /// Product parsing error
    #[error("Product parsing error: {0}")]
    ProductParsing(String),

    /// Instance not found
    #[error("Instance not found: {0}")]
    InstanceNotFound(String),

    /// Instance already exists
    #[error("Instance already exists: {0}")]
    InstanceExists(String),

    /// Invalid instance name
    #[error("Invalid instance name: {0}")]
    InvalidInstanceName(String),

    /// Validation error
    #[error("Validation error: {0}")]
    Validation(String),

    /// Database error
    #[error("Database error: {0}")]
    Database(String),

    /// RTDB error
    #[error("RTDB error: {0}")]
    Rtdb(String),

    /// Configuration error
    #[error("Configuration error: {0}")]
    Config(String),

    /// IO error
    #[error("IO error: {0}")]
    Io(String),

    /// Serialization error
    #[error("Serialization error: {0}")]
    Serialization(String),

    /// Internal error
    #[error("Internal error: {0}")]
    Internal(String),
}

impl From<std::io::Error> for ModelError {
    fn from(err: std::io::Error) -> Self {
        ModelError::Io(err.to_string())
    }
}

impl From<serde_json::Error> for ModelError {
    fn from(err: serde_json::Error) -> Self {
        ModelError::Serialization(err.to_string())
    }
}

impl From<sqlx::Error> for ModelError {
    fn from(err: sqlx::Error) -> Self {
        ModelError::Database(err.to_string())
    }
}

// Helper methods
impl ModelError {
    pub fn expression(msg: impl Into<String>) -> Self {
        ModelError::Expression(msg.into())
    }

    pub fn statistics(msg: impl Into<String>) -> Self {
        ModelError::Statistics(msg.into())
    }

    pub fn calculation(msg: impl Into<String>) -> Self {
        ModelError::Calculation(msg.into())
    }

    pub fn validation(msg: impl Into<String>) -> Self {
        ModelError::Validation(msg.into())
    }

    pub fn internal(msg: impl Into<String>) -> Self {
        ModelError::Internal(msg.into())
    }
}
