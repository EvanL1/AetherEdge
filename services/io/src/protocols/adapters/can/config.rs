//! CAN protocol configuration types

#[cfg(target_os = "linux")]
use aether_config::io::MAX_CHANNEL_TIMING_MS;
use aether_core::PointType;
#[cfg(any(target_os = "linux", test))]
use serde::de::{self, Visitor};
use serde::{Deserialize, Serialize};
#[cfg(any(target_os = "linux", test))]
use serde_json::Value;
use std::collections::HashMap;

#[cfg(any(target_os = "linux", test))]
use crate::protocols::core::error::{GatewayError, Result};

/// CAN client configuration.
#[derive(Debug, Clone)]
pub struct CanConfig {
    /// CAN interface name (e.g., "can0").
    pub can_interface: String,

    /// CAN bitrate (bits per second).
    pub bitrate: u32,

    /// Connection/open timeout in milliseconds.
    pub connect_timeout_ms: u64,

    /// Read (frame receive) timeout in milliseconds.
    pub read_timeout_ms: u64,

    /// Reconnect interval in milliseconds.
    pub retry_interval_ms: u64,

    /// RX polling interval in milliseconds.
    pub rx_poll_interval_ms: u64,

    /// Data reading interval in milliseconds.
    pub data_read_interval_ms: u64,
}

impl Default for CanConfig {
    fn default() -> Self {
        Self {
            can_interface: "can0".to_string(),
            bitrate: 250000,
            connect_timeout_ms: 3000,
            read_timeout_ms: 3000,
            retry_interval_ms: 2000,
            rx_poll_interval_ms: 50,
            data_read_interval_ms: 1000,
        }
    }
}

/// CAN data type enumeration.
///
/// Using an enum instead of String eliminates heap allocation per point
/// and enables fast integer-based matching in the decoder hot path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum CanDataType {
    /// Unsigned 8-bit integer
    UInt8,
    /// Unsigned 16-bit integer (default)
    #[default]
    UInt16,
    /// Signed 16-bit integer
    Int16,
    /// Unsigned 32-bit integer
    UInt32,
    /// Signed 32-bit integer
    Int32,
    /// 32-bit floating point
    Float32,
}

/// CAN point mapping structure
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CanPoint {
    /// Unique point identifier (numeric)
    pub point_id: u32,
    /// Point type (T/S/C/A)
    pub point_type: PointType,
    /// CAN-ID (e.g., 0x351)
    pub can_id: u32,
    /// Byte offset in CAN data field (0-7)
    pub byte_offset: u8,
    /// Bit starting position within byte (0-7, LSB=0)
    pub bit_position: u8,
    /// Bit length (2/8/16/32/64)
    pub bit_length: u8,
    /// Data type for interpretation
    pub data_type: CanDataType,
    /// Scale factor for linear transformation (value = raw * scale + offset)
    #[serde(default = "default_scale")]
    pub scale: f64,
    /// Offset for linear transformation
    #[serde(default)]
    pub offset: f64,
}

fn default_scale() -> f64 {
    1.0
}

/// Persisted CAN point mapping decoded by both validation and runtime construction.
#[cfg(any(target_os = "linux", test))]
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CanPointMapping {
    #[serde(deserialize_with = "deserialize_can_id")]
    can_id: u32,
    start_bit: Option<u8>,
    byte_offset: Option<u8>,
    bit_position: Option<u8>,
    bit_length: u8,
    #[serde(default)]
    data_type: CanDataType,
    #[serde(default = "default_scale")]
    scale: f64,
    #[serde(default)]
    offset: f64,
}

