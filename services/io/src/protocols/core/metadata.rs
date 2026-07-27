//! Protocol and driver metadata system.
//!
//! This module provides self-describing metadata for protocols and drivers,
//! enabling dynamic discovery and configuration generation.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::LazyLock;

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

/// Registry of all available protocols and drivers.
pub struct ProtocolRegistry {
    protocols: Vec<ProtocolMetadata>,
}

impl ProtocolRegistry {
    /// Create a new empty registry.
    pub fn new() -> Self {
        Self {
            protocols: Vec::new(),
        }
    }

    /// Register a protocol.
    pub fn register(&mut self, protocol: ProtocolMetadata) {
        self.protocols.push(protocol);
    }

    /// Get all registered protocols.
    pub fn protocols(&self) -> &[ProtocolMetadata] {
        &self.protocols
    }
}

impl Default for ProtocolRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Build the global protocol registry.
fn build_registry() -> ProtocolRegistry {
    let mut registry = ProtocolRegistry::new();

    // Register GPIO protocol (Linux only)
    #[cfg(all(feature = "gpio", target_os = "linux"))]
    {
        use crate::protocols::adapters::gpio::{GpiodDriver, SysfsDriver};
        registry.register(ProtocolMetadata {
            name: "gpio",
            display_name: "GPIO",
            description: "Digital Input/Output via GPIO pins",
            protocol_type: "di_do",
            drivers: vec![GpiodDriver::metadata(), SysfsDriver::metadata()],
            supports_points: true,
        });
    }

    // Register Modbus protocol.
    #[cfg(feature = "modbus")]
    {
        use crate::protocols::adapters::modbus::ModbusChannel;
        registry.register(ProtocolMetadata {
            name: "modbus",
            display_name: "Modbus TCP",
            description: "Industrial Modbus TCP protocol",
            protocol_type: "modbus_tcp",
            drivers: vec![ModbusChannel::tcp_metadata()],
            supports_points: true,
        });
        registry.register(ProtocolMetadata {
            name: "modbus_rtu",
            display_name: "Modbus RTU",
            description: "Industrial Modbus RTU protocol over a serial device",
            protocol_type: "modbus_rtu",
            drivers: vec![ModbusChannel::rtu_metadata()],
            supports_points: true,
        });
    }

    // Register CAN protocol (Linux only)
    #[cfg(all(feature = "can", target_os = "linux"))]
    {
        use crate::protocols::adapters::can::CanClient;
        let can_meta = CanClient::metadata();
        registry.register(ProtocolMetadata {
            name: "can",
            display_name: "CAN Bus",
            description: "Controller Area Network (CAN) bus protocol",
            protocol_type: "can",
            drivers: vec![can_meta],
            supports_points: true,
        });
    }

    registry
}

/// Global protocol registry instance.
static PROTOCOL_REGISTRY: LazyLock<ProtocolRegistry> = LazyLock::new(build_registry);

/// Get the global protocol registry.
pub fn get_protocol_registry() -> &'static ProtocolRegistry {
    &PROTOCOL_REGISTRY
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_registry_creation() {
        let registry = get_protocol_registry();
        if cfg!(any(feature = "modbus", feature = "can", feature = "gpio")) {
            assert!(!registry.protocols().is_empty());
        } else {
            assert!(registry.protocols().is_empty());
        }
    }
}
