//! Core domain types for AetherEMS
//!
//! This module contains fundamental types used across the system.
//!
//! ## Re-exports from aether-core
//!
//! `PointType` is re-exported from `aether-core` to maintain a single source of truth
//! for this fundamental type used by both firmware and gateway layers.

use serde::{Deserialize, Serialize};
use std::fmt;

// ============================================================================
// Re-export PointType from aether-core
// ============================================================================

// Re-export the core PointType - this is the single source of truth
// JsonSchema is implemented in aether-core when schema feature is enabled
pub use aether_core::PointType;

#[cfg(test)]
#[allow(clippy::disallowed_methods)] // Test code - unwrap is acceptable
mod point_type_tests {
    use super::*;

    #[test]
    fn test_point_type_as_str() {
        assert_eq!(PointType::Telemetry.as_str(), "T");
        assert_eq!(PointType::Signal.as_str(), "S");
        assert_eq!(PointType::Control.as_str(), "C");
        assert_eq!(PointType::Adjustment.as_str(), "A");
    }

    #[test]
    fn test_point_type_from_str() {
        assert_eq!(PointType::from_str("T"), Some(PointType::Telemetry));
        assert_eq!(PointType::from_str("YC"), Some(PointType::Telemetry));
        assert_eq!(PointType::from_str("yc"), Some(PointType::Telemetry));
        assert_eq!(PointType::from_str("invalid"), None);
    }

    #[test]
    fn test_point_type_categories() {
        assert!(PointType::Telemetry.is_measurement());
        assert!(PointType::Signal.is_measurement());
        assert!(!PointType::Control.is_measurement());
        assert!(PointType::Control.is_action());
        assert!(PointType::Adjustment.is_action());
    }

    #[test]
    fn test_point_type_serde() {
        assert_eq!(
            serde_json::to_string(&PointType::Telemetry).unwrap(),
            "\"T\""
        );
        assert_eq!(
            serde_json::from_str::<PointType>("\"YC\"").unwrap(),
            PointType::Telemetry
        );
        assert_eq!(
            serde_json::from_str::<PointType>("\"telemetry\"").unwrap(),
            PointType::Telemetry
        );
    }

    #[test]
    fn test_internal_id_roundtrip() {
        let internal = PointType::Signal.to_internal_id(5);
        let (pt, id) = PointType::from_internal_id(internal);
        assert_eq!(pt, PointType::Signal);
        assert_eq!(id, 5);
    }
}

// ============================================================================
// Point Role Types (Data Flow Direction)
// ============================================================================

/// Point role types indicating data flow direction
///
/// Unlike `PointType` which describes the type of data (T/S/C/A),
/// `PointRole` describes the direction of data flow:
/// - M (Measurement): Data flows from device → model (uplink)
/// - A (Action): Data flows from model → device (downlink)
///
/// # Usage
/// ```
/// # use aether_model::PointRole;
/// let role = PointRole::Measurement;
/// assert_eq!(role.as_str(), "M");
/// ```
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[repr(u8)]
pub enum PointRole {
    /// Measurement point (M) - data flows from device to model
    #[serde(rename = "M")]
    #[default]
    Measurement = 0,
    /// Action point (A) - data flows from model to device
    #[serde(rename = "A")]
    Action = 1,
}

impl PointRole {
    /// Convert to string representation
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Measurement => "M",
            Self::Action => "A",
        }
    }
}

impl std::str::FromStr for PointRole {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_uppercase().as_str() {
            "M" | "MEASUREMENT" => Ok(Self::Measurement),
            "A" | "ACTION" => Ok(Self::Action),
            _ => Err(format!("Unknown point role: {}", s)),
        }
    }
}

impl fmt::Display for PointRole {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

#[cfg(test)]
#[allow(clippy::disallowed_methods)] // Test code - unwrap is acceptable
mod point_role_tests {
    use super::*;

    #[test]
    fn test_point_role_as_str() {
        assert_eq!(PointRole::Measurement.as_str(), "M");
        assert_eq!(PointRole::Action.as_str(), "A");
    }

    #[test]
    fn test_point_role_from_str() {
        assert_eq!("M".parse::<PointRole>().unwrap(), PointRole::Measurement);
        assert_eq!("A".parse::<PointRole>().unwrap(), PointRole::Action);
        assert_eq!(
            "measurement".parse::<PointRole>().unwrap(),
            PointRole::Measurement
        );
        assert!("X".parse::<PointRole>().is_err());
    }

    #[test]
    fn test_point_role_display() {
        assert_eq!(format!("{}", PointRole::Measurement), "M");
        assert_eq!(format!("{}", PointRole::Action), "A");
    }

    #[test]
    fn test_point_role_default() {
        assert_eq!(PointRole::default(), PointRole::Measurement);
    }
}
