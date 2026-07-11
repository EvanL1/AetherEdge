//! Core type definitions for AetherEMS.
//!
//! These types are shared between firmware and Linux gateway layers.

use core::fmt;

/// Four Remote Point Types used in industrial SCADA systems.
///
/// These types correspond to the standard IEC "Four Remote" classification:
/// - T (Telemetry): Analog measurements (YC in Chinese standards)
/// - S (Signal): Digital status (YX in Chinese standards)
/// - C (Control): Digital commands (YK in Chinese standards)
/// - A (Adjustment): Analog setpoints (YT in Chinese standards)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[repr(u8)]
pub enum PointType {
    /// T - Telemetry - Analog measurements
    #[cfg_attr(
        feature = "serde",
        serde(rename = "T", alias = "YC", alias = "yc", alias = "telemetry")
    )]
    Telemetry = 0,

    /// S - Signal - Digital status
    #[cfg_attr(
        feature = "serde",
        serde(rename = "S", alias = "YX", alias = "yx", alias = "signal")
    )]
    Signal = 1,

    /// C - Control - Digital commands
    #[cfg_attr(
        feature = "serde",
        serde(rename = "C", alias = "YK", alias = "yk", alias = "control")
    )]
    Control = 2,

    /// A - Adjustment - Analog setpoints
    #[cfg_attr(
        feature = "serde",
        serde(
            rename = "A",
            alias = "YT",
            alias = "yt",
            alias = "adjustment",
            alias = "setpoint"
        )
    )]
    Adjustment = 3,
}

// Implement JsonSchema for PointType when the schema feature is enabled
#[cfg(feature = "schema")]
impl schemars::JsonSchema for PointType {
    fn schema_name() -> String {
        "PointType".to_owned()
    }

    fn json_schema(_gen: &mut schemars::r#gen::SchemaGenerator) -> schemars::schema::Schema {
        use schemars::schema::{InstanceType, SchemaObject, StringValidation};

        SchemaObject {
            instance_type: Some(InstanceType::String.into()),
            string: Some(Box::new(StringValidation {
                pattern: Some(r"^[TSCAtsca]$|^Y[CXKT]$|^y[cxkt]$".to_owned()),
                ..Default::default()
            })),
            enum_values: Some(vec![
                "T".into(),
                "S".into(),
                "C".into(),
                "A".into(),
                "YC".into(),
                "YX".into(),
                "YK".into(),
                "YT".into(),
            ]),
            ..Default::default()
        }
        .into()
    }
}

impl PointType {
    // ========================================================================
    // Internal ID Encoding/Decoding for point_id collision avoidance
    // ========================================================================

    /// Offset between point type ranges (~1 billion points per type).
    ///
    /// This ensures different point types can use the same original point_id
    /// without colliding in the internal representation.
    ///
    /// # Layout
    /// - Telemetry: 0x00000000 - 0x3FFFFFFF
    /// - Signal:    0x40000000 - 0x7FFFFFFF
    /// - Control:   0x80000000 - 0xBFFFFFFF
    /// - Adjustment: 0xC0000000 - 0xFFFFFFFF
    pub const OFFSET: u32 = u32::MAX / 4; // 0x3FFFFFFF ≈ 1.07 billion

