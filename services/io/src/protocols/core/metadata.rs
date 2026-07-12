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

/// Trait for types that can provide their own metadata.
pub trait HasMetadata {
    /// Get the metadata for this type.
    fn metadata() -> DriverMetadata;
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

    // Register Modbus protocol
    #[cfg(feature = "modbus")]
    {
        use crate::protocols::adapters::modbus::ModbusChannel;
        let modbus_tcp = ModbusChannel::tcp_metadata();
        let modbus_rtu = ModbusChannel::rtu_metadata();
        registry.register(ProtocolMetadata {
            name: "modbus",
            display_name: "Modbus TCP",
            description: "Industrial Modbus TCP protocol",
            protocol_type: "modbus_tcp",
            drivers: vec![modbus_tcp.clone()],
            supports_points: true,
        });
        registry.register(ProtocolMetadata {
            name: "modbus_rtu",
            display_name: "Modbus RTU",
            description: "Industrial Modbus RTU protocol over a serial device",
            protocol_type: "modbus_rtu",
            drivers: vec![modbus_rtu.clone()],
            supports_points: true,
        });
        registry.register(ProtocolMetadata {
            name: "sunspec_tcp",
            display_name: "SunSpec TCP",
            description:
                "SunSpec information model over Modbus TCP (same transport and point mappings as Modbus)",
            protocol_type: "sunspec_tcp",
            drivers: vec![modbus_tcp],
            supports_points: true,
        });
        registry.register(ProtocolMetadata {
            name: "sunspec_rtu",
            display_name: "SunSpec RTU",
            description:
                "SunSpec information model over Modbus RTU (same transport and point mappings as Modbus)",
            protocol_type: "sunspec_rtu",
            drivers: vec![modbus_rtu],
            supports_points: true,
        });
    }

    // Register IEC 104 protocol
    #[cfg(feature = "iec104")]
    {
        use crate::protocols::adapters::iec104::Iec104Channel;
        let iec104_meta = Iec104Channel::metadata();
        registry.register(ProtocolMetadata {
            name: "iec104",
            display_name: "IEC 60870-5-104",
            description: "IEC 104 telecontrol protocol over TCP/IP",
            protocol_type: "iec104",
            drivers: vec![iec104_meta],
            supports_points: true,
        });
    }

    // Register OPC UA protocol
    #[cfg(feature = "opcua")]
    {
        use crate::protocols::adapters::opcua::OpcUaChannel;
        let opcua_meta = OpcUaChannel::metadata();
        registry.register(ProtocolMetadata {
            name: "opcua",
            display_name: "OPC UA",
            description: "OPC UA client for industrial automation",
            protocol_type: "opcua",
            drivers: vec![opcua_meta],
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

    // Register BLE protocol
    #[cfg(feature = "ble")]
    {
        use crate::protocols::adapters::ble::BleChannel;
        let ble_meta = BleChannel::metadata();
        registry.register(ProtocolMetadata {
            name: "ble",
            display_name: "BLE GATT",
            description: "Bluetooth Low Energy GATT client",
            protocol_type: "ble",
            drivers: vec![ble_meta],
            supports_points: true,
        });
    }

    // Register Zigbee protocol
    #[cfg(feature = "zigbee")]
    {
        use crate::protocols::adapters::zigbee::ZigbeeChannel;
        let zigbee_meta = ZigbeeChannel::metadata();
        registry.register(ProtocolMetadata {
            name: "zigbee",
            display_name: "Zigbee",
            description: "Zigbee protocol via TCP-connected coordinator gateway",
            protocol_type: "zigbee",
            drivers: vec![zigbee_meta],
            supports_points: true,
        });
    }

    // Register Matter protocol
    #[cfg(feature = "matter")]
    {
        use crate::protocols::adapters::matter::MatterChannel;
        let matter_meta = MatterChannel::metadata();
        registry.register(ProtocolMetadata {
            name: "matter",
            display_name: "Matter",
            description: "Matter smart home protocol over UDP",
            protocol_type: "matter",
            drivers: vec![matter_meta],
            supports_points: true,
        });
    }

    // Register Virtual protocol
    {
        use crate::protocols::adapters::virtual_channel::VirtualChannel;
        let virtual_meta = VirtualChannel::metadata();
        registry.register(ProtocolMetadata {
            name: "virtual",
            display_name: "Virtual",
            description: "Virtual channel for testing and simulation",
            protocol_type: "virtual",
            drivers: vec![virtual_meta],
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
        // Should have at least one protocol (virtual is always available)
        assert!(!registry.protocols().is_empty());
    }
}
