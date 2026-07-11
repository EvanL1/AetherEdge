//! Point configuration with protocol-specific addresses and SCADA categorization.

use aether_model::PointType;
use serde::{Deserialize, Serialize};

use crate::protocols::core::error::GatewayError;

/// Point configuration with protocol address and SCADA type.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PointConfig {
    pub id: u32,
    pub point_type: PointType,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub address: ProtocolAddress,
    #[serde(default)]
    pub transform: TransformConfig,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub poll_group: Option<String>,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

fn default_true() -> bool {
    true
}

impl PointConfig {
    pub fn new(id: u32, point_type: PointType, address: ProtocolAddress) -> Self {
        Self {
            id,
            point_type,
            name: None,
            address,
            transform: TransformConfig::default(),
            poll_group: None,
            enabled: true,
        }
    }

    pub fn telemetry(id: u32, address: ProtocolAddress) -> Self {
        Self::new(id, PointType::Telemetry, address)
    }

    pub fn signal(id: u32, address: ProtocolAddress) -> Self {
        Self::new(id, PointType::Signal, address)
    }

    pub fn control(id: u32, address: ProtocolAddress) -> Self {
        Self::new(id, PointType::Control, address)
    }

    pub fn adjustment(id: u32, address: ProtocolAddress) -> Self {
        Self::new(id, PointType::Adjustment, address)
    }

    #[must_use]
    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    #[must_use]
    pub fn with_transform(mut self, transform: TransformConfig) -> Self {
        self.transform = transform;
        self
    }
}

/// Protocol-specific address configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "protocol", content = "params")]
pub enum ProtocolAddress {
    Modbus(ModbusAddress),
    Iec104(Iec104Address),
    OpcUa(OpcUaAddress),
    Virtual(VirtualAddress),
    #[cfg(feature = "gpio")]
    Gpio(GpioAddress),
    #[cfg(feature = "can")]
    Can(CanAddress),
    Generic(String),
    #[cfg(feature = "dl645")]
    Dl645(Dl645Address),
    #[cfg(feature = "ble")]
    Ble(BleAddress),
    #[cfg(feature = "zigbee")]
    Zigbee(ZigbeeAddress),
    #[cfg(feature = "matter")]
    Matter(MatterAddress),
    #[cfg(feature = "iec61850")]
    Iec61850(Iec61850Address),
}

/// Virtual channel address (no physical device).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VirtualAddress {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    pub tag: String,
}

impl VirtualAddress {
    pub fn new(tag: impl Into<String>) -> Self {
        Self {
            group: None,
            tag: tag.into(),
        }
    }

    pub fn grouped(group: impl Into<String>, tag: impl Into<String>) -> Self {
        Self {
            group: Some(group.into()),
            tag: tag.into(),
        }
    }
}

/// DL/T 645-2007 point address (DI code only).
///
/// The DI code (Data Identifier) specifies which data item to read from the meter.
/// The meter address is configured at the channel level, not per-point.
///
/// # Format
///
/// Supports hexadecimal format with optional "0x" prefix:
/// - `"0x02010100"` - Phase A voltage
/// - `"02010100"` - same as above, without prefix
///
/// # Example
///
/// ```ignore
/// // Create address for A-phase voltage
/// let addr = Dl645Address::new(0x02010100);
///
/// // Parse from string
/// let addr = Dl645Address::parse("0x02010100")?;
/// ```
#[cfg(feature = "dl645")]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Dl645Address {
    /// DI code (Data Identifier, 4 bytes).
    /// Standard codes defined in DL/T 645-2007.
    pub di_code: u32,
}

#[cfg(feature = "dl645")]
impl Dl645Address {
    /// Create a new DL/T 645 address from DI code.
    #[must_use]
    pub fn new(di_code: u32) -> Self {
        Self { di_code }
    }

    /// Parse from hexadecimal string (with or without "0x" prefix).
    ///
    /// # Examples
    ///
    /// - `"0x02010100"` → di_code = 0x02010100
    /// - `"02010100"` → di_code = 0x02010100
    /// - `"00010000"` → di_code = 0x00010000 (total positive active energy)
    pub fn parse(s: &str) -> Result<Self, GatewayError> {
        let s = s.trim();

        // Remove optional "0x" or "0X" prefix
        let hex_str = s
            .strip_prefix("0x")
            .or_else(|| s.strip_prefix("0X"))
            .unwrap_or(s);

        // Validate: must be 8 hex characters
        if hex_str.len() != 8 {
            return Err(GatewayError::Config(format!(
                "Invalid DL/T 645 DI code: '{}'. Expected 8 hex characters (e.g., '02010100').",
                s
            )));
        }

        if !hex_str.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(GatewayError::Config(format!(
                "Invalid DL/T 645 DI code: '{}'. Must contain only hex digits.",
                s
            )));
        }

