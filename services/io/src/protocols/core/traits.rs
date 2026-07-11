//! Core traits for protocol implementations.
//!
//! This module defines the fundamental traits that all protocols must implement.
//!
//! # Trait Hierarchy
//!
//! ```text
//! Layer 1: Basic Capabilities (stateless queries)
//! ├── ProtocolCapabilities  // metadata: name, modes, version
//! └── Protocol              // connection_state, diagnostics
//!
//! Layer 2: Core Operations (single responsibility)
//! ├── ProtocolClient        // connect, disconnect, poll_once, write_*
//! └── EventDrivenProtocol   // event_stream (broadcast)
//!
//! Layer 3: Optional Extensions
//! └── ProtocolServer        // listen, stop, connected_clients
//! ```

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::future::Future;
use std::sync::Arc;
use tokio::sync::broadcast;

use crate::protocols::core::data::DataBatch;
use crate::protocols::core::error::Result;

/// Communication mode supported by a protocol.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommunicationMode {
    /// Polling mode - actively request data at intervals.
    ///
    /// Used by: Modbus, BACnet (read), etc.
    Polling,

    /// Event-driven mode - passively receive data updates.
    ///
    /// Used by: IEC 104 (spontaneous), OPC UA (subscriptions), etc.
    EventDriven,

    /// Hybrid mode - supports both polling and events.
    ///
    /// Used by: DNP3, OPC UA, etc.
    Hybrid,
}

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

/// Request for reading data points.
///
/// Simple request type for protocol-layer reads. The application layer
/// is responsible for any SCADA-level filtering (by type, etc.).
#[derive(Debug, Clone, Default)]
pub struct ReadRequest {
    /// Point IDs to read (None = all configured points)
    pub point_ids: Option<Vec<u32>>,
}

impl ReadRequest {
    /// Create a request for all configured points.
    pub fn all() -> Self {
        Self { point_ids: None }
    }
}

/// Response from reading data points.
#[derive(Debug, Clone)]
pub struct ReadResponse {
    /// The data batch containing all read points.
    pub data: DataBatch,

    /// Number of points that failed to read.
    pub failed_count: usize,
}

// ReadResponse constructors are added as needed by protocol adapters.

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

/// Polling configuration.
#[derive(Debug, Clone)]
pub struct PollingConfig {
    /// Polling interval in milliseconds.
    pub interval_ms: u64,

    /// Point IDs to poll (None = all configured points).
    pub point_ids: Option<Vec<u32>>,

    /// Whether to continue on individual point errors.
    pub continue_on_error: bool,
}