    /// Convert to string representation.
    #[inline]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Telemetry => "T",
            Self::Signal => "S",
            Self::Control => "C",
            Self::Adjustment => "A",
        }
    }

    /// Convert to single-character representation.
    #[inline]
    pub const fn as_char(&self) -> char {
        match self {
            Self::Telemetry => 'T',
            Self::Signal => 'S',
            Self::Control => 'C',
            Self::Adjustment => 'A',
        }
    }

    /// Convert to u8 representation.
    #[inline]
    pub const fn to_u8(&self) -> u8 {
        *self as u8
    }

    /// Convert from u8 representation.
    #[inline]
    pub const fn from_u8(value: u8) -> Option<Self> {
        match value {
            0 => Some(Self::Telemetry),
            1 => Some(Self::Signal),
            2 => Some(Self::Control),
            3 => Some(Self::Adjustment),
            _ => None,
        }
    }

    /// Parse from string (returns Option for convenience).
    ///
    /// Supports: T/S/C/A, YC/YX/YK/YT (case-insensitive)
    #[inline]
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "T" | "t" | "YC" | "yc" | "Yc" | "yC" => Some(Self::Telemetry),
            "S" | "s" | "YX" | "yx" | "Yx" | "yX" => Some(Self::Signal),
            "C" | "c" | "YK" | "yk" | "Yk" | "yK" => Some(Self::Control),
            "A" | "a" | "YT" | "yt" | "Yt" | "yT" => Some(Self::Adjustment),
            _ => None,
        }
    }

    /// Check if this is a measurement type (Telemetry or Signal).
    #[inline]
    pub const fn is_measurement(&self) -> bool {
        matches!(self, Self::Telemetry | Self::Signal)
    }

    /// Check if this is an action type (Control or Adjustment).
    #[inline]
    pub const fn is_action(&self) -> bool {
        matches!(self, Self::Control | Self::Adjustment)
    }

    /// Check if this is an analog type (Telemetry or Adjustment).
    #[inline]
    pub const fn is_analog(&self) -> bool {
        matches!(self, Self::Telemetry | Self::Adjustment)
    }

    /// Check if this is a digital type (Signal or Control).
    #[inline]
    pub const fn is_digital(&self) -> bool {
        matches!(self, Self::Signal | Self::Control)
    }

    /// Check if this is an input type (T or S) - alias for is_measurement.
    #[inline]
    pub const fn is_input(&self) -> bool {
        self.is_measurement()
    }

    /// Check if this is an output type (C or A) - alias for is_action.
    #[inline]
    pub const fn is_output(&self) -> bool {
        self.is_action()
    }

    /// Get the type offset for this point type.
    #[inline]
    pub const fn type_offset(&self) -> u32 {
        match self {
            Self::Telemetry => 0,
            Self::Signal => Self::OFFSET,
            Self::Control => Self::OFFSET * 2,
            Self::Adjustment => Self::OFFSET * 3,
        }
    }

    /// Convert an original point_id to an internal_id that encodes the type.
    ///
    /// Used when building protocol configurations to avoid point_id collisions
    /// between different point types.
    #[inline]
    pub const fn to_internal_id(&self, point_id: u32) -> u32 {
        point_id + self.type_offset()
    }

    /// Decode an internal_id back to (PointType, original_point_id).
    ///
    /// Used when writing data to Redis or other storage.
    #[inline]
    pub const fn from_internal_id(internal_id: u32) -> (Self, u32) {
        let type_index = internal_id / Self::OFFSET;
        let original_id = internal_id % Self::OFFSET;
        let point_type = match type_index {
            0 => Self::Telemetry,
            1 => Self::Signal,
            2 => Self::Control,
            _ => Self::Adjustment, // 3 or overflow wraps to Adjustment
        };
        (point_type, original_id)
    }
}

impl fmt::Display for PointType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Error type for PointType parsing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParsePointTypeError;

impl fmt::Display for ParsePointTypeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("invalid point type, expected: T/S/C/A or YC/YX/YK/YT")
    }
}

#[cfg(feature = "std")]
impl std::error::Error for ParsePointTypeError {}

impl core::str::FromStr for PointType {
    type Err = ParsePointTypeError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::from_str(s).ok_or(ParsePointTypeError)
    }
}

/// Data quality flags for point values.
///
/// Uses bit flags for efficient storage and combination.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(u8)]
pub enum Quality {
    /// Good quality, value is valid
    #[default]
    Good = 0,
    /// Value is stale (not updated recently)
    Stale = 1,
    /// Communication failure
    CommFail = 2,
    /// Device reports error
    DeviceError = 3,
    /// Value out of range
    OutOfRange = 4,
    /// Invalid or unknown quality
    Invalid = 0xFF,
}

impl Quality {
    /// Check if quality indicates a valid value.
    #[inline]
    pub const fn is_good(&self) -> bool {
        matches!(self, Self::Good)
    }

    /// Convert from u8.
    #[inline]
    pub const fn from_u8(value: u8) -> Self {
        match value {
            0 => Self::Good,
            1 => Self::Stale,
            2 => Self::CommFail,
            3 => Self::DeviceError,
            4 => Self::OutOfRange,
            _ => Self::Invalid,
        }
    }
}

/// Point value representation.
///
/// Optimized for stack allocation with no heap usage.
/// For string values, use a fixed-size buffer (max 16 bytes).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Value {
    /// 64-bit floating point
    Float(f64),
    /// 64-bit signed integer
    Integer(i64),
    /// Boolean value
    Bool(bool),
    /// Fixed-size string buffer (for ASCII data like CAN strings)
    /// Format: (data, length)
    String16([u8; 16], u8),
}

impl Value {
    /// Create a new float value.
    #[inline]
    pub const fn float(v: f64) -> Self {
        Self::Float(v)
    }

    /// Create a new integer value.
    #[inline]
    pub const fn integer(v: i64) -> Self {
        Self::Integer(v)
    }

    /// Create a new boolean value.
    #[inline]
    pub const fn bool(v: bool) -> Self {
        Self::Bool(v)
    }

