//! Error types for aether-calc

use thiserror::Error;

/// Calculation errors
#[derive(Debug, Error)]
pub enum CalcError {
    #[error("Expression error: {0}")]
    Expression(String),

    #[error("State error: {0}")]
    State(String),

    #[error("Function error: {0}")]
    Function(String),

    #[error("Variable not found: {0}")]
    VariableNotFound(String),
}

impl CalcError {
    pub fn expression(msg: impl Into<String>) -> Self {
        Self::Expression(msg.into())
    }

    pub fn state(msg: impl Into<String>) -> Self {
        Self::State(msg.into())
    }

    pub fn function(msg: impl Into<String>) -> Self {
        Self::Function(msg.into())
    }

    pub fn variable_not_found(name: impl Into<String>) -> Self {
        Self::VariableNotFound(name.into())
    }
}

pub type Result<T> = std::result::Result<T, CalcError>;
