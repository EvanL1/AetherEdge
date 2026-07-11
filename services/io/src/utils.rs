//! Utility Functions and Common Components
//!
//! This module provides essential utilities and shared
//! components used throughout the communication service library.
//!
//! # Features
//!
//! - Protocol name normalization
//! - Protocol type parsing

use std::borrow::Cow;
use std::fmt;

// ============================================================================
// Protocol Types (inlined from aether-comlink)
// ============================================================================

/// Supported protocol types
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ProtocolType {
    // Modbus variants
    ModbusTcp,
    ModbusRtu,
    ModbusAscii,
    // IEC protocols
    Iec60870_5_104,
    Iec61850,
    // Other protocols
    Mqtt,
    Opcua,
    Bacnet,
    Dnp3,
    Virtual,
    Grpc,
}

impl ProtocolType {
    /// Parse from string
    ///
    /// Optimization: First try exact matches and common variations (zero allocation),
    /// then fallback to normalized comparison if needed.
    pub fn parse(s: &str) -> Option<Self> {
        let s = s.trim();

        // Fast path: exact match for common lowercase names (zero allocation)
        match s {
            "modbus_tcp" | "ModbusTcp" | "MODBUS_TCP" => return Some(Self::ModbusTcp),
            "modbus_rtu" | "ModbusRtu" | "MODBUS_RTU" => return Some(Self::ModbusRtu),
            "modbus_ascii" | "ModbusAscii" | "MODBUS_ASCII" => return Some(Self::ModbusAscii),
            "iec60870_5_104" | "iec104" | "IEC104" => return Some(Self::Iec60870_5_104),
            "iec61850" | "IEC61850" => return Some(Self::Iec61850),
            "mqtt" | "MQTT" => return Some(Self::Mqtt),
            "opcua" | "opc_ua" | "OPCUA" | "OPC_UA" => return Some(Self::Opcua),
            "bacnet" | "BACnet" | "BACNET" => return Some(Self::Bacnet),
            "dnp3" | "DNP3" => return Some(Self::Dnp3),
            "virtual" | "Virtual" | "VIRTUAL" => return Some(Self::Virtual),
            "grpc" | "GRPC" | "gRPC" => return Some(Self::Grpc),
            _ => {},
        }

        // Slow path: normalize and match (allocates only for non-standard inputs)
        let normalized = s.to_ascii_lowercase().replace('-', "_");
        match normalized.as_str() {
            "modbus_tcp" => Some(Self::ModbusTcp),
            "modbus_rtu" => Some(Self::ModbusRtu),
            "modbus_ascii" => Some(Self::ModbusAscii),
            "iec60870_5_104" | "iec104" => Some(Self::Iec60870_5_104),
            "iec61850" => Some(Self::Iec61850),
            "mqtt" => Some(Self::Mqtt),
            "opcua" | "opc_ua" => Some(Self::Opcua),
            "bacnet" => Some(Self::Bacnet),
            "dnp3" => Some(Self::Dnp3),
            "virtual" => Some(Self::Virtual),
            "grpc" => Some(Self::Grpc),
            _ => None,
        }
    }

    /// Convert to string representation
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::ModbusTcp => "modbus_tcp",
            Self::ModbusRtu => "modbus_rtu",
            Self::ModbusAscii => "modbus_ascii",
            Self::Iec60870_5_104 => "iec60870_5_104",
            Self::Iec61850 => "iec61850",
            Self::Mqtt => "mqtt",
            Self::Opcua => "opcua",
            Self::Bacnet => "bacnet",
            Self::Dnp3 => "dnp3",
            Self::Virtual => "virtual",
            Self::Grpc => "grpc",
        }
    }
}

impl fmt::Display for ProtocolType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

// ============================================================================
// Protocol Name Utilities
// ============================================================================

/// Returns true for Modbus and SunSpec aliases (identical point mapping format).
pub fn is_modbus_family(protocol: &str) -> bool {
    let p = protocol.trim();
    p.eq_ignore_ascii_case("modbus")
        || p.eq_ignore_ascii_case("modbus_tcp")
        || p.eq_ignore_ascii_case("modbus_rtu")
        || p.eq_ignore_ascii_case("sunspec_tcp")
        || p.eq_ignore_ascii_case("sunspec_rtu")
}

