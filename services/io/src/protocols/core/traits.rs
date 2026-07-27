//! Core traits for protocol implementations.
//!
//! This module defines the fundamental traits that all protocols must implement.
//!
//! # Trait Hierarchy
//!
//! ```text
//! Runtime operations are exposed through the object-safe `ChannelRuntime`
//! boundary in `protocols::gateway`.
//! ```

use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::sync::Arc;
use tokio::sync::broadcast;

use crate::protocols::core::data::DataBatch;

/// Connection state of a protocol client.
///
/// Uses `#[repr(u8)]` to enable lock-free atomic storage via `AtomicU8`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[repr(u8)]
pub enum ConnectionState {
    /// Not connected to the target.
    #[default]
    Disconnected = 0,

    /// Attempting to connect.
    Connecting = 1,

    /// Connected and operational.
    Connected = 2,

    /// Attempting to reconnect after failure.
    Reconnecting = 3,

    /// Connection error state.
    Error = 4,
}

impl From<u8> for ConnectionState {
    fn from(value: u8) -> Self {
        match value {
            0 => Self::Disconnected,
            1 => Self::Connecting,
            2 => Self::Connected,
            3 => Self::Reconnecting,
            4 => Self::Error,
            _ => Self::Error, // Fallback for invalid values
        }
    }
}

impl From<ConnectionState> for u8 {
    fn from(state: ConnectionState) -> Self {
        state as u8
    }
}

impl ConnectionState {
    /// Check if currently connected.
    #[inline]
    pub const fn is_connected(&self) -> bool {
        matches!(self, Self::Connected)
    }
}

impl std::fmt::Display for ConnectionState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Self::Disconnected => "Disconnected",
            Self::Connecting => "Connecting",
            Self::Connected => "Connected",
            Self::Reconnecting => "Reconnecting",
            Self::Error => "Error",
        };
        write!(f, "{}", s)
    }
}

/// A control command to write.
#[derive(Debug, Clone)]
pub struct ControlCommand {
    /// Point ID
    pub id: u32,

    /// Command value (true = ON/CLOSE, false = OFF/OPEN)
    pub value: bool,

    /// Pulse duration in milliseconds (None = latching)
    pub pulse_duration_ms: Option<u32>,
}

impl ControlCommand {
    /// Create a latching control command.
    pub fn latching(id: u32, value: bool) -> Self {
        Self {
            id,
            value,
            pulse_duration_ms: None,
        }
    }

    /// Create a pulse control command.
    pub fn pulse(id: u32, value: bool, duration_ms: u32) -> Self {
        Self {
            id,
            value,
            pulse_duration_ms: Some(duration_ms),
        }
    }
}

/// An adjustment command to write.
#[derive(Debug, Clone)]
pub struct AdjustmentCommand {
    /// Point ID
    pub id: u32,

    /// Setpoint value
    pub value: f64,
}

impl AdjustmentCommand {
    /// Create an adjustment command.
    pub fn new(id: u32, value: f64) -> Self {
        Self { id, value }
    }
}

/// Result of write operations.
#[derive(Debug, Clone)]
pub struct WriteResult {
    /// Number of successful writes.
    pub success_count: usize,

    /// IDs of failed writes with error messages.
    pub failures: Vec<(u32, String)>,
}

impl WriteResult {
    /// Create a fully successful result.
    pub fn success(count: usize) -> Self {
        Self {
            success_count: count,
            failures: vec![],
        }
    }

    /// Check if all writes succeeded.
    pub fn is_success(&self) -> bool {
        self.failures.is_empty()
    }
}

// ============================================================================
// Poll Result Types
// ============================================================================

/// Result of a poll operation (supports partial success).
///
/// Unlike `Result<DataBatch>`, this type can represent scenarios where
/// some points were read successfully while others failed.
#[derive(Debug, Clone, Default)]
pub struct PollResult {
    /// Successfully collected data points.
    pub data: DataBatch,

    /// Points that failed to read.
    pub failures: Vec<PointFailure>,
}

impl PollResult {
    /// Create a successful result with no failures.
    pub fn success(data: DataBatch) -> Self {
        Self {
            data,
            failures: vec![],
        }
    }

    /// Create a result with partial failures.
    pub fn partial(data: DataBatch, failures: Vec<PointFailure>) -> Self {
        Self { data, failures }
    }

    /// Create a failed result with no data.
    pub fn failed(failures: Vec<PointFailure>) -> Self {
        Self {
            data: DataBatch::default(),
            failures,
        }
    }

    /// Check if any points failed to read.
    pub fn has_failures(&self) -> bool {
        !self.failures.is_empty()
    }