/// Parse and validate a CAN point mapping without cloning its JSON value.
///
/// CAN exposes a read-only acquisition plane, so only telemetry and signal
/// point mappings are accepted.
#[cfg(any(target_os = "linux", test))]
pub(crate) fn parse_point_mapping(
    mapping: &Value,
    point_type: PointType,
    point_id: u32,
) -> Result<CanPoint> {
    if !matches!(point_type, PointType::Telemetry | PointType::Signal) {
        return Err(mapping_error(
            point_id,
            &format!("CAN exposes read-only telemetry/signal mappings, not {point_type:?}"),
        ));
    }

    for field in ["start_bit", "byte_offset", "bit_position"] {
        if mapping.get(field).is_some_and(Value::is_null) {
            return Err(mapping_error(
                point_id,
                &format!("{field} must be an unsigned integer when present"),
            ));
        }
    }

    let mapping = CanPointMapping::deserialize(mapping)
        .map_err(|error| mapping_error(point_id, &format!("invalid mapping object: {error}")))?;
    let start_bit = match (mapping.start_bit, mapping.byte_offset, mapping.bit_position) {
        (Some(start_bit), None, None) if start_bit <= 63 => start_bit,
        (None, Some(byte_offset), bit_position)
            if byte_offset <= 7 && bit_position.unwrap_or_default() <= 7 =>
        {
            byte_offset * 8 + bit_position.unwrap_or_default()
        },
        (Some(_), Some(_), _) => {
            return Err(mapping_error(
                point_id,
                "use start_bit or byte_offset, not both",
            ));
        },
        (Some(_), None, Some(_)) => {
            return Err(mapping_error(
                point_id,
                "bit_position is valid only with byte_offset",
            ));
        },
        (None, None, _) => {
            return Err(mapping_error(
                point_id,
                "start_bit or byte_offset is required",
            ));
        },
        (Some(_), None, None) => {
            return Err(mapping_error(point_id, "start_bit must be in 0..63"));
        },
        (None, Some(_), _) => {
            return Err(mapping_error(
                point_id,
                "byte_offset and bit_position must each be in 0..7",
            ));
        },
    };

    if !(1..=64).contains(&mapping.bit_length)
        || u16::from(start_bit) + u16::from(mapping.bit_length) > 64
    {
        return Err(mapping_error(
            point_id,
            "bit layout must fit in a 64-bit payload",
        ));
    }

    let width_matches = match mapping.data_type {
        CanDataType::UInt8 => matches!(mapping.bit_length, 2 | 8),
        CanDataType::UInt16 | CanDataType::Int16 => mapping.bit_length == 16,
        CanDataType::UInt32 | CanDataType::Int32 | CanDataType::Float32 => mapping.bit_length == 32,
    };
    if !width_matches {
        return Err(mapping_error(
            point_id,
            &format!(
                "bit_length {} does not match {:?}",
                mapping.bit_length, mapping.data_type
            ),
        ));
    }
    if !mapping.scale.is_finite() || !mapping.offset.is_finite() {
        return Err(mapping_error(point_id, "scale and offset must be finite"));
    }

    Ok(CanPoint {
        point_id,
        point_type,
        can_id: mapping.can_id,
        byte_offset: start_bit / 8,
        bit_position: start_bit % 8,
        bit_length: mapping.bit_length,
        data_type: mapping.data_type,
        scale: mapping.scale,
        offset: mapping.offset,
    })
}

#[cfg(any(target_os = "linux", test))]
fn mapping_error(point_id: u32, reason: &str) -> GatewayError {
    GatewayError::Config(format!(
        "invalid CAN mapping for point {point_id}: {reason}"
    ))
}

#[cfg(any(target_os = "linux", test))]
fn deserialize_can_id<'de, D>(deserializer: D) -> std::result::Result<u32, D::Error>
where
    D: serde::Deserializer<'de>,
{
    struct CanIdVisitor;

    impl<'de> Visitor<'de> for CanIdVisitor {
        type Value = u32;

        fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("a u32 CAN ID or a hexadecimal string prefixed with 0x")
        }

        fn visit_u64<E>(self, value: u64) -> std::result::Result<Self::Value, E>
        where
            E: de::Error,
        {
            u32::try_from(value).map_err(|_| E::custom("CAN ID exceeds u32"))
        }

        fn visit_i64<E>(self, value: i64) -> std::result::Result<Self::Value, E>
        where
            E: de::Error,
        {
            u32::try_from(value).map_err(|_| E::custom("CAN ID must be in the u32 range"))
        }

        fn visit_str<E>(self, value: &str) -> std::result::Result<Self::Value, E>
        where
            E: de::Error,
        {
            let hex = value
                .strip_prefix("0x")
                .or_else(|| value.strip_prefix("0X"))
                .ok_or_else(|| E::custom("CAN ID string must use a 0x prefix"))?;
            u32::from_str_radix(hex, 16).map_err(|_| E::custom("invalid hexadecimal CAN ID"))
        }
    }

    deserializer.deserialize_any(CanIdVisitor)
}

