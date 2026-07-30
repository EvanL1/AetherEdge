//! Modbus configuration types and constants.
//!
//! Contains all configuration structs, serde helpers, and builder patterns
//! for Modbus TCP/RTU channel setup.

use std::time::Duration;

use aether_core::PointType;
use serde::de::{self, Visitor};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

use crate::protocols::core::error::{GatewayError, Result};
use crate::protocols::core::point::{ByteOrder, DataFormat, PointConfig};

// ============================================================================
// Constants
// ============================================================================

/// Default connection timeout in milliseconds
pub const DEFAULT_CONNECT_TIMEOUT_MS: u64 = 5000;

/// Default I/O operation timeout in milliseconds
pub const DEFAULT_IO_TIMEOUT_MS: u64 = 3000;

/// Default maximum registers per batch read
pub const DEFAULT_MAX_BATCH_SIZE: u16 = 64;

/// Default maximum gap between registers to allow merging
pub const DEFAULT_MAX_GAP: u16 = 10;

/// Default reconnect cooldown in milliseconds (60 seconds)
pub const DEFAULT_RECONNECT_COOLDOWN_MS: u64 = 60_000;

/// Default maximum reconnect attempts (0 = unlimited)
pub const DEFAULT_MAX_RECONNECT_ATTEMPTS: u32 = 0;

/// Default consecutive zero-data cycles before triggering reconnect
pub const DEFAULT_ZERO_DATA_THRESHOLD: u32 = 5;

/// Modbus point address owned by the Modbus adapter.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModbusAddress {
    pub slave_id: u8,
    pub function_code: u8,
    pub register: u16,
    #[serde(default)]
    pub format: DataFormat,
    #[serde(default)]
    pub byte_order: ByteOrder,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bit_position: Option<u8>,
}

impl ModbusAddress {
    pub fn holding_register(slave_id: u8, register: u16, format: DataFormat) -> Self {
        Self {
            slave_id,
            function_code: 3,
            register,
            format,
            byte_order: ByteOrder::default(),
            bit_position: None,
        }
    }

    pub fn coil(slave_id: u8, register: u16) -> Self {
        Self {
            slave_id,
            function_code: 1,
            register,
            format: DataFormat::Bool,
            byte_order: ByteOrder::default(),
            bit_position: None,
        }
    }

    pub fn register_count(&self) -> u16 {
        self.format.register_count()
    }
}

// ============================================================================
// ReconnectConfig
// ============================================================================

/// Reconnect configuration for automatic connection recovery.
#[derive(Debug, Clone)]
pub struct ReconnectConfig {
    /// Cooldown period after disconnect before reconnect attempts (in ms)
    pub cooldown_ms: u64,
    /// Maximum reconnect attempts (0 = unlimited)
    pub max_attempts: u32,
    /// Consecutive zero-data polling cycles before triggering reconnect
    pub zero_data_threshold: u32,
}

impl Default for ReconnectConfig {
    fn default() -> Self {
        Self {
            cooldown_ms: DEFAULT_RECONNECT_COOLDOWN_MS,
            max_attempts: DEFAULT_MAX_RECONNECT_ATTEMPTS,
            zero_data_threshold: DEFAULT_ZERO_DATA_THRESHOLD,
        }
    }
}

impl ReconnectConfig {
    /// Create a new reconnect configuration.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set cooldown period.
    pub fn with_cooldown_ms(mut self, ms: u64) -> Self {
        self.cooldown_ms = ms;
        self
    }

    /// Set maximum reconnect attempts.
    pub fn with_max_attempts(mut self, attempts: u32) -> Self {
        self.max_attempts = attempts;
        self
    }

    /// Set zero-data threshold.
    pub fn with_zero_data_threshold(mut self, threshold: u32) -> Self {
        self.zero_data_threshold = threshold;
        self
    }
}

// ============================================================================
// ConnectionMode
// ============================================================================

/// Connection mode for Modbus channel.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ConnectionMode {
    /// TCP/IP connection (default)
    #[default]
    Tcp,
    /// RTU serial port connection
    #[cfg(feature = "modbus")]
    Rtu,
}

// ============================================================================
// ModbusMappingConfig (JSON deserialization)
// ============================================================================

