//! Data quality codes for industrial protocols.
//!
//! Quality codes indicate the reliability and validity of data points.
//! This implementation is compatible with OPC UA quality codes.

use serde::{Deserialize, Serialize};

/// Data quality indicator.
///
/// Represents the quality/reliability of a data point value.
/// Based on OPC UA quality codes for maximum compatibility.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[repr(u8)]
pub enum Quality {
    /// Value is good and reliable
    #[default]
    Good = 0,

    /// Value is bad/unreliable
    Bad = 1,

    /// Value quality is uncertain
    Uncertain = 2,

    /// Value is invalid (not applicable)
    Invalid = 3,

    /// Communication with device lost
    NotConnected = 4,

    /// Device failure detected
    DeviceFailure = 5,

    /// Sensor failure detected
    SensorFailure = 6,

    /// Communication failure
    CommFailure = 7,

    /// Point is out of service
    OutOfService = 8,

    /// Value has been manually substituted
    Substituted = 9,

    /// Value overflow (out of range)
    Overflow = 10,

    /// Value underflow (below range)
    Underflow = 11,

    /// Configuration error
    ConfigError = 12,

    /// Last known value (connection lost but value cached)
    LastKnown = 13,
}

impl Quality {
    /// Check if the quality is good.
    #[inline]
    pub fn is_good(&self) -> bool {
        matches!(self, Self::Good)
    }

    /// Check if the quality is bad (any non-good status).
    #[inline]
    pub fn is_bad(&self) -> bool {
        !self.is_good()
    }

    /// Get a short description of this quality.
    pub fn description(&self) -> &'static str {
        match self {
            Self::Good => "Good",
            Self::Bad => "Bad",
            Self::Uncertain => "Uncertain",
            Self::Invalid => "Invalid",
            Self::NotConnected => "Not Connected",
            Self::DeviceFailure => "Device Failure",
            Self::SensorFailure => "Sensor Failure",
            Self::CommFailure => "Communication Failure",
            Self::OutOfService => "Out of Service",
            Self::Substituted => "Substituted",
            Self::Overflow => "Overflow",
            Self::Underflow => "Underflow",
            Self::ConfigError => "Configuration Error",
            Self::LastKnown => "Last Known Value",
        }
    }
}

impl std::fmt::Display for Quality {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.description())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_quality_default() {
        assert_eq!(Quality::default(), Quality::Good);
    }

    #[test]
    fn test_quality_checks() {
        assert!(Quality::Good.is_good());
        assert!(!Quality::Bad.is_good());
        assert!(Quality::Bad.is_bad());
    }
}
