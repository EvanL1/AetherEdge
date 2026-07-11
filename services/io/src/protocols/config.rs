//! Configuration utilities for protocol channel construction.
//!
//! This module provides generic utilities for building protocol channels,
//! without any application-layer concepts like "Four Remotes" (telemetry, signaling, control, adjustment).
//!
//! # Design Principle
//!
//! The protocol layer is a pure protocol library. It knows about:
//! - Protocol addresses (Modbus registers, GPIO pins, CAN IDs)
//! - Protocol-specific configuration parsing
//! - Point configuration with transforms
//!
//! It does NOT know about:
//! - SCADA concepts (Telemetry, Signal, Control, Adjustment)
//! - Application-layer point categorization
//!
//! The application layer (e.g., io) is responsible for:
//! - Iterating over its own point categories
//! - Calling the protocol parsers
//! - Building PointConfig lists

use crate::protocols::core::point::PointConfig;

/// Result of building a protocol channel.
///
/// Contains both the channel instance and the parsed point configurations,
/// which the application layer may need for routing/storage.
#[derive(Debug)]
pub struct ChannelBuildResult<C> {
    /// The constructed protocol channel.
    pub channel: C,

    /// Parsed point configurations with protocol-specific addresses.
    ///
    /// The application layer can use these for routing or storage mapping.
    pub points: Vec<PointConfig>,

    /// Points that failed to parse (point_id, error message).
    ///
    /// These are logged as warnings during construction but don't
    /// prevent the channel from being created.
    pub failed_points: Vec<(u32, String)>,
}

impl<C> ChannelBuildResult<C> {
    /// Create a new build result.
    pub fn new(channel: C, points: Vec<PointConfig>) -> Self {
        Self {
            channel,
            points,
            failed_points: Vec::new(),
        }
    }

    /// Create a build result with some failed points.
    pub fn with_failures(
        channel: C,
        points: Vec<PointConfig>,
        failures: Vec<(u32, String)>,
    ) -> Self {
        Self {
            channel,
            points,
            failed_points: failures,
        }
    }

    /// Check if any points failed to parse.
    pub fn has_failures(&self) -> bool {
        !self.failed_points.is_empty()
    }

    /// Get the number of successfully parsed points.
    pub fn success_count(&self) -> usize {
        self.points.len()
    }

    /// Get the number of failed points.
    pub fn failure_count(&self) -> usize {
        self.failed_points.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_channel_build_result() {
        let result: ChannelBuildResult<()> = ChannelBuildResult::new((), vec![]);
        assert!(!result.has_failures());
        assert_eq!(result.success_count(), 0);
        assert_eq!(result.failure_count(), 0);

        let result: ChannelBuildResult<()> =
            ChannelBuildResult::with_failures((), vec![], vec![(1, "test error".to_string())]);
        assert!(result.has_failures());
        assert_eq!(result.failure_count(), 1);
    }
}