        let di_code = u32::from_str_radix(hex_str, 16)
            .map_err(|_| GatewayError::Config(format!("Failed to parse DI code: '{}'", s)))?;

        Ok(Self::new(di_code))
    }

    /// Get the DI code as a hex string (8 uppercase characters).
    #[must_use]
    pub fn to_hex_string(&self) -> String {
        format!("{:08X}", self.di_code)
    }
}

/// BLE GATT address: service UUID + characteristic UUID.
///
/// Identifies a specific GATT characteristic on a BLE peripheral.
/// The peripheral device address is configured at the channel level.
///
/// # Address Format
///
/// UUIDs support both short (16-bit) and full (128-bit) formats:
/// - Short: `"180f"` expands to `"0000180f-0000-1000-8000-00805f9b34fb"`
/// - Full: `"12345678-1234-1234-1234-123456789abc"`
#[cfg(feature = "ble")]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BleAddress {
    /// GATT Service UUID (short or full format)
    pub service_uuid: String,
    /// GATT Characteristic UUID (short or full format)
    pub characteristic_uuid: String,
    /// Value data format for parsing raw bytes
    #[serde(default)]
    pub data_format: DataFormat,
    /// Whether to subscribe via Notify (otherwise poll-read)
    #[serde(default)]
    pub notify: bool,
}

/// Zigbee device address for ZCL attribute identification.
///
/// Uniquely identifies a data point on a Zigbee device by combining the device's
/// IEEE address with the ZCL endpoint, cluster, and attribute IDs.
///
/// # Example
///
/// ```ignore
/// let addr = ZigbeeAddress {
///     ieee_address: 0x00124B0018ED1234,
///     endpoint: 1,
///     cluster_id: 0x0402,    // Temperature Measurement
///     attribute_id: 0x0000,  // MeasuredValue
/// };
/// ```
#[cfg(feature = "zigbee")]
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ZigbeeAddress {
    /// Device IEEE address (64-bit).
    pub ieee_address: u64,
    /// Zigbee endpoint (1-254).
    pub endpoint: u8,
    /// ZCL Cluster ID.
    pub cluster_id: u16,
    /// ZCL Attribute ID.
    pub attribute_id: u16,
}

/// Matter attribute address (endpoint/cluster/attribute path).
///
/// Identifies a specific attribute on a Matter device using the
/// Matter data model hierarchy: Endpoint -> Cluster -> Attribute.
///
/// # Format
///
/// Address string: `"endpoint/cluster_id/attribute_id"`
/// Supports hex (0x prefix) and decimal for cluster and attribute IDs.
///
/// # Examples
///
/// - `"1/0x0006/0x0000"` - On/Off attribute on endpoint 1
/// - `"1/0x0402/0x0000"` - Temperature measurement
/// - `"1/6/0"` - On/Off in decimal
#[cfg(feature = "matter")]
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct MatterAddress {
    /// Matter Endpoint (0-65535)
    pub endpoint: u16,
    /// Cluster ID (e.g., 0x0006 = On/Off, 0x0402 = Temperature)
    pub cluster_id: u32,
    /// Attribute ID within the cluster
    pub attribute_id: u32,
}

#[cfg(feature = "matter")]
impl MatterAddress {
    /// Create a new Matter address.
    pub fn new(endpoint: u16, cluster_id: u32, attribute_id: u32) -> Self {
        Self {
            endpoint,
            cluster_id,
            attribute_id,
        }
    }
}