/// Normalize protocol name to standard format (lowercase underscore)
/// This ensures consistency across configuration files, plugins, and database.
///
/// Returns `Cow::Borrowed` for known protocol names (zero allocation),
/// `Cow::Owned` only for unknown custom protocols.
pub fn normalize_protocol_name(name: &str) -> Cow<'static, str> {
    // Clean input: trim whitespace and convert to lowercase
    let cleaned = name.trim().to_lowercase();

    // Replace common separators with underscores for matching
    let normalized = cleaned.replace(['-', ' ', '.'], "_");

    // Map various protocol name variations to standard names
    match normalized.as_str() {
        // Modbus variations
        "modbus_tcp" | "modbustcp" | "modbus tcp" => Cow::Borrowed("modbus_tcp"),
        "modbus_rtu" | "modbusrtu" | "modbus rtu" => Cow::Borrowed("modbus_rtu"),
        "modbus_ascii" | "modbusascii" | "modbus ascii" => Cow::Borrowed("modbus_ascii"),

        // SunSpec (Modbus transport alias)
        "sunspec_tcp" | "sunspectcp" | "sunspec tcp" => Cow::Borrowed("sunspec_tcp"),
        "sunspec_rtu" | "sunspecrtu" | "sunspec rtu" => Cow::Borrowed("sunspec_rtu"),

        // Virtual protocol variations
        "virtual" | "virt" | "virtual_protocol" => Cow::Borrowed("virtual"),

        // IEC variations
        "iec104" | "iec_104" | "iec60870" | "iec_60870" | "iec60870_5_104" | "iec_60870_5_104" => {
            Cow::Borrowed("iec104")
        },

        // gRPC variations
        "grpc" | "g_rpc" => Cow::Borrowed("grpc"),

        // MQTT variations
        "mqtt" | "mqtt_protocol" => Cow::Borrowed("mqtt"),

        // OPC UA variations
        "opcua" | "opc_ua" | "opc ua" => Cow::Borrowed("opcua"),

        // Aether-485 private protocol
        "aether_485" | "aether485" | "v485" => Cow::Borrowed("aether_485"),

        // Default: return cleaned name with underscores
        _ => Cow::Owned(normalized),
    }
}

/// Parse protocol name string to ProtocolType enum
/// Returns None if the protocol type is not recognized
pub fn parse_protocol_type(name: &str) -> Option<ProtocolType> {
    // Use normalize_protocol_name to handle variations
    let normalized = normalize_protocol_name(name);

    // Try to parse using ProtocolType::parse
    ProtocolType::parse(&normalized)
}

/// Get protocol type string from enum
pub fn protocol_type_to_string(protocol: ProtocolType) -> String {
    protocol.to_string()
}

#[cfg(test)]
#[allow(clippy::disallowed_methods)] // Test code - unwrap is acceptable
mod tests {
    use super::*;

    #[test]
    fn test_normalize_protocol_name() {
        // Test Modbus variations
        assert_eq!(normalize_protocol_name("modbus_tcp"), "modbus_tcp");
        assert_eq!(normalize_protocol_name("modbustcp"), "modbus_tcp");
        assert_eq!(normalize_protocol_name("MODBUSTCP"), "modbus_tcp");
        assert_eq!(normalize_protocol_name("modbus-tcp"), "modbus_tcp");
        assert_eq!(normalize_protocol_name("modbus tcp"), "modbus_tcp");
        assert_eq!(normalize_protocol_name(" Modbus_TCP "), "modbus_tcp");

        assert_eq!(normalize_protocol_name("modbus_rtu"), "modbus_rtu");
        assert_eq!(normalize_protocol_name("modbusrtu"), "modbus_rtu");
        assert_eq!(normalize_protocol_name("MODBUS-RTU"), "modbus_rtu");

        assert_eq!(normalize_protocol_name("sunspec_tcp"), "sunspec_tcp");
        assert_eq!(normalize_protocol_name("sunspectcp"), "sunspec_tcp");
        assert_eq!(normalize_protocol_name("SUNSPEC-TCP"), "sunspec_tcp");
        assert_eq!(normalize_protocol_name("sunspec_rtu"), "sunspec_rtu");

        // Test Virtual variations
        assert_eq!(normalize_protocol_name("virtual"), "virtual");
        assert_eq!(normalize_protocol_name("virt"), "virtual");
        assert_eq!(normalize_protocol_name("VIRTUAL"), "virtual");
        assert_eq!(normalize_protocol_name("virtual_protocol"), "virtual");

        // Test IEC variations
        assert_eq!(normalize_protocol_name("iec104"), "iec104");
        assert_eq!(normalize_protocol_name("iec_104"), "iec104");
        assert_eq!(normalize_protocol_name("iec60870"), "iec104");
        assert_eq!(normalize_protocol_name("IEC-60870-5-104"), "iec104");

        // Test other protocols
        assert_eq!(normalize_protocol_name("grpc"), "grpc");
        assert_eq!(normalize_protocol_name("GRPC"), "grpc");
        assert_eq!(normalize_protocol_name("mqtt"), "mqtt");
        assert_eq!(normalize_protocol_name("opcua"), "opcua");
        assert_eq!(normalize_protocol_name("OPC-UA"), "opcua");

        // Test unknown protocols (should return cleaned version)
        assert_eq!(
            normalize_protocol_name("custom-protocol"),
            "custom_protocol"
        );
        assert_eq!(normalize_protocol_name("NEW.PROTOCOL"), "new_protocol");
    }