/// CAN channel parameters configuration (deserialized from parameters_json).
///
/// # Example JSON
/// ```json
/// {
///     "device": "can0",
///     "bitrate": 250000,
///     "connect_timeout_ms": 3000,
///     "read_timeout_ms": 3000,
///     "retry_interval_ms": 2000,
///     "rx_poll_interval_ms": 50
/// }
/// ```
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CanChannelParamsConfig {
    /// CAN device name (e.g., "can0", "vcan0").
    /// Also accepts the legacy key "interface".
    #[serde(default = "default_can_device", alias = "interface")]
    pub device: String,

    /// CAN bitrate in bits per second.
    #[serde(default = "default_bitrate")]
    pub bitrate: u32,

    /// Connection/open timeout in milliseconds.
    #[serde(default = "default_connect_timeout")]
    pub connect_timeout_ms: u64,

    /// Read (frame receive) timeout in milliseconds.
    #[serde(default = "default_read_timeout")]
    pub read_timeout_ms: u64,

    /// Reconnect interval in milliseconds.
    #[serde(default = "default_retry_interval")]
    pub retry_interval_ms: u64,

    /// RX polling interval in milliseconds.
    #[serde(default = "default_rx_poll_interval")]
    pub rx_poll_interval_ms: u64,

    /// Data reading interval in milliseconds.
    #[serde(default = "default_data_read_interval")]
    pub data_read_interval_ms: u64,
}

fn default_can_device() -> String {
    "can0".to_string()
}

fn default_bitrate() -> u32 {
    250000
}

fn default_connect_timeout() -> u64 {
    3000
}

fn default_read_timeout() -> u64 {
    3000
}

fn default_retry_interval() -> u64 {
    2000
}

fn default_rx_poll_interval() -> u64 {
    50
}

fn default_data_read_interval() -> u64 {
    1000
}

impl CanChannelParamsConfig {
    #[cfg(target_os = "linux")]
    pub(crate) fn validate(&self) -> Result<()> {
        if self.device.trim().is_empty() {
            return Err(GatewayError::Config(
                "CAN device must be non-empty".to_owned(),
            ));
        }
        if self.bitrate == 0 {
            return Err(GatewayError::Config(
                "CAN bitrate must be greater than zero".to_owned(),
            ));
        }
        for (name, value) in [
            ("connect_timeout_ms", self.connect_timeout_ms),
            ("read_timeout_ms", self.read_timeout_ms),
            ("retry_interval_ms", self.retry_interval_ms),
            ("rx_poll_interval_ms", self.rx_poll_interval_ms),
            ("data_read_interval_ms", self.data_read_interval_ms),
        ] {
            if !(1..=MAX_CHANNEL_TIMING_MS).contains(&value) {
                return Err(GatewayError::Config(format!(
                    "CAN {name} must be between 1 and {MAX_CHANNEL_TIMING_MS}"
                )));
            }
        }
        Ok(())
    }

    /// Convert to CanConfig.
    pub fn into_config(self) -> CanConfig {
        CanConfig {
            can_interface: self.device,
            bitrate: self.bitrate,
            connect_timeout_ms: self.connect_timeout_ms,
            read_timeout_ms: self.read_timeout_ms,
            retry_interval_ms: self.retry_interval_ms,
            rx_poll_interval_ms: self.rx_poll_interval_ms,
            data_read_interval_ms: self.data_read_interval_ms,
        }
    }
}