/// IEC 61850 MMS variable address.
///
/// Identifies a data attribute on an IEC 61850 server via its MMS path.
///
/// # Format
///
/// ```yaml
/// protocol: Iec61850
/// params:
///   domain: "simpleIOGenericIO"
///   item: "GGIO1$MX$AnIn1$mag$f"
/// ```
///
/// The `domain` is the IED logical device name and `item` is the MMS item ID
/// using `$` as separator with the functional constraint embedded (e.g., `$MX$`).
#[cfg(feature = "iec61850")]
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Iec61850Address {
    /// Logical device name (MMS domain), e.g. "simpleIOGenericIO"
    pub domain: String,
    /// MMS item ID with functional constraint, e.g. "GGIO1$MX$AnIn1$mag$f"
    pub item: String,
    /// IEC 61850 control model (only relevant for Control/Adjustment points):
    ///   1 = direct-with-normal-security  (default)
    ///   2 = SBO-with-normal-security
    ///   3 = direct-with-enhanced-security
    ///   4 = SBO-with-enhanced-security (SBOw)
    #[serde(default = "default_ctrl_model")]
    pub ctrl_model: u8,
}

#[cfg(feature = "iec61850")]
fn default_ctrl_model() -> u8 {
    1
}

#[cfg(feature = "iec61850")]
impl Iec61850Address {
    pub fn new(domain: impl Into<String>, item: impl Into<String>) -> Self {
        Self {
            domain: domain.into(),
            item: item.into(),
            ctrl_model: 1,
        }
    }

    /// Parse from IEC 61850 object reference string.
    ///
    /// Accepts formats:
    /// - `"LD/LNname.DOname.DAname[FC]"` → converts dots to `$` and inserts FC
    /// - `"domain:item"` → split by colon, already in MMS form
    pub fn parse(s: &str) -> Result<Self, GatewayError> {
        if let Some((domain, item)) = s.split_once(':') {
            return Ok(Self::new(domain.trim(), item.trim()));
        }
        if let Some((domain, rest)) = s.split_once('/') {
            let item = rest.replace('.', "$");
            return Ok(Self::new(domain.trim(), item));
        }
        Err(GatewayError::Config(format!(
            "Invalid IEC 61850 address: '{}'. Expected 'domain/item' or 'domain:item$...'",
            s
        )))
    }
}

/// GPIO pin direction.
#[cfg(feature = "gpio")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GpioDirection {
    /// Input pin (DI - Digital Input).
    Input,
    /// Output pin (DO - Digital Output).
    Output,
}

/// GPIO address for hardware DI/DO control.
#[cfg(feature = "gpio")]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GpioAddress {
    /// GPIO chip name (e.g., "gpiochip0").
    pub chip: String,

    /// Pin number/offset.
    pub pin: u32,

    /// Pin direction.
    pub direction: GpioDirection,

    /// Active low (invert logic).
    #[serde(default)]
    pub active_low: bool,
}

#[cfg(feature = "gpio")]
impl GpioAddress {
    /// Create a digital input address.
    pub fn digital_input(chip: impl Into<String>, pin: u32) -> Self {
        Self {
            chip: chip.into(),
            pin,
            direction: GpioDirection::Input,
            active_low: false,
        }
    }

    /// Create a digital output address.
    pub fn digital_output(chip: impl Into<String>, pin: u32) -> Self {
        Self {
            chip: chip.into(),
            pin,
            direction: GpioDirection::Output,
            active_low: false,
        }
    }

    /// Set active low mode.
    #[must_use]
    pub fn with_active_low(mut self, active_low: bool) -> Self {
        self.active_low = active_low;
        self
    }
}

/// CAN bus address for raw CAN and J1939 protocols.
///
/// Supports bit-level extraction from CAN frame data.
/// Format string: "can_id:byte_offset:bit_position:bit_length"
#[cfg(feature = "can")]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CanAddress {
    /// CAN frame ID (11-bit standard or 29-bit extended).
    pub can_id: u32,

    /// Byte offset in CAN data field (0-7).
    pub byte_offset: u8,

    /// Bit position within the starting byte (0-7, LSB=0).
    pub bit_position: u8,

    /// Bit length to extract (1-64).
    pub bit_length: u8,
}

#[cfg(feature = "can")]
impl CanAddress {
    /// Create a new CAN address.
    pub fn new(can_id: u32, byte_offset: u8, bit_position: u8, bit_length: u8) -> Self {
        Self {
            can_id,
            byte_offset,
            bit_position,
            bit_length,
        }
    }

    /// Create a CAN address for a 16-bit unsigned value at the given byte offset.
    pub fn uint16(can_id: u32, byte_offset: u8) -> Self {
        Self::new(can_id, byte_offset, 0, 16)
    }