    #[test]
    fn test_parse_protocol_type() {
        // Test valid protocol types
        assert_eq!(
            parse_protocol_type("modbus_tcp"),
            Some(ProtocolType::ModbusTcp)
        );
        assert_eq!(
            parse_protocol_type("modbustcp"),
            Some(ProtocolType::ModbusTcp)
        );
        assert_eq!(
            parse_protocol_type("MODBUS-TCP"),
            Some(ProtocolType::ModbusTcp)
        );
        assert_eq!(
            parse_protocol_type(" Modbus TCP "),
            Some(ProtocolType::ModbusTcp)
        );

        assert_eq!(
            parse_protocol_type("modbus_rtu"),
            Some(ProtocolType::ModbusRtu)
        );
        assert_eq!(
            parse_protocol_type("modbusrtu"),
            Some(ProtocolType::ModbusRtu)
        );
        assert_eq!(
            parse_protocol_type("MODBUS-RTU"),
            Some(ProtocolType::ModbusRtu)
        );

        assert_eq!(parse_protocol_type("virtual"), Some(ProtocolType::Virtual));
        assert_eq!(parse_protocol_type("virt"), Some(ProtocolType::Virtual));
        assert_eq!(parse_protocol_type("VIRTUAL"), Some(ProtocolType::Virtual));

        // Test additional valid protocol types (now supported in full ProtocolType)
        assert_eq!(
            parse_protocol_type("iec104"),
            Some(ProtocolType::Iec60870_5_104)
        );
        assert_eq!(
            parse_protocol_type("IEC-60870-5-104"),
            Some(ProtocolType::Iec60870_5_104)
        );
        assert_eq!(parse_protocol_type("mqtt"), Some(ProtocolType::Mqtt));
        assert_eq!(parse_protocol_type("grpc"), Some(ProtocolType::Grpc));
        assert_eq!(parse_protocol_type("opcua"), Some(ProtocolType::Opcua));
        assert_eq!(parse_protocol_type("OPC-UA"), Some(ProtocolType::Opcua));

        // Test invalid protocol types
        assert_eq!(parse_protocol_type("unknown"), None);
        assert_eq!(parse_protocol_type("can"), None); // CAN protocol removed
    }

    #[test]
    fn test_protocol_type_to_string() {
        assert_eq!(
            protocol_type_to_string(ProtocolType::ModbusTcp),
            "modbus_tcp"
        );
        assert_eq!(
            protocol_type_to_string(ProtocolType::ModbusRtu),
            "modbus_rtu"
        );
        assert_eq!(protocol_type_to_string(ProtocolType::Virtual), "virtual");
    }

    #[test]
    fn test_is_modbus_family() {
        assert!(is_modbus_family("modbus_tcp"));
        assert!(is_modbus_family("MODBUS_RTU"));
        assert!(is_modbus_family("sunspec_tcp"));
        assert!(is_modbus_family("sunspec_rtu"));
        assert!(!is_modbus_family("can"));
        assert!(!is_modbus_family("iec61850"));
    }
}
