//! Protocol frame definitions.
//!
//! This module defines common frame structures used across protocols.

use crate::types::{PointType, Quality, Value};

/// Maximum points per batch.
pub const MAX_BATCH_POINTS: usize = 64;

/// A single point reading.
#[derive(Debug, Clone, Copy)]
pub struct PointReading {
    /// Point identifier within the instance.
    pub point_id: u32,
    /// Point type (T/S/C/A).
    pub point_type: PointType,
    /// Point value.
    pub value: Value,
    /// Data quality.
    pub quality: Quality,
    /// Timestamp in Unix milliseconds.
    pub timestamp: u64,
}

impl PointReading {
    /// Create a new telemetry reading.
    #[inline]
    pub const fn telemetry(point_id: u32, value: f64, timestamp: u64) -> Self {
        Self {
            point_id,
            point_type: PointType::Telemetry,
            value: Value::Float(value),
            quality: Quality::Good,
            timestamp,
        }
    }

    /// Create a new signal reading.
    #[inline]
    pub const fn signal(point_id: u32, value: bool, timestamp: u64) -> Self {
        Self {
            point_id,
            point_type: PointType::Signal,
            value: Value::Bool(value),
            quality: Quality::Good,
            timestamp,
        }
    }

    /// Create a reading with stale quality.
    #[inline]
    pub const fn with_quality(mut self, quality: Quality) -> Self {
        self.quality = quality;
        self
    }
}

/// Batch of point readings.
///
/// Stack-allocated, no heap usage.
#[derive(Debug)]
pub struct PointBatch {
    /// Instance/channel identifier.
    pub instance_id: u32,
    /// Point readings.
    readings: [PointReading; MAX_BATCH_POINTS],
    /// Number of valid readings.
    count: usize,
}

impl PointBatch {
    /// Create a new empty batch.
    pub const fn new(instance_id: u32) -> Self {
        Self {
            instance_id,
            readings: [PointReading::telemetry(0, 0.0, 0); MAX_BATCH_POINTS],
            count: 0,
        }
    }

    /// Add a reading to the batch.
    ///
    /// Returns false if batch is full.
    #[inline]
    pub fn push(&mut self, reading: PointReading) -> bool {
        if self.count >= MAX_BATCH_POINTS {
            return false;
        }
        self.readings[self.count] = reading;
        self.count += 1;
        true
    }

    /// Get the readings slice.
    #[inline]
    pub fn readings(&self) -> &[PointReading] {
        &self.readings[..self.count]
    }

    /// Get the number of readings.
    #[inline]
    pub const fn len(&self) -> usize {
        self.count
    }

    /// Check if empty.
    #[inline]
    pub const fn is_empty(&self) -> bool {
        self.count == 0
    }

    /// Clear all readings.
    #[inline]
    pub fn clear(&mut self) {
        self.count = 0;
    }

    /// Iterate over readings.
    #[inline]
    pub fn iter(&self) -> impl Iterator<Item = &PointReading> {
        self.readings[..self.count].iter()
    }
}

impl Default for PointBatch {
    fn default() -> Self {
        Self::new(0)
    }
}

/// Control command to send to a device.
#[derive(Debug, Clone, Copy)]
pub struct ControlCommand {
    /// Target instance/channel.
    pub instance_id: u32,
    /// Target point.
    pub point_id: u32,
    /// Command value.
    pub value: Value,
    /// Command sequence number (for acknowledgment).
    pub sequence: u32,
    /// Timeout in milliseconds.
    pub timeout_ms: u32,
}

impl ControlCommand {
    /// Create a new digital control command (on/off).
    pub const fn digital(instance_id: u32, point_id: u32, on: bool, sequence: u32) -> Self {
        Self {
            instance_id,
            point_id,
            value: Value::Bool(on),
            sequence,
            timeout_ms: 5000,
        }
    }

    /// Create a new analog adjustment command.
    pub const fn analog(instance_id: u32, point_id: u32, value: f64, sequence: u32) -> Self {
        Self {
            instance_id,
            point_id,
            value: Value::Float(value),
            sequence,
            timeout_ms: 5000,
        }
    }

    /// Set timeout.
    pub const fn with_timeout(mut self, timeout_ms: u32) -> Self {
        self.timeout_ms = timeout_ms;
        self
    }
}

/// Command acknowledgment status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum CommandStatus {
    /// Command pending execution.
    Pending = 0,
    /// Command executed successfully.
    Success = 1,
    /// Command failed.
    Failed = 2,
    /// Command timed out.
    Timeout = 3,
    /// Device rejected command.
    Rejected = 4,
}

/// Command acknowledgment.
#[derive(Debug, Clone, Copy)]
pub struct CommandAck {
    /// Command sequence number.
    pub sequence: u32,
    /// Execution status.
    pub status: CommandStatus,
    /// Optional error code.
    pub error_code: u16,
    /// Timestamp of acknowledgment.
    pub timestamp: u64,
}

impl CommandAck {
    /// Create a success acknowledgment.
    pub const fn success(sequence: u32, timestamp: u64) -> Self {
        Self {
            sequence,
            status: CommandStatus::Success,
            error_code: 0,
            timestamp,
        }
    }

    /// Create a failure acknowledgment.
    pub const fn failed(sequence: u32, error_code: u16, timestamp: u64) -> Self {
        Self {
            sequence,
            status: CommandStatus::Failed,
            error_code,
            timestamp,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_point_reading() {
        let reading = PointReading::telemetry(1, 42.5, 1234567890);
        assert_eq!(reading.point_id, 1);
        assert_eq!(reading.point_type, PointType::Telemetry);
        assert_eq!(reading.value.as_f64(), Some(42.5));
        assert!(reading.quality.is_good());
    }

    #[test]
    fn test_point_batch() {
        let mut batch = PointBatch::new(100);
        assert!(batch.is_empty());

        batch.push(PointReading::telemetry(0, 1.0, 100));
        batch.push(PointReading::telemetry(1, 2.0, 100));
        batch.push(PointReading::signal(2, true, 100));

        assert_eq!(batch.len(), 3);
        assert_eq!(batch.instance_id, 100);

        let readings: Vec<_> = batch.iter().collect();
        assert_eq!(readings.len(), 3);
    }

    #[test]
    fn test_control_command() {
        let cmd = ControlCommand::digital(1, 10, true, 42);
        assert_eq!(cmd.instance_id, 1);
        assert_eq!(cmd.point_id, 10);
        assert_eq!(cmd.value.as_bool(), Some(true));
        assert_eq!(cmd.sequence, 42);
    }

    #[test]
    fn test_command_ack() {
        let ack = CommandAck::success(42, 1234567890);
        assert_eq!(ack.sequence, 42);
        assert_eq!(ack.status, CommandStatus::Success);
        assert_eq!(ack.error_code, 0);
    }
}
