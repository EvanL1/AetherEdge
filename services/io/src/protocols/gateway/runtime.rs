//! Channel runtime trait for unified protocol management.
//!
//! This module defines the `ChannelRuntime` trait, an object-safe wrapper
//! that allows heterogeneous protocol channels to be managed uniformly.

use async_trait::async_trait;

use std::sync::Arc;

use crate::protocols::core::error::Result;
use crate::protocols::core::logging::{ChannelLogConfig, ChannelLogHandler};
use crate::protocols::core::traits::{ConnectionState, DataEventReceiver, Diagnostics, PollResult};

/// Object-safe wrapper for protocol channels.
///
/// This trait provides a unified interface for managing different protocol
/// channels (Modbus, IEC104, OPC UA, etc.) in the gateway runtime.
///
/// # Design Rationale
///
/// The core protocol traits (`ProtocolClient`, `EventDrivenProtocol`) use
/// `impl Future` return types which are not object-safe. This wrapper uses
/// `async_trait` to enable dynamic dispatch via `Box<dyn ChannelRuntime>`.
#[async_trait]
pub trait ChannelRuntime: Send + Sync {
    // === Identity ===

    /// Channel unique identifier.
    fn id(&self) -> u32;

    /// Channel display name.
    fn name(&self) -> &str;

    /// Protocol name (e.g., "modbus", "iec104", "opcua").
    fn protocol(&self) -> &str;

    /// Whether this channel is event-driven (vs polling).
    fn is_event_driven(&self) -> bool;

    // === Lifecycle ===

    /// Connect to the remote device/server.
    async fn connect(&mut self) -> Result<()>;

    /// Disconnect from the remote device/server.
    async fn disconnect(&mut self) -> Result<()>;

    // === Data Operations ===

    /// Poll data once (for polling channels).
    ///
    /// Event-driven channels may return cached data or empty batch.
    async fn poll_once(&mut self) -> PollResult;

    /// Write control commands.
    async fn write_control(&mut self, commands: &[(u32, f64)]) -> Result<usize>;

    /// Write adjustment commands.
    async fn write_adjustment(&mut self, adjustments: &[(u32, f64)]) -> Result<usize>;

    // === Event-Driven Support ===

    /// Subscribe to data events (event-driven channels only).
    ///
    /// Returns `None` for polling-only channels.
    fn subscribe(&self) -> Option<DataEventReceiver>;

    /// Start event streaming (event-driven channels only).
    async fn start_events(&mut self) -> Result<()>;

    /// Stop event streaming (event-driven channels only).
    async fn stop_events(&mut self) -> Result<()>;

    // === Diagnostics ===

    /// Get channel diagnostics.
    async fn diagnostics(&self) -> Result<Diagnostics>;

    /// Get current connection state.
    fn connection_state(&self) -> ConnectionState;

    // === Logging ===

    /// Set the log handler for this channel.
    ///
    /// Default implementation does nothing.
    fn set_log_handler(&mut self, _handler: Arc<dyn ChannelLogHandler>) {}

    /// Set the log configuration for this channel.
    ///
    /// Default implementation does nothing.
    fn set_log_config(&mut self, _config: ChannelLogConfig) {}

    /// Get the log handler for this channel.
    ///
    /// Returns `None` by default. Protocols that support hot-reload of log levels
    /// should override this to return their handler reference.
    fn log_handler(&self) -> Option<Arc<dyn ChannelLogHandler>> {
        None
    }
}

/// Channel communication mode.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum ChannelMode {
    /// Polling mode: data is fetched periodically via `poll_once()`.
    #[default]
    Polling,
    /// Event-driven mode: data is pushed via `subscribe()`.
    EventDriven,
    /// Hybrid mode: both polling and event-driven.
    Hybrid,
}
