//! Virtual channel for data aggregation and relay.
//!
//! A virtual channel does not connect to any physical device.
//! It serves as a data hub for aggregating data from multiple sources
//! or as an intermediate point for protocol conversion.
//!
//! # Architecture (No DataStore Dependency)
//!
//! `VirtualChannel` manages its own internal data buffer. Data is pushed
//! to it via `write()` and can be retrieved via `poll_once()`.
//!
//! # Example
//!
//! ```rust,ignore
//! use crate::protocols::adapters::virtual_channel::{VirtualChannel, VirtualChannelConfig};
//!
//! let config = VirtualChannelConfig::new("data_hub");
//! let mut channel = VirtualChannel::new(config);
//!
//! // Push data from any source
//! channel.write_point(DataPoint::telemetry(1, 25.5)).await?;
//!
//! // Get accumulated data (service layer handles storage)
//! let batch = channel.poll_once().await?;
//! store.write_batch(channel_id, &batch).await?;
//! ```

use std::sync::Arc;

use dashmap::DashMap;
use tokio::sync::broadcast;

use crate::protocols::core::data::{DataBatch, DataPoint};
use crate::protocols::core::diagnostics::AtomicDiagnostics;
use crate::protocols::core::error::Result;
use crate::protocols::core::metadata::{
    DriverMetadata, HasMetadata, ParameterMetadata, ParameterType,
};
use crate::protocols::core::point::PointConfig;
use crate::protocols::core::traits::{
    AdjustmentCommand, CommunicationMode, ConnectionState, ControlCommand, DataEvent,
    DataEventHandler, DataEventReceiver, DataEventSender, Diagnostics, EventDrivenProtocol,
    PollResult, Protocol, ProtocolCapabilities, ProtocolClient, WriteResult,
};
use crate::protocols::gateway::ChannelRuntime;
use async_trait::async_trait;
use serde::Deserialize;

/// Virtual channel configuration.
#[derive(Debug, Clone)]
pub struct VirtualChannelConfig {
    /// Channel name for identification.
    pub name: String,

    /// Point configurations (defines accepted points).
    pub points: Vec<PointConfig>,

    /// Event buffer size.
    pub buffer_size: usize,
}

impl Default for VirtualChannelConfig {
    fn default() -> Self {
        Self {
            name: "virtual".to_string(),
            points: Vec::new(),
            buffer_size: 1024,
        }
    }
}

impl VirtualChannelConfig {
    /// Create a new virtual channel configuration.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            ..Default::default()
        }
    }

    /// Add point configurations.
    pub fn with_points(mut self, points: Vec<PointConfig>) -> Self {
        self.points = points;
        self
    }

    /// Set buffer size.
    pub fn with_buffer_size(mut self, size: usize) -> Self {
        self.buffer_size = size;
        self
    }
}

// ============================================================================
// Strongly-typed mapping configs for JSON deserialization
// ============================================================================

/// Virtual channel parameters configuration (deserialized from parameters_json).
///
/// Virtual channels are simple data hubs and don't require complex configuration.
///
/// # Example JSON
/// ```json
/// {
///     "name": "data_hub",
///     "buffer_size": 2048
/// }
/// ```
#[derive(Debug, Clone, Deserialize, Default)]
pub struct VirtualChannelParamsConfig {
    /// Channel name for identification.
    #[serde(default = "default_virtual_name")]
    pub name: String,

    /// Event buffer size.
    #[serde(default = "default_buffer_size")]
    pub buffer_size: usize,
}

fn default_virtual_name() -> String {
    "virtual".to_string()
}

fn default_buffer_size() -> usize {
    1024
}

impl VirtualChannelParamsConfig {
    /// Convert to VirtualChannelConfig.
    pub fn to_config(&self) -> VirtualChannelConfig {
        VirtualChannelConfig::new(&self.name).with_buffer_size(self.buffer_size)
    }
}