    /// Create a new string value from bytes.
    ///
    /// Copies up to 16 bytes from the input slice.
    pub fn from_bytes(bytes: &[u8]) -> Self {
        let mut buf = [0u8; 16];
        let len = bytes.len().min(16);
        buf[..len].copy_from_slice(&bytes[..len]);
        Self::String16(buf, len as u8)
    }

    /// Try to get as f64.
    #[inline]
    pub const fn as_f64(&self) -> Option<f64> {
        match self {
            Self::Float(v) => Some(*v),
            Self::Integer(v) => Some(*v as f64),
            Self::Bool(v) => Some(if *v { 1.0 } else { 0.0 }),
            Self::String16(_, _) => None,
        }
    }

    /// Try to get as i64.
    #[inline]
    pub const fn as_i64(&self) -> Option<i64> {
        match self {
            Self::Float(v) => Some(*v as i64),
            Self::Integer(v) => Some(*v),
            Self::Bool(v) => Some(if *v { 1 } else { 0 }),
            Self::String16(_, _) => None,
        }
    }

    /// Try to get as bool.
    #[inline]
    pub const fn as_bool(&self) -> Option<bool> {
        match self {
            Self::Float(v) => Some(*v != 0.0),
            Self::Integer(v) => Some(*v != 0),
            Self::Bool(v) => Some(*v),
            Self::String16(_, _) => None,
        }
    }

    /// Get string bytes if this is a string value.
    #[inline]
    pub const fn as_str_bytes(&self) -> Option<(&[u8; 16], u8)> {
        match self {
            Self::String16(buf, len) => Some((buf, *len)),
            _ => None,
        }
    }
}

impl Default for Value {
    fn default() -> Self {
        Self::Float(0.0)
    }
}

impl From<f64> for Value {
    fn from(v: f64) -> Self {
        Self::Float(v)
    }
}

impl From<f32> for Value {
    fn from(v: f32) -> Self {
        Self::Float(v as f64)
    }
}

impl From<i64> for Value {
    fn from(v: i64) -> Self {
        Self::Integer(v)
    }
}

impl From<i32> for Value {
    fn from(v: i32) -> Self {
        Self::Integer(v as i64)
    }
}

impl From<u32> for Value {
    fn from(v: u32) -> Self {
        Self::Integer(v as i64)
    }
}