    /// Create a CAN address for an 8-bit unsigned value at the given byte offset.
    pub fn uint8(can_id: u32, byte_offset: u8) -> Self {
        Self::new(can_id, byte_offset, 0, 8)
    }

    /// Parse from string format: "can_id:byte_offset:bit_position:bit_length"
    pub fn parse(s: &str) -> Result<Self, GatewayError> {
        let parts: Vec<&str> = s.split(':').collect();
        if parts.len() != 4 {
            return Err(GatewayError::Config(format!(
                "Invalid CAN address format '{}', expected 'can_id:byte_offset:bit_pos:bit_len'",
                s
            )));
        }

        let can_id = parse_can_id(parts[0])?;
        let byte_offset = parts[1]
            .parse::<u8>()
            .map_err(|_| GatewayError::Config(format!("Invalid byte_offset: {}", parts[1])))?;
        let bit_position = parts[2]
            .parse::<u8>()
            .map_err(|_| GatewayError::Config(format!("Invalid bit_position: {}", parts[2])))?;
        let bit_length = parts[3]
            .parse::<u8>()
            .map_err(|_| GatewayError::Config(format!("Invalid bit_length: {}", parts[3])))?;

        // Validate ranges
        if byte_offset > 7 {
            return Err(GatewayError::Config(format!(
                "byte_offset {} exceeds CAN frame size (0-7)",
                byte_offset
            )));
        }
        if bit_position > 7 {
            return Err(GatewayError::Config(format!(
                "bit_position {} must be 0-7",
                bit_position
            )));
        }
        if bit_length == 0 || bit_length > 64 {
            return Err(GatewayError::Config(format!(
                "bit_length {} must be 1-64",
                bit_length
            )));
        }

        Ok(Self::new(can_id, byte_offset, bit_position, bit_length))
    }
}

/// Parse CAN ID from string (supports decimal and hex "0x" prefix).
#[cfg(feature = "can")]
fn parse_can_id(s: &str) -> Result<u32, GatewayError> {
    let s = s.trim();
    if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        u32::from_str_radix(hex, 16)
            .map_err(|_| GatewayError::Config(format!("Invalid hex CAN ID: {}", s)))
    } else {
        s.parse::<u32>()
            .map_err(|_| GatewayError::Config(format!("Invalid CAN ID: {}", s)))
    }
}

/// Modbus point address.
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

/// IEC 60870-5-104 address.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Iec104Address {
    pub ioa: u32,
    pub type_id: u8,
    pub common_address: u16,
}

impl Iec104Address {
    pub fn new(ioa: u32, type_id: u8, common_address: u16) -> Self {
        Self {
            ioa,
            type_id,
            common_address,
        }
    }
}

/// OPC UA address.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpcUaAddress {
    pub node_id: String,
    #[serde(default)]
    pub namespace_index: u16,
}

impl OpcUaAddress {
    pub fn new(node_id: impl Into<String>, namespace_index: u16) -> Self {
        Self {
            node_id: node_id.into(),
            namespace_index,
        }
    }
}

/// Data format for protocol values (case-insensitive deserialization).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
pub enum DataFormat {
    Bool,
    #[default]
    UInt16,
    Int16,
    UInt32,
    Int32,
    UInt64,
    Int64,
    Float32,
    Float64,
    String,
}

impl DataFormat {
    pub fn register_count(&self) -> u16 {
        match self {
            Self::Bool | Self::UInt16 | Self::Int16 => 1,
            Self::UInt32 | Self::Int32 | Self::Float32 => 2,
            Self::UInt64 | Self::Int64 | Self::Float64 => 4,
            Self::String => 8, // Default 16 characters
        }
    }
}

impl<'de> Deserialize<'de> for DataFormat {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct DataFormatVisitor;

        impl<'de> serde::de::Visitor<'de> for DataFormatVisitor {
            type Value = DataFormat;

            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                formatter.write_str("a data format string like 'int32', 'uint16', 'float32', etc.")
            }