/// Virtual channel implementation.
///
/// This channel type:
/// - Accepts data writes from any source via `write()` or `write_point()`
/// - Stores data internally (no external DataStore dependency)
/// - Emits events when data is written (broadcast to all subscribers)
/// - Returns accumulated data via `poll_once()`
pub struct VirtualChannel {
    /// Channel unique identifier.
    channel_id: u32,
    config: VirtualChannelConfig,
    /// Internal data buffer: point_id -> DataPoint
    data_buffer: DashMap<u32, DataPoint>,
    diagnostics: Arc<AtomicDiagnostics>,
    /// Broadcast sender for event-driven subscribers.
    event_tx: DataEventSender,
    event_handler: Option<Arc<dyn DataEventHandler>>,
}

impl VirtualChannel {
    /// Create a new virtual channel.
    pub fn new(config: VirtualChannelConfig, channel_id: u32) -> Self {
        // Use broadcast channel for multiple subscribers
        let (event_tx, _) = broadcast::channel(config.buffer_size);

        Self {
            channel_id,
            config,
            data_buffer: DashMap::new(),
            diagnostics: Arc::new(AtomicDiagnostics::new()),
            event_tx,
            event_handler: None,
        }
    }

    /// Get channel ID.
    pub fn id(&self) -> u32 {
        self.channel_id
    }

    /// Get the channel name.
    pub fn name(&self) -> &str {
        &self.config.name
    }

    /// Write a data batch directly to this channel.
    ///
    /// This is the primary method for feeding data into a virtual channel.
    /// Data is stored internally and can be retrieved via `poll_once()`.
    pub async fn write(&self, batch: &DataBatch) -> Result<()> {
        // Store to internal buffer
        for point in batch.iter() {
            self.data_buffer.insert(point.id, point.clone());
        }

        // Emit event to all subscribers - Arc for zero-copy sharing
        // Note: batch is &DataBatch, so we clone once then share via Arc
        let batch_arc = Arc::new(batch.clone());
        let _ = self
            .event_tx
            .send(DataEvent::DataUpdate(Arc::clone(&batch_arc)));

        // Update diagnostics (lock-free)
        self.diagnostics.inc_write();

        // Call event handler if set
        if let Some(handler) = &self.event_handler {
            handler.on_data_update(batch_arc).await;
        }

        Ok(())
    }

    /// Write a single data point.
    pub async fn write_point(&self, point: DataPoint) -> Result<()> {
        let mut batch = DataBatch::with_capacity(1);
        batch.add(point);
        self.write(&batch).await
    }

    /// Get all points currently in the buffer.
    fn get_all_points(&self) -> DataBatch {
        let mut batch = DataBatch::with_capacity(self.data_buffer.len());
        for entry in self.data_buffer.iter() {
            batch.add(entry.value().clone());
        }
        batch
    }
}

impl HasMetadata for VirtualChannel {
    #[allow(clippy::disallowed_methods)] // json! macro
    fn metadata() -> DriverMetadata {
        DriverMetadata {
            name: "virtual",
            display_name: "Virtual Channel",
            description: "Virtual channel for data aggregation, testing, and simulation. Does not connect to physical devices.",
            is_recommended: true,
            example_config: serde_json::json!({
                "name": "virtual_hub",
                "buffer_size": 1000,
                "mode": "aggregation"
            }),
            parameters: vec![
                ParameterMetadata::required(
                    "name",
                    "Name",
                    "Virtual channel name for identification",
                    ParameterType::String,
                ),
                ParameterMetadata::optional(
                    "buffer_size",
                    "Buffer Size",
                    "Maximum number of data points to buffer",
                    ParameterType::Integer,
                    serde_json::json!(1000),
                ),
                ParameterMetadata::optional(
                    "mode",
                    "Mode",
                    "Channel mode: 'aggregation' or 'simulation'",
                    ParameterType::String,
                    serde_json::json!("aggregation"),
                ),
            ],
        }
    }
}

impl ProtocolCapabilities for VirtualChannel {
    fn name(&self) -> &'static str {
        "Virtual"
    }

    fn supported_modes(&self) -> &[CommunicationMode] {
        &[CommunicationMode::EventDriven]
    }
}