/// Modbus point mapping configuration (deserialized from protocol_mappings JSON).
///
/// # Required Fields
/// - `slave_id`: Unit/slave ID in 1..=247.
/// - `function_code`: Function code compatible with the point direction.
/// - `register_address`: The Modbus register address (0-based).
///
/// # Optional Fields
/// - `data_type`: Data format (default: uint16)
/// - `byte_order`: Byte order for multi-byte values (default: ABCD)
/// - `bit_position`: Bit position for boolean extraction from register (0-15)
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModbusMappingConfig {
    #[serde(deserialize_with = "deserialize_u8")]
    pub slave_id: u8,

    #[serde(deserialize_with = "deserialize_u8")]
    pub function_code: u8,

    /// Register address (0-based). **Required field**.
    #[serde(deserialize_with = "deserialize_u16")]
    pub register_address: u16,

    #[serde(default)]
    pub data_type: DataFormat,

    #[serde(default, deserialize_with = "deserialize_byte_order")]
    pub byte_order: ByteOrder,

    #[serde(default, deserialize_with = "deserialize_optional_u8")]
    pub bit_position: Option<u8>,
}

/// Parse and validate one Modbus mapping without taking ownership of its JSON value.
///
/// This is the single codec used by both registry validation and runtime point
/// construction, so accepted aliases and validation bounds cannot drift.
pub(crate) fn parse_point_mapping(
    point_type: PointType,
    point_id: u32,
    mapping: &Value,
) -> Result<ModbusAddress> {
    let mapping = ModbusMappingConfig::deserialize(mapping).map_err(|error| {
        GatewayError::Config(format!(
            "invalid Modbus mapping for point {point_id}: {error}"
        ))
    })?;

    if !(1..=247).contains(&mapping.slave_id) {
        return Err(mapping_error(point_id, "slave_id must be in 1..247"));
    }

    let valid_function = match point_type {
        PointType::Telemetry | PointType::Signal => {
            matches!(mapping.function_code, 1..=4)
        },
        PointType::Control => matches!(mapping.function_code, 5 | 6 | 15 | 16),
        PointType::Adjustment => matches!(mapping.function_code, 6 | 16),
    };
    if !valid_function {
        return Err(mapping_error(
            point_id,
            &format!(
                "function_code {} does not match {point_type:?}",
                mapping.function_code
            ),
        ));
    }

    if mapping.bit_position.is_some_and(|bit| bit > 15) {
        return Err(mapping_error(point_id, "bit_position must be in 0..15"));
    }

    if mapping.data_type == DataFormat::String {
        return Err(mapping_error(
            point_id,
            "data_type must be a numeric or boolean Modbus format",
        ));
    }

    Ok(ModbusAddress {
        slave_id: mapping.slave_id,
        function_code: mapping.function_code,
        register: mapping.register_address,
        format: mapping.data_type,
        byte_order: mapping.byte_order,
        bit_position: mapping.bit_position,
    })
}

fn mapping_error(point_id: u32, reason: &str) -> GatewayError {
    GatewayError::Config(format!(
        "invalid Modbus mapping for point {point_id}: {reason}"
    ))
}

fn deserialize_u8<'de, D>(deserializer: D) -> std::result::Result<u8, D::Error>
where
    D: Deserializer<'de>,
{
    let value = deserialize_unsigned(deserializer)?;
    u8::try_from(value).map_err(|_| de::Error::custom("expected an integer in 0..=255"))
}

fn deserialize_u16<'de, D>(deserializer: D) -> std::result::Result<u16, D::Error>
where
    D: Deserializer<'de>,
{
    let value = deserialize_unsigned(deserializer)?;
    u16::try_from(value).map_err(|_| de::Error::custom("expected an integer in 0..=65535"))
}