/// CAN frame data - stack-allocated fixed buffer for up to 8 bytes
#[derive(Debug, Clone, Copy, Default)]
pub struct CanFrameData {
    data: [u8; 8],
    len: u8,
}

impl CanFrameData {
    /// Create from a byte slice (copies up to 8 bytes)
    pub fn from_slice(bytes: &[u8]) -> Self {
        let mut data = [0u8; 8];
        let len = bytes.len().min(8) as u8;
        data[..len as usize].copy_from_slice(&bytes[..len as usize]);
        Self { data, len }
    }

    /// Get the data as a slice
    pub fn as_slice(&self) -> &[u8] {
        &self.data[..self.len as usize]
    }
}

/// CAN frame cache - stores the latest received frame for each CAN-ID
/// Uses fixed-size arrays instead of Vec to avoid heap allocation per frame
#[derive(Debug, Clone, Default)]
pub struct CanFrameCache {
    /// Map from CAN-ID to frame data (fixed 8-byte buffer + length)
    frames: HashMap<u32, CanFrameData>,
}

impl CanFrameCache {
    /// Create a new empty frame cache
    pub fn new() -> Self {
        Self {
            frames: HashMap::new(),
        }
    }

    /// Update cache with a new frame (no heap allocation for the data)
    pub fn update(&mut self, can_id: u32, data: &[u8]) {
        self.frames.insert(can_id, CanFrameData::from_slice(data));
    }

    /// Get the latest frame data for a CAN-ID
    pub fn get(&self, can_id: u32) -> Option<&[u8]> {
        self.frames.get(&can_id).map(|f| f.as_slice())
    }

    /// Iterate cached frames (used by tracing-support diagnostic logging)
    #[cfg(feature = "tracing-support")]
    pub fn iter(&self) -> impl Iterator<Item = (&u32, &CanFrameData)> {
        self.frames.iter()
    }
}

/// LYNK Serial CAN protocol CAN-IDs
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u32)]
pub enum LynkCanId {
    /// Battery Limits (1s period)
    BatteryLimits = 0x351,
    /// Battery Capacity Information (1s period)
    BatteryCapacity = 0x354,
    /// Battery Status (SOC/SOH) (1s period)
    BatteryStatus = 0x355,
    /// Battery Measurements (voltage/current/temp) (1s period)
    BatteryMeasurements = 0x356,
    /// Battery Alarms & Warnings (1s period)
    BatteryAlarms = 0x35A,
    /// Manufacturer Name ASCII (10s period)
    ManufacturerName = 0x35E,
    /// Model Name Upper ASCII (10s period)
    ModelNameUpper = 0x370,
    /// Model Name Lower ASCII (10s period)
    ModelNameLower = 0x371,
    /// Firmware Version (10s period)
    FirmwareVersion = 0x372,
    /// Protocol Version (10s period)
    ProtocolVersion = 0x373,
}

impl LynkCanId {
    /// Try to create from u32
    pub fn from_u32(id: u32) -> Option<Self> {
        match id {
            0x351 => Some(Self::BatteryLimits),
            0x354 => Some(Self::BatteryCapacity),
            0x355 => Some(Self::BatteryStatus),
            0x356 => Some(Self::BatteryMeasurements),
            0x35A => Some(Self::BatteryAlarms),
            0x35E => Some(Self::ManufacturerName),
            0x370 => Some(Self::ModelNameUpper),
            0x371 => Some(Self::ModelNameLower),
            0x372 => Some(Self::FirmwareVersion),
            0x373 => Some(Self::ProtocolVersion),
            _ => None,
        }
    }

    /// Check if this is a LYNK protocol CAN-ID
    pub fn is_lynk_id(id: u32) -> bool {
        Self::from_u32(id).is_some()
    }
}

#[cfg(test)]
mod mapping_tests {
    use aether_core::PointType;
    use serde_json::json;

    use super::{CanDataType, parse_point_mapping};