impl Protocol for VirtualChannel {
    fn connection_state(&self) -> ConnectionState {
        // Virtual channels are always "connected"
        ConnectionState::Connected
    }

    #[allow(clippy::disallowed_methods)] // json! macro
    async fn diagnostics(&self) -> Result<Diagnostics> {
        Ok(Diagnostics {
            protocol: "Virtual".to_string(),
            connection_state: ConnectionState::Connected,
            read_count: self.diagnostics.read_count(),
            write_count: self.diagnostics.write_count(),
            error_count: self.diagnostics.error_count(),
            last_error: self.diagnostics.last_error(),
            extra: serde_json::json!({
                "name": self.config.name,
                "points_stored": self.data_buffer.len(),
            }),
        })
    }
}

impl ProtocolClient for VirtualChannel {
    async fn connect(&mut self) -> Result<()> {
        // Virtual channel is always connected - no-op
        Ok(())
    }

    async fn disconnect(&mut self) -> Result<()> {
        // No-op for virtual channel
        Ok(())
    }

    /// Poll returns all data currently in the buffer.
    ///
    /// For virtual channels, data is pushed (not polled), so this returns
    /// the accumulated data. The service layer should call this to get
    /// data that was pushed via `write()`.
    async fn poll_once(&mut self) -> PollResult {
        let batch = self.get_all_points();
        self.diagnostics.inc_read();
        PollResult::success(batch)
    }

    async fn write_control(&mut self, commands: &[ControlCommand]) -> Result<WriteResult> {
        // For virtual channels, control commands are stored as data points
        let mut batch = DataBatch::new();
        for cmd in commands {
            batch.add(DataPoint::control(cmd.id, cmd.value));
        }
        self.write(&batch).await?;
        Ok(WriteResult::success(commands.len()))
    }

    async fn write_adjustment(&mut self, adjustments: &[AdjustmentCommand]) -> Result<WriteResult> {
        // For virtual channels, adjustments are stored as data points
        let mut batch = DataBatch::new();
        for adj in adjustments {
            batch.add(DataPoint::adjustment(adj.id, adj.value));
        }
        self.write(&batch).await?;
        Ok(WriteResult::success(adjustments.len()))
    }
}

impl EventDrivenProtocol for VirtualChannel {
    fn subscribe(&self) -> DataEventReceiver {
        // Broadcast channel supports multiple subscribers
        // Each call to subscribe() returns a new receiver that gets all future events
        self.event_tx.subscribe()
    }

    fn set_event_handler(&mut self, handler: Arc<dyn DataEventHandler>) {
        self.event_handler = Some(handler);
    }

    async fn start(&mut self) -> Result<()> {
        // Virtual channel is always "started" - no-op
        Ok(())
    }

    async fn stop(&mut self) -> Result<()> {
        // Virtual channel doesn't need explicit stop - no-op
        Ok(())
    }
}

// ============================================================================
// ChannelRuntime implementation (direct, no wrapper needed)
// ============================================================================

#[async_trait]
impl ChannelRuntime for VirtualChannel {
    fn id(&self) -> u32 {
        self.channel_id
    }

    fn name(&self) -> &str {
        &self.config.name
    }

    fn protocol(&self) -> &str {
        "virtual"
    }

    fn is_event_driven(&self) -> bool {
        true
    }

    async fn connect(&mut self) -> Result<()> {
        <Self as ProtocolClient>::connect(self).await
    }

    async fn disconnect(&mut self) -> Result<()> {
        <Self as ProtocolClient>::disconnect(self).await
    }

    async fn poll_once(&mut self) -> PollResult {
        <Self as ProtocolClient>::poll_once(self).await
    }

    async fn write_control(&mut self, commands: &[(u32, f64)]) -> Result<usize> {
        let cmds: Vec<_> = commands
            .iter()
            .map(|(id, value)| ControlCommand::latching(*id, *value != 0.0))
            .collect();
        let result = <Self as ProtocolClient>::write_control(self, &cmds).await?;
        Ok(result.success_count)
    }