impl From<bool> for Value {
    fn from(v: bool) -> Self {
        Self::Bool(v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_point_type_as_str() {
        assert_eq!(PointType::Telemetry.as_str(), "T");
        assert_eq!(PointType::Signal.as_str(), "S");
        assert_eq!(PointType::Control.as_str(), "C");
        assert_eq!(PointType::Adjustment.as_str(), "A");
    }

    #[test]
    fn test_point_type_as_char() {
        assert_eq!(PointType::Telemetry.as_char(), 'T');
        assert_eq!(PointType::Signal.as_char(), 'S');
        assert_eq!(PointType::Control.as_char(), 'C');
        assert_eq!(PointType::Adjustment.as_char(), 'A');
    }

    #[test]
    fn test_point_type_to_u8() {
        assert_eq!(PointType::Telemetry.to_u8(), 0);
        assert_eq!(PointType::Signal.to_u8(), 1);
        assert_eq!(PointType::Control.to_u8(), 2);
        assert_eq!(PointType::Adjustment.to_u8(), 3);
    }

    #[test]
    fn test_point_type_from_u8() {
        assert_eq!(PointType::from_u8(0), Some(PointType::Telemetry));
        assert_eq!(PointType::from_u8(1), Some(PointType::Signal));
        assert_eq!(PointType::from_u8(2), Some(PointType::Control));
        assert_eq!(PointType::from_u8(3), Some(PointType::Adjustment));
        assert_eq!(PointType::from_u8(4), None);
    }

    #[test]
    fn test_point_type_from_str_method() {
        // Standard codes
        assert_eq!(PointType::from_str("T"), Some(PointType::Telemetry));
        assert_eq!(PointType::from_str("S"), Some(PointType::Signal));
        assert_eq!(PointType::from_str("C"), Some(PointType::Control));
        assert_eq!(PointType::from_str("A"), Some(PointType::Adjustment));
        // IEC synonyms
        assert_eq!(PointType::from_str("YC"), Some(PointType::Telemetry));
        assert_eq!(PointType::from_str("YX"), Some(PointType::Signal));
        assert_eq!(PointType::from_str("YK"), Some(PointType::Control));
        assert_eq!(PointType::from_str("YT"), Some(PointType::Adjustment));
        // Case insensitive
        assert_eq!(PointType::from_str("yc"), Some(PointType::Telemetry));
        assert_eq!(PointType::from_str("t"), Some(PointType::Telemetry));
        // Invalid
        assert_eq!(PointType::from_str("invalid"), None);
    }

    #[test]
    fn test_point_type_parse_trait() {
        assert_eq!("T".parse::<PointType>(), Ok(PointType::Telemetry));
        assert_eq!("YC".parse::<PointType>(), Ok(PointType::Telemetry));
        assert!("invalid".parse::<PointType>().is_err());
    }

    #[test]
    fn test_point_type_categories() {
        assert!(PointType::Telemetry.is_measurement());
        assert!(PointType::Signal.is_measurement());
        assert!(!PointType::Control.is_measurement());
        assert!(!PointType::Adjustment.is_measurement());

        assert!(!PointType::Telemetry.is_action());
        assert!(PointType::Control.is_action());
        assert!(PointType::Adjustment.is_action());

        assert!(PointType::Telemetry.is_analog());
        assert!(!PointType::Signal.is_analog());
        assert!(PointType::Adjustment.is_analog());

        assert!(PointType::Signal.is_digital());
        assert!(PointType::Control.is_digital());
        assert!(!PointType::Telemetry.is_digital());
    }

    #[test]
    fn test_point_type_input_output() {
        assert!(PointType::Telemetry.is_input());
        assert!(PointType::Signal.is_input());
        assert!(!PointType::Control.is_input());

        assert!(!PointType::Telemetry.is_output());
        assert!(PointType::Control.is_output());
        assert!(PointType::Adjustment.is_output());
    }

    #[test]
    fn test_internal_id_roundtrip() {
        for (pt, original_id) in [
            (PointType::Telemetry, 1),
            (PointType::Telemetry, 100),
            (PointType::Signal, 1),
            (PointType::Signal, 8),
            (PointType::Control, 1),
            (PointType::Control, 8),
            (PointType::Adjustment, 1),
            (PointType::Adjustment, 1000),
        ] {
            let internal = pt.to_internal_id(original_id);
            let (recovered_type, recovered_id) = PointType::from_internal_id(internal);
            assert_eq!(recovered_type, pt);
            assert_eq!(recovered_id, original_id);
        }
    }

    #[test]
    fn test_internal_id_no_collision() {
        let signal_internal = PointType::Signal.to_internal_id(1);
        let control_internal = PointType::Control.to_internal_id(1);
        assert_ne!(signal_internal, control_internal);

        let (s_type, s_id) = PointType::from_internal_id(signal_internal);
        let (c_type, c_id) = PointType::from_internal_id(control_internal);
        assert_eq!(s_type, PointType::Signal);
        assert_eq!(c_type, PointType::Control);
        assert_eq!(s_id, 1);
        assert_eq!(c_id, 1);
    }

    #[test]
    fn test_type_offset_values() {
        assert_eq!(PointType::Telemetry.type_offset(), 0);
        assert_eq!(PointType::Signal.type_offset(), PointType::OFFSET);
        assert_eq!(PointType::Control.type_offset(), PointType::OFFSET * 2);
        assert_eq!(PointType::Adjustment.type_offset(), PointType::OFFSET * 3);
    }

    #[test]
    fn test_point_type_display() {
        assert_eq!(format!("{}", PointType::Telemetry), "T");
        assert_eq!(format!("{}", PointType::Signal), "S");
    }

    #[test]
    fn test_quality() {
        assert!(Quality::Good.is_good());
        assert!(!Quality::Stale.is_good());
        assert!(!Quality::CommFail.is_good());

        assert_eq!(Quality::from_u8(0), Quality::Good);
        assert_eq!(Quality::from_u8(1), Quality::Stale);
        assert_eq!(Quality::from_u8(255), Quality::Invalid);
    }

    #[test]
    #[allow(clippy::approx_constant, clippy::disallowed_methods)]
    fn test_value_conversions() {
        let v = Value::Float(3.14);
        assert_eq!(v.as_f64(), Some(3.14));
        assert_eq!(v.as_i64(), Some(3));
        assert_eq!(v.as_bool(), Some(true));

        let v = Value::Integer(42);
        assert_eq!(v.as_f64(), Some(42.0));
        assert_eq!(v.as_i64(), Some(42));

        let v = Value::Bool(false);
        assert_eq!(v.as_f64(), Some(0.0));
        assert_eq!(v.as_i64(), Some(0));
        assert_eq!(v.as_bool(), Some(false));

        let v = Value::from_bytes(b"HELLO");
        assert!(v.as_str_bytes().is_some());
        let (buf, len) = v.as_str_bytes().unwrap();
        assert_eq!(len, 5);
        assert_eq!(&buf[..5], b"HELLO");
    }
}