    /// Check if poll was completely successful.
    pub fn is_success(&self) -> bool {
        self.failures.is_empty()
    }

    /// Get number of successfully read points.
    pub fn success_count(&self) -> usize {
        self.data.len()
    }

    /// Get number of failed points.
    pub fn failure_count(&self) -> usize {
        self.failures.len()
    }
}

/// Information about a point that failed to read.
#[derive(Debug, Clone, serde::Serialize)]
pub struct PointFailure {
    /// The point ID that failed.
    pub point_id: u32,

    /// Error message describing the failure.
    /// Uses `Cow<'static, str>` to avoid allocation for static error messages.
    pub error: Cow<'static, str>,
}

impl PointFailure {
    /// Create a new point failure with a static error message (zero allocation).
    pub fn new(point_id: u32, error: &'static str) -> Self {
        Self {
            point_id,
            error: Cow::Borrowed(error),
        }
    }

    /// Create a new point failure with a dynamic error message.
    pub fn with_error(point_id: u32, error: String) -> Self {
        Self {
            point_id,
            error: Cow::Owned(error),
        }
    }
}

/// Protocol diagnostics information.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Diagnostics {
    /// Protocol name.
    pub protocol: String,

    /// Connection state.
    pub connection_state: ConnectionState,

    /// Number of successful reads.
    pub read_count: u64,

    /// Number of successful writes.
    pub write_count: u64,

    /// Number of errors.
    pub error_count: u64,

    /// Last error message.
    pub last_error: Option<String>,

    /// Protocol-specific information.
    #[serde(default)]
    pub extra: serde_json::Value,
}

impl Diagnostics {
    /// Create new diagnostics.
    pub fn new(protocol: impl Into<String>) -> Self {
        Self {
            protocol: protocol.into(),
            connection_state: ConnectionState::Disconnected,
            read_count: 0,
            write_count: 0,
            error_count: 0,
            last_error: None,
            extra: serde_json::Value::Null,
        }
    }
}

/// Data event for event-driven protocols.
///
/// Note: `DataUpdate` uses `Arc<DataBatch>` to avoid deep cloning on broadcast.
/// This is a significant performance optimization for high-frequency data streams.
#[derive(Debug, Clone)]
pub enum DataEvent {
    /// Data update received (Arc-wrapped to avoid clone overhead).
    DataUpdate(Arc<DataBatch>),

    /// Connection state changed.
    ConnectionChanged(ConnectionState),

    /// Error occurred.
    Error(String),

    /// Heartbeat/keep-alive.
    Heartbeat,
}

/// Event receiver type (broadcast supports multiple subscribers).
pub type DataEventReceiver = broadcast::Receiver<DataEvent>;

/// Event sender type (broadcast supports multiple subscribers).
pub type DataEventSender = broadcast::Sender<DataEvent>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_connection_state() {
        assert!(!ConnectionState::Disconnected.is_connected());
        assert!(ConnectionState::Connected.is_connected());
    }

    #[test]
    fn test_control_command() {
        let cmd = ControlCommand::latching(1, true);
        assert!(cmd.pulse_duration_ms.is_none());

        let cmd = ControlCommand::pulse(1, true, 500);
        assert_eq!(cmd.pulse_duration_ms, Some(500));
    }

    #[test]
    fn test_poll_result_success() {
        let batch = DataBatch::new();
        let result = PollResult::success(batch);
        assert!(result.is_success());
        assert!(!result.has_failures());
        assert_eq!(result.failure_count(), 0);
    }

    #[test]
    fn test_poll_result_partial() {
        let batch = DataBatch::new();
        let failures = vec![
            PointFailure::new(1, "error 1"),
            PointFailure::new(2, "error 2"),
        ];
        let result = PollResult::partial(batch, failures);

        assert!(!result.is_success());
        assert!(result.has_failures());
        assert_eq!(result.failure_count(), 2);
    }

    #[test]
    fn test_poll_result_failed() {
        let failures = vec![PointFailure::new(1, "connection timeout")];
        let result = PollResult::failed(failures);

        assert!(!result.is_success());
        assert!(result.has_failures());
        assert_eq!(result.success_count(), 0);
        assert_eq!(result.failure_count(), 1);
    }

    #[test]
    fn test_point_failure() {
        let failure = PointFailure::new(42, "read timeout");
        assert_eq!(failure.point_id, 42);
        assert_eq!(failure.error, "read timeout");
    }

    #[test]
    fn test_write_result() {
        let result = WriteResult::success(5);
        assert!(result.is_success());
        assert_eq!(result.success_count, 5);
        assert!(result.failures.is_empty());
    }
}
