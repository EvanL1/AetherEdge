//! Protocol and driver metadata system.
//!
//! This module provides self-describing metadata for protocols and drivers,
//! enabling dynamic discovery and configuration generation.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Parameter type for configuration options.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ParameterType {
    String,
    Integer,
    Boolean,
    Float,
    Object,
    Array,
}

/// Metadata for a single configuration parameter.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParameterMetadata {
    /// Internal parameter name (used in config).
    pub name: &'static str,
    /// Human-readable display name.
    pub display_name: &'static str,
    /// Description of the parameter.
    pub description: &'static str,
    /// Whether this parameter is required.
    pub required: bool,
    /// Default value if not specified.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_value: Option<Value>,
    /// Type of the parameter.
    pub param_type: ParameterType,
    /// Inclusive numeric minimum for integer parameters.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub minimum: Option<u64>,
    /// Inclusive numeric maximum for integer parameters.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub maximum: Option<u64>,
    /// Minimum UTF-8 string length for string parameters.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min_length: Option<usize>,
}

impl ParameterMetadata {
    /// Create a new required parameter.
    pub const fn required(
        name: &'static str,
        display_name: &'static str,
        description: &'static str,
        param_type: ParameterType,
    ) -> Self {
        Self {
            name,
            display_name,
            description,
            required: true,
            default_value: None,
            param_type,
            minimum: None,
            maximum: None,
            min_length: None,
        }
    }

    /// Create a new optional parameter with a default value.
    pub fn optional(
        name: &'static str,
        display_name: &'static str,
        description: &'static str,
        param_type: ParameterType,
        default_value: Value,
    ) -> Self {
        Self {
            name,
            display_name,
            description,
            required: false,
            default_value: Some(default_value),
            param_type,
            minimum: None,
            maximum: None,
            min_length: None,
        }
    }

    /// Declares an inclusive range for an integer parameter.
    #[must_use]
    pub const fn with_integer_range(mut self, minimum: u64, maximum: u64) -> Self {
        self.minimum = Some(minimum);
        self.maximum = Some(maximum);
        self
    }

    /// Declares a minimum length for a string parameter.
    #[must_use]
    pub const fn with_min_length(mut self, minimum: usize) -> Self {
        self.min_length = Some(minimum);
        self
    }
}

/// Metadata for a driver implementation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DriverMetadata {
    /// Internal driver name (used in config).
    pub name: &'static str,
    /// Human-readable display name.
    pub display_name: &'static str,
    /// Description of the driver.
    pub description: &'static str,
    /// Whether this is the recommended driver.
    pub is_recommended: bool,
    /// Example configuration JSON.
    pub example_config: Value,
    /// Available configuration parameters.
    pub parameters: Vec<ParameterMetadata>,
}

/// Metadata for a protocol.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProtocolMetadata {
    /// Internal protocol name.
    pub name: &'static str,
    /// Human-readable display name.
    pub display_name: &'static str,
    /// Description of the protocol.
    pub description: &'static str,
    /// Protocol type identifier (e.g., "modbus_tcp", "di_do").
    pub protocol_type: &'static str,
    /// Available drivers for this protocol.
    pub drivers: Vec<DriverMetadata>,
    /// Whether this protocol supports point configuration.
    pub supports_points: bool,
}

/// Trait for types that can provide their own metadata.
pub trait HasMetadata {
    /// Get the metadata for this type.
    fn metadata() -> DriverMetadata;
}