fn deserialize_unsigned<'de, D>(deserializer: D) -> std::result::Result<u64, D::Error>
where
    D: Deserializer<'de>,
{
    struct UnsignedVisitor;

    impl<'de> Visitor<'de> for UnsignedVisitor {
        type Value = u64;

        fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("an unsigned integer or its decimal string representation")
        }

        fn visit_u64<E>(self, value: u64) -> std::result::Result<Self::Value, E> {
            Ok(value)
        }

        fn visit_i64<E>(self, value: i64) -> std::result::Result<Self::Value, E>
        where
            E: de::Error,
        {
            u64::try_from(value).map_err(|_| E::custom("expected an unsigned integer"))
        }

        fn visit_str<E>(self, value: &str) -> std::result::Result<Self::Value, E>
        where
            E: de::Error,
        {
            value
                .parse()
                .map_err(|_| E::custom("expected an unsigned decimal integer string"))
        }
    }

    deserializer.deserialize_any(UnsignedVisitor)
}

fn deserialize_optional_u8<'de, D>(deserializer: D) -> std::result::Result<Option<u8>, D::Error>
where
    D: Deserializer<'de>,
{
    struct OptionalU8Visitor;

    impl<'de> Visitor<'de> for OptionalU8Visitor {
        type Value = Option<u8>;

        fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("null, an unsigned byte, or its decimal string representation")
        }

        fn visit_none<E>(self) -> std::result::Result<Self::Value, E> {
            Ok(None)
        }

        fn visit_unit<E>(self) -> std::result::Result<Self::Value, E> {
            Ok(None)
        }

        fn visit_some<D>(self, deserializer: D) -> std::result::Result<Self::Value, D::Error>
        where
            D: Deserializer<'de>,
        {
            deserialize_u8(deserializer).map(Some)
        }
    }

    deserializer.deserialize_option(OptionalU8Visitor)
}

fn deserialize_byte_order<'de, D>(deserializer: D) -> std::result::Result<ByteOrder, D::Error>
where
    D: Deserializer<'de>,
{
    let value = <&str>::deserialize(deserializer)?;
    if ["ABCD", "AB", "BIG_ENDIAN", "BE"]
        .iter()
        .any(|alias| value.eq_ignore_ascii_case(alias))
    {
        Ok(ByteOrder::Abcd)
    } else if ["DCBA", "BA", "LITTLE_ENDIAN", "LE"]
        .iter()
        .any(|alias| value.eq_ignore_ascii_case(alias))
    {
        Ok(ByteOrder::Dcba)
    } else if ["BADC", "WORD_SWAP"]
        .iter()
        .any(|alias| value.eq_ignore_ascii_case(alias))
    {
        Ok(ByteOrder::Badc)
    } else if ["CDAB", "BYTE_SWAP"]
        .iter()
        .any(|alias| value.eq_ignore_ascii_case(alias))
    {
        Ok(ByteOrder::Cdab)
    } else {
        Err(de::Error::unknown_variant(
            value,
            &["ABCD", "DCBA", "BADC", "CDAB"],
        ))
    }
}

// ============================================================================
// ModbusChannelConfig (builder pattern)
// ============================================================================

/// Modbus channel configuration.
#[derive(Debug, Clone)]
pub struct ModbusChannelConfig {
    /// Connection mode (TCP or RTU)
    pub connection_mode: ConnectionMode,
    /// Target address for TCP (e.g., "192.168.1.100:502")
    pub address: String,
    /// Connection timeout (TCP only)
    pub connect_timeout: Duration,
    /// I/O operation timeout
    pub io_timeout: Duration,
    /// RTU serial device path (e.g., "/dev/ttyUSB0")
    #[cfg(feature = "modbus")]
    pub rtu_device: String,
    /// RTU baud rate (e.g., 9600, 19200, 115200)
    #[cfg(feature = "modbus")]
    pub baud_rate: u32,
    /// Point configurations
    pub points: Vec<PointConfig<ModbusAddress>>,
    /// Maximum registers per batch read (default: 125)
    pub max_batch_size: u16,
    /// Maximum gap between registers to allow merging (default: 10)
    pub max_gap: u16,
    /// Reconnect configuration
    pub reconnect: ReconnectConfig,
}

impl ModbusChannelConfig {
    /// Create a TCP configuration.
    pub fn tcp(address: impl Into<String>) -> Self {
        Self {
            connection_mode: ConnectionMode::Tcp,
            address: address.into(),
            connect_timeout: Duration::from_millis(DEFAULT_CONNECT_TIMEOUT_MS),
            io_timeout: Duration::from_millis(DEFAULT_IO_TIMEOUT_MS),
            #[cfg(feature = "modbus")]
            rtu_device: String::new(),
            #[cfg(feature = "modbus")]
            baud_rate: 9600,
            points: Vec::new(),
            max_batch_size: DEFAULT_MAX_BATCH_SIZE,
            max_gap: DEFAULT_MAX_GAP,
            reconnect: ReconnectConfig::default(),
        }
    }