            fn visit_str<E>(self, value: &str) -> Result<DataFormat, E>
            where
                E: serde::de::Error,
            {
                match value.to_lowercase().as_str() {
                    "bool" | "boolean" => Ok(DataFormat::Bool),
                    "uint16" | "u16" => Ok(DataFormat::UInt16),
                    "int16" | "i16" => Ok(DataFormat::Int16),
                    "uint32" | "u32" => Ok(DataFormat::UInt32),
                    "int32" | "i32" => Ok(DataFormat::Int32),
                    "uint64" | "u64" => Ok(DataFormat::UInt64),
                    "int64" | "i64" => Ok(DataFormat::Int64),
                    "float32" | "f32" | "float" => Ok(DataFormat::Float32),
                    "float64" | "f64" | "double" => Ok(DataFormat::Float64),
                    "string" => Ok(DataFormat::String),
                    _ => Err(serde::de::Error::unknown_variant(
                        value,
                        &[
                            "bool", "uint16", "int16", "uint32", "int32", "uint64", "int64",
                            "float32", "float64", "string",
                        ],
                    )),
                }
            }
        }

        deserializer.deserialize_str(DataFormatVisitor)
    }
}

/// Byte order for multi-byte values (supports serde aliases: BE/LE/WORD_SWAP/BYTE_SWAP).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum ByteOrder {
    #[default]
    #[serde(alias = "big_endian", alias = "BIG_ENDIAN", alias = "BE")]
    Abcd,
    #[serde(alias = "little_endian", alias = "LITTLE_ENDIAN", alias = "LE")]
    Dcba,
    #[serde(alias = "WORD_SWAP", alias = "word_swap")]
    Badc,
    #[serde(alias = "BYTE_SWAP", alias = "byte_swap")]
    Cdab,
}

impl ByteOrder {
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Abcd => "ABCD",
            Self::Dcba => "DCBA",
            Self::Badc => "BADC",
            Self::Cdab => "CDAB",
        }
    }
}

/// Data transformation: result = raw * scale + offset.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TransformConfig {
    #[serde(default = "default_scale")]
    pub scale: f64,
    #[serde(default)]
    pub offset: f64,
    #[serde(default)]
    pub reverse: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deadband: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min_value: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_value: Option<f64>,
}

fn default_scale() -> f64 {
    1.0
}

impl TransformConfig {
    pub fn linear(scale: f64, offset: f64) -> Self {
        Self {
            scale,
            offset,
            ..Default::default()
        }
    }

    pub fn apply(&self, raw: f64) -> f64 {
        raw * self.scale + self.offset
    }

    pub fn reverse_apply(&self, value: f64) -> Result<f64, GatewayError> {
        if self.scale == 0.0 {
            return Err(GatewayError::DataConversion(
                "Cannot reverse transform: scale is zero".into(),
            ));
        }
        Ok((value - self.offset) / self.scale)
    }

    pub fn apply_bool(&self, raw: bool) -> bool {
        if self.reverse { !raw } else { raw }
    }
}

#[cfg(test)]
#[allow(clippy::disallowed_methods)] // unwrap in tests
mod tests {
    use super::*;

    #[test]
    fn test_modbus_address() {
        let addr = ModbusAddress::holding_register(1, 100, DataFormat::Float32);
        assert_eq!(addr.slave_id, 1);
        assert_eq!(addr.function_code, 3);
        assert_eq!(addr.register, 100);
        assert_eq!(addr.register_count(), 2);
    }

    #[test]
    fn test_transform() {
        let t = TransformConfig::linear(0.1, 10.0);
        assert_eq!(t.apply(100.0), 20.0); // 100 * 0.1 + 10 = 20
        assert_eq!(t.reverse_apply(20.0).unwrap(), 100.0);
    }

    #[test]
    fn test_transform_zero_scale() {
        let t = TransformConfig::linear(0.0, 10.0);
        assert!(t.reverse_apply(20.0).is_err());
    }

    #[test]
    fn test_data_format_register_count() {
        assert_eq!(DataFormat::UInt16.register_count(), 1);
        assert_eq!(DataFormat::Float32.register_count(), 2);
        assert_eq!(DataFormat::Float64.register_count(), 4);
    }

    #[test]
    fn test_data_format_case_insensitive() {
        let formats = vec![
            ("\"int32\"", DataFormat::Int32),
            ("\"Int32\"", DataFormat::Int32),
            ("\"INT32\"", DataFormat::Int32),
            ("\"i32\"", DataFormat::Int32),
            ("\"float32\"", DataFormat::Float32),
            ("\"Float32\"", DataFormat::Float32),
        ];

        for (json, expected) in formats {
            let result: DataFormat = serde_json::from_str(json).unwrap();
            assert_eq!(result, expected, "Failed for {}", json);
        }
    }
}
