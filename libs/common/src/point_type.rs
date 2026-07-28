//! Storage and protocol representation of the four physical point planes.
//!
//! Application semantics use `aether_domain::PointKind`. This adapter type
//! preserves the established T/S/C/A SQLite, HTTP, and protocol wire format.

use std::fmt;

use serde::{Deserialize, Serialize};

#[cfg(feature = "schema")]
use schemars::JsonSchema;

/// Physical point plane encoded by existing configuration and protocol DTOs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(JsonSchema))]
#[repr(u8)]
pub enum PointType {
    #[serde(rename = "T", alias = "YC", alias = "yc", alias = "telemetry")]
    Telemetry = 0,
    #[serde(rename = "S", alias = "YX", alias = "yx", alias = "signal")]
    Signal = 1,
    #[serde(rename = "C", alias = "YK", alias = "yk", alias = "control")]
    Control = 2,
    #[serde(
        rename = "A",
        alias = "YT",
        alias = "yt",
        alias = "adjustment",
        alias = "setpoint"
    )]
    Adjustment = 3,
}

impl PointType {
    /// Offset between the four legacy internal identifier ranges.
    pub const OFFSET: u32 = u32::MAX / 4;

    #[inline]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Telemetry => "T",
            Self::Signal => "S",
            Self::Control => "C",
            Self::Adjustment => "A",
        }
    }

    /// Parse the accepted storage and historical IEC aliases.
    #[inline]
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(value: &str) -> Option<Self> {
        match value {
            "T" | "t" | "YC" | "yc" | "Yc" | "yC" => Some(Self::Telemetry),
            "S" | "s" | "YX" | "yx" | "Yx" | "yX" => Some(Self::Signal),
            "C" | "c" | "YK" | "yk" | "Yk" | "yK" => Some(Self::Control),
            "A" | "a" | "YT" | "yt" | "Yt" | "yT" => Some(Self::Adjustment),
            _ => None,
        }
    }

    #[inline]
    pub const fn is_measurement(self) -> bool {
        matches!(self, Self::Telemetry | Self::Signal)
    }

    #[inline]
    pub const fn is_input(self) -> bool {
        self.is_measurement()
    }

    #[inline]
    pub const fn is_output(self) -> bool {
        matches!(self, Self::Control | Self::Adjustment)
    }

    #[inline]
    const fn type_offset(self) -> u32 {
        match self {
            Self::Telemetry => 0,
            Self::Signal => Self::OFFSET,
            Self::Control => Self::OFFSET * 2,
            Self::Adjustment => Self::OFFSET * 3,
        }
    }

    #[inline]
    pub const fn to_internal_id(self, point_id: u32) -> u32 {
        point_id + self.type_offset()
    }

    #[inline]
    pub const fn from_internal_id(internal_id: u32) -> (Self, u32) {
        let type_index = internal_id / Self::OFFSET;
        let original_id = internal_id % Self::OFFSET;
        let point_type = match type_index {
            0 => Self::Telemetry,
            1 => Self::Signal,
            2 => Self::Control,
            _ => Self::Adjustment,
        };
        (point_type, original_id)
    }
}

impl fmt::Display for PointType {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParsePointTypeError;

impl fmt::Display for ParsePointTypeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("invalid point type, expected T/S/C/A or YC/YX/YK/YT")
    }
}

impl std::error::Error for ParsePointTypeError {}

impl std::str::FromStr for PointType {
    type Err = ParsePointTypeError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::from_str(value).ok_or(ParsePointTypeError)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn storage_codes_and_aliases_remain_compatible() {
        for (value, expected) in [
            ("T", PointType::Telemetry),
            ("YX", PointType::Signal),
            ("yk", PointType::Control),
            ("YT", PointType::Adjustment),
        ] {
            assert_eq!(PointType::from_str(value), Some(expected));
        }
        let serialized = serde_json::to_string(&PointType::Signal);
        assert!(matches!(serialized.as_deref(), Ok("\"S\"")));
    }

    #[test]
    fn internal_identifier_round_trips_all_planes() {
        for point_type in [
            PointType::Telemetry,
            PointType::Signal,
            PointType::Control,
            PointType::Adjustment,
        ] {
            let encoded = point_type.to_internal_id(42);
            assert_eq!(PointType::from_internal_id(encoded), (point_type, 42));
        }
    }
}