    /// Create an RTU (serial) configuration.
    #[cfg(feature = "modbus")]
    pub fn rtu(device: impl Into<String>, baud_rate: u32) -> Self {
        Self {
            connection_mode: ConnectionMode::Rtu,
            address: String::new(),
            connect_timeout: Duration::from_millis(DEFAULT_CONNECT_TIMEOUT_MS),
            io_timeout: Duration::from_millis(DEFAULT_IO_TIMEOUT_MS),
            rtu_device: device.into(),
            baud_rate,
            points: Vec::new(),
            max_batch_size: DEFAULT_MAX_BATCH_SIZE,
            max_gap: DEFAULT_MAX_GAP,
            reconnect: ReconnectConfig::default(),
        }
    }

    /// Set connection timeout.
    pub fn with_connect_timeout(mut self, timeout: Duration) -> Self {
        self.connect_timeout = timeout;
        self
    }

    /// Set I/O timeout.
    pub fn with_io_timeout(mut self, timeout: Duration) -> Self {
        self.io_timeout = timeout;
        self
    }

    /// Add point configurations.
    pub fn with_points(mut self, points: Vec<PointConfig<ModbusAddress>>) -> Self {
        self.points = points;
        self
    }

    /// Set maximum batch size for register reads.
    pub fn with_max_batch_size(mut self, size: u16) -> Self {
        self.max_batch_size = size;
        self
    }

    /// Set maximum gap for merging consecutive registers.
    pub fn with_max_gap(mut self, gap: u16) -> Self {
        self.max_gap = gap;
        self
    }

    /// Set reconnect configuration.
    pub fn with_reconnect(mut self, config: ReconnectConfig) -> Self {
        self.reconnect = config;
        self
    }
}

#[cfg(test)]
mod tests {
    use aether_core::PointType;
    use serde_json::json;

    use super::parse_point_mapping;
    use crate::protocols::core::point::{ByteOrder, DataFormat};

    #[test]
    fn point_mapping_codec_preserves_runtime_aliases_without_owning_the_value() {
        let mapping = json!({
            "slave_id": "7",
            "function_code": "3",
            "register_address": "42",
            "data_type": "F32",
            "byte_order": "big_endian",
            "bit_position": "15"
        });

        let address = parse_point_mapping(PointType::Telemetry, 11, &mapping).unwrap();

        assert_eq!(address.slave_id, 7);
        assert_eq!(address.function_code, 3);
        assert_eq!(address.register, 42);
        assert_eq!(address.format, DataFormat::Float32);
        assert_eq!(address.byte_order, ByteOrder::Abcd);
        assert_eq!(address.bit_position, Some(15));
        assert_eq!(mapping["register_address"], "42");
    }

    #[test]
    fn point_mapping_codec_rejects_unknown_fields_and_out_of_range_bits() {
        let unknown = json!({
            "slave_id": 1,
            "function_code": 3,
            "register_address": 0,
            "surprise": true
        });
        assert!(
            parse_point_mapping(PointType::Telemetry, 12, &unknown)
                .unwrap_err()
                .to_string()
                .contains("unknown field")
        );

        let invalid_bit = json!({
            "slave_id": 1,
            "function_code": 3,
            "register_address": 0,
            "bit_position": 16
        });
        assert!(
            parse_point_mapping(PointType::Telemetry, 13, &invalid_bit)
                .unwrap_err()
                .to_string()
                .contains("0..15")
        );
    }

    #[test]
    fn point_mapping_codec_enforces_function_direction() {
        let mapping = json!({
            "slave_id": 1,
            "function_code": 5,
            "register_address": 0
        });

        assert!(parse_point_mapping(PointType::Telemetry, 14, &mapping).is_err());
        assert!(parse_point_mapping(PointType::Control, 14, &mapping).is_ok());
    }
}
