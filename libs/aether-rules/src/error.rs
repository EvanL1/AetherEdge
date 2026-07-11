//! Rule Engine Error Types

use thiserror::Error;

/// Result type for rule operations
pub type Result<T> = std::result::Result<T, RuleError>;

/// Rule engine errors
#[derive(Debug, Error)]
pub enum RuleError {
    /// Rule not found
    #[error("Rule not found: {0}")]
    NotFound(String),

    /// Rule already exists
    #[error("Rule already exists: {0}")]
    AlreadyExists(String),

    /// Invalid rule format
    #[error("Invalid rule format: {0}")]
    InvalidFormat(String),

    /// Rule parsing error
    #[error("Rule parsing error: {0}")]
    ParseError(String),

    /// Rule execution error
    #[error("Rule execution error: {0}")]
    ExecutionError(String),

    /// Condition evaluation error
    #[error("Condition evaluation error: {0}")]
    ConditionError(String),

    /// Action execution error
    #[error("Action execution error: {0}")]
    ActionError(String),

    /// Database error
    #[error("Database error: {0}")]
    DatabaseError(String),

    /// Serialization error
    #[error("Serialization error: {0}")]
    SerializationError(String),

    /// Scheduler error
    #[error("Scheduler error: {0}")]
    SchedulerError(String),

    /// Routing error
    #[error("Routing error: {0}")]
    RoutingError(String),
}

impl From<sqlx::Error> for RuleError {
    fn from(err: sqlx::Error) -> Self {
        RuleError::DatabaseError(err.to_string())
    }
}

impl From<serde_json::Error> for RuleError {
    fn from(err: serde_json::Error) -> Self {
        RuleError::SerializationError(err.to_string())
    }
}

impl From<anyhow::Error> for RuleError {
    fn from(err: anyhow::Error) -> Self {
        RuleError::RoutingError(err.to_string())
    }
}