impl Default for PollingConfig {
    fn default() -> Self {
        Self {
            interval_ms: 1000,
            point_ids: None,
            continue_on_error: true,
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

/// Protocol capabilities description.
pub trait ProtocolCapabilities {
    /// Get the protocol name.
    fn name(&self) -> &'static str;

    /// Get supported communication modes.
    fn supported_modes(&self) -> &[CommunicationMode];

    /// Check if client role is supported.
    fn supports_client(&self) -> bool {
        true
    }

    /// Check if server role is supported.
    fn supports_server(&self) -> bool {
        false
    }

    /// Get protocol version.
    fn version(&self) -> &'static str {
        "1.0"
    }
}

/// Base protocol trait - connection status and diagnostics.
///
/// This trait provides read-only access to protocol state. For data acquisition,
/// use `ProtocolClient::poll_once()` instead.
pub trait Protocol: ProtocolCapabilities + Send + Sync {
    /// Get current connection state.
    fn connection_state(&self) -> ConnectionState;

    /// Get diagnostics information.
    ///
    /// Returns protocol statistics including read/write counts, error counts,
    /// and protocol-specific extra information.
    fn diagnostics(&self) -> impl Future<Output = Result<Diagnostics>> + Send;
}

/// Client protocol trait - active connection + data operations.
///
/// This trait combines connection lifecycle management with data acquisition
/// and command writing capabilities.
pub trait ProtocolClient: Protocol {
    /// Connect to the target device/server.
    fn connect(&mut self) -> impl Future<Output = Result<()>> + Send;

    /// Disconnect from the target.
    fn disconnect(&mut self) -> impl Future<Output = Result<()>> + Send;

    /// Execute a single poll cycle and return collected data.
    ///
    /// This is the primary method for data acquisition. The caller (service layer)
    /// is responsible for:
    /// - Managing the polling loop (interval, scheduling)
    /// - Storing the returned data
    /// - Handling reconnection on failures
    ///
    /// # Returns
    ///
    /// A `PollResult` containing:
    /// - `data`: Successfully read data points
    /// - `failures`: Points that failed to read (partial success supported)
    fn poll_once(&mut self) -> impl Future<Output = PollResult> + Send;

    /// Write control commands (remote control).
    ///
    /// Control commands are boolean operations (ON/OFF, OPEN/CLOSE) with
    /// optional pulse duration for momentary outputs.
    fn write_control(
        &mut self,
        commands: &[ControlCommand],
    ) -> impl Future<Output = Result<WriteResult>> + Send;

    /// Write adjustment commands (remote adjustment).
    ///
    /// Adjustment commands are setpoint operations with floating-point values.
    fn write_adjustment(
        &mut self,
        adjustments: &[AdjustmentCommand],
    ) -> impl Future<Output = Result<WriteResult>> + Send;
}

/// Server protocol trait - passive connection acceptance.
pub trait ProtocolServer: Protocol {
    /// Start listening on the specified address.
    fn listen(&mut self, addr: &str) -> impl Future<Output = Result<()>> + Send;

    /// Stop listening and close all connections.
    fn stop(&mut self) -> impl Future<Output = Result<()>> + Send;

    /// Get number of connected clients.
    fn connected_clients(&self) -> usize;
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

/// Event handler trait.
///
/// This trait uses `async_trait` because it needs to be object-safe for `dyn DataEventHandler`.
/// Uses `Arc<DataBatch>` to enable zero-copy sharing between event_tx and handler.
#[async_trait]
pub trait DataEventHandler: Send + Sync {
    /// Handle data update event (Arc for zero-copy sharing).
    async fn on_data_update(&self, batch: Arc<DataBatch>);

    /// Handle connection state change.
    async fn on_connection_changed(&self, state: ConnectionState);

    /// Handle error event.
    async fn on_error(&self, error: &str);
}

/// Event-driven protocol extension trait.
///
/// Protocols implementing this trait can push data events to subscribers
/// instead of (or in addition to) being polled.
pub trait EventDrivenProtocol: Protocol {
    /// Subscribe to data events.
    ///
    /// Returns a broadcast receiver that will receive all events from this protocol.
    /// Multiple subscribers can call this method and each will receive all events.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let mut rx = protocol.subscribe();
    /// tokio::spawn(async move {
    ///     while let Ok(event) = rx.recv().await {
    ///         match event {
    ///             DataEvent::DataUpdate(batch) => { /* handle data */ }
    ///             DataEvent::ConnectionChanged(state) => { /* handle state change */ }
    ///             _ => {}
    ///         }
    ///     }
    /// });
    /// ```
    fn subscribe(&self) -> DataEventReceiver;

    /// Set event handler for callback-style event processing.
    ///
    /// This is an alternative to `subscribe()` for protocols that prefer
    /// callback-based event handling.
    fn set_event_handler(&mut self, handler: Arc<dyn DataEventHandler>);

    /// Start receiving events from the device/server.
    ///
    /// For IEC 104, this triggers STARTDT. For OPC UA, this activates subscriptions.
    fn start(&mut self) -> impl Future<Output = Result<()>> + Send;

    /// Stop receiving events.
    ///
    /// For IEC 104, this triggers STOPDT. For OPC UA, this deactivates subscriptions.
    fn stop(&mut self) -> impl Future<Output = Result<()>> + Send;
}

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