    #[test]
    fn mapping_codec_accepts_both_canonical_layout_forms() {
        let absolute = json!({
            "can_id": "0x351",
            "start_bit": 8,
            "bit_length": 16,
            "data_type": "int16",
            "scale": 0.1,
            "offset": -4.0
        });
        let point = parse_point_mapping(&absolute, PointType::Telemetry, 10).unwrap();
        assert_eq!(point.can_id, 0x351);
        assert_eq!(point.byte_offset, 1);
        assert_eq!(point.bit_position, 0);
        assert_eq!(point.bit_length, 16);
        assert_eq!(point.data_type, CanDataType::Int16);
        assert_eq!(point.scale, 0.1);
        assert_eq!(point.offset, -4.0);

        let byte_relative = json!({
            "can_id": 0x355,
            "byte_offset": 7,
            "bit_position": 0,
            "bit_length": 8,
            "data_type": "uint8"
        });
        let point = parse_point_mapping(&byte_relative, PointType::Signal, 11).unwrap();
        assert_eq!(point.can_id, 0x355);
        assert_eq!(point.byte_offset, 7);
        assert_eq!(point.bit_position, 0);
    }

    #[test]
    fn mapping_codec_rejects_commands_and_unknown_fields() {
        let mapping = json!({
            "can_id": 0x351,
            "start_bit": 0,
            "bit_length": 16,
            "unexpected": true
        });

        assert!(parse_point_mapping(&mapping, PointType::Control, 12).is_err());
        assert!(
            parse_point_mapping(&mapping, PointType::Telemetry, 12)
                .unwrap_err()
                .to_string()
                .contains("unknown field")
        );
    }

    #[test]
    fn mapping_codec_fails_closed_for_invalid_ids_and_bit_layouts() {
        for mapping in [
            json!({"can_id":"351","start_bit":0,"bit_length":16}),
            json!({"can_id":"0x100000000","start_bit":0,"bit_length":16}),
            json!({"can_id":1,"start_bit":0,"byte_offset":0,"bit_length":16}),
            json!({"can_id":1,"start_bit":0,"bit_position":0,"bit_length":16}),
            json!({"can_id":1,"start_bit":null,"byte_offset":0,"bit_length":16}),
            json!({"can_id":1,"bit_position":0,"bit_length":16}),
            json!({"can_id":1,"byte_offset":8,"bit_length":16}),
            json!({"can_id":1,"byte_offset":0,"bit_position":8,"bit_length":16}),
            json!({"can_id":1,"start_bit":64,"bit_length":16}),
            json!({"can_id":1,"start_bit":60,"bit_length":16}),
            json!({"can_id":1,"start_bit":0,"bit_length":8,"data_type":"uint16"}),
            json!({"can_id":1,"start_bit":0,"bit_length":16,"data_type":"opaque"}),
        ] {
            assert!(
                parse_point_mapping(&mapping, PointType::Telemetry, 13).is_err(),
                "mapping unexpectedly accepted: {mapping}"
            );
        }
    }
}

/// Turn a configured millisecond interval into a tick period.
///
/// `tokio::time::interval` panics on a zero period and the workspace release
/// profile sets `panic = "abort"`, so a channel config carrying `0` would take
/// the whole IO service down rather than just failing that channel.
/// Only the Linux socket client drives a ticker; `test` keeps it reachable for
/// the unit tests on every platform.
#[cfg(any(target_os = "linux", test))]
pub(crate) fn tick_period(configured_ms: u64) -> std::time::Duration {
    std::time::Duration::from_millis(configured_ms.max(1))
}

#[cfg(test)]
mod tick_period_tests {
    use super::tick_period;

    #[test]
    fn a_zero_configured_interval_is_clamped_instead_of_panicking() {
        assert_eq!(tick_period(0), std::time::Duration::from_millis(1));
    }

    #[test]
    fn a_configured_interval_is_preserved() {
        assert_eq!(tick_period(50), std::time::Duration::from_millis(50));
    }
}