    async fn write_adjustment(&mut self, adjustments: &[(u32, f64)]) -> Result<usize> {
        let adjs: Vec<_> = adjustments
            .iter()
            .map(|(id, value)| AdjustmentCommand::new(*id, *value))
            .collect();
        let result = <Self as ProtocolClient>::write_adjustment(self, &adjs).await?;
        Ok(result.success_count)
    }

    fn subscribe(&self) -> Option<DataEventReceiver> {
        Some(<Self as EventDrivenProtocol>::subscribe(self))
    }

    async fn start_events(&mut self) -> Result<()> {
        <Self as EventDrivenProtocol>::start(self).await
    }

    async fn stop_events(&mut self) -> Result<()> {
        <Self as EventDrivenProtocol>::stop(self).await
    }

    async fn diagnostics(&self) -> Result<Diagnostics> {
        <Self as Protocol>::diagnostics(self).await
    }

    fn connection_state(&self) -> ConnectionState {
        <Self as Protocol>::connection_state(self)
    }
}

#[cfg(test)]
#[allow(clippy::disallowed_methods)] // unwrap in tests
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_virtual_channel_write_poll() {
        let config = VirtualChannelConfig::new("test_channel");
        let mut channel = VirtualChannel::new(config, 1);

        // Write a point
        channel
            .write_point(DataPoint::telemetry(1, 25.5))
            .await
            .unwrap();

        // Poll it back (use UFCS to disambiguate)
        let result = ProtocolClient::poll_once(&mut channel).await;
        assert!(result.is_success());
        assert_eq!(result.data.len(), 1);

        let point = result.data.iter().next().unwrap();
        assert_eq!(point.id, 1);
    }

    #[tokio::test]
    async fn test_virtual_channel_always_connected() {
        let config = VirtualChannelConfig::new("test");
        let channel = VirtualChannel::new(config, 1);

        assert_eq!(
            Protocol::connection_state(&channel),
            ConnectionState::Connected
        );
    }

    #[tokio::test]
    async fn test_virtual_channel_poll_once() {
        let config = VirtualChannelConfig::new("poll_test");
        let mut channel = VirtualChannel::new(config, 1);

        // Write some data
        channel
            .write_point(DataPoint::telemetry(1, 1.0))
            .await
            .unwrap();
        channel
            .write_point(DataPoint::telemetry(2, 2.0))
            .await
            .unwrap();

        // Poll returns accumulated data as PollResult (use UFCS)
        let result = ProtocolClient::poll_once(&mut channel).await;
        assert!(result.is_success());
        assert_eq!(result.data.len(), 2);
    }

    #[tokio::test]
    async fn test_virtual_channel_diagnostics() {
        let config = VirtualChannelConfig::new("diag_test");
        let channel = VirtualChannel::new(config, 1);

        channel
            .write_point(DataPoint::telemetry(1, 1.0))
            .await
            .unwrap();
        channel
            .write_point(DataPoint::telemetry(2, 2.0))
            .await
            .unwrap();

        // Use UFCS to disambiguate
        let diag = Protocol::diagnostics(&channel).await.unwrap();
        assert_eq!(diag.write_count, 2);
        assert_eq!(diag.protocol, "Virtual");
    }

    #[tokio::test]
    async fn test_virtual_channel_broadcast_subscribe() {
        let config = VirtualChannelConfig::new("broadcast_test");
        let channel = VirtualChannel::new(config, 1);

        // Create two subscribers (use UFCS to call EventDrivenProtocol::subscribe)
        let mut rx1 = EventDrivenProtocol::subscribe(&channel);
        let mut rx2 = EventDrivenProtocol::subscribe(&channel);

        // Write data
        channel
            .write_point(DataPoint::telemetry(1, 42.0))
            .await
            .unwrap();

        // Both subscribers should receive the event
        let event1 = rx1.recv().await.unwrap();
        let event2 = rx2.recv().await.unwrap();

        match (event1, event2) {
            (DataEvent::DataUpdate(b1), DataEvent::DataUpdate(b2)) => {
                assert_eq!(b1.len(), 1);
                assert_eq!(b2.len(), 1);
            },
            _ => panic!("Expected DataUpdate events"),
        }
    }
}
