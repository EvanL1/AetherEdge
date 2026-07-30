//! Protocol channel runtime trait for unified protocol management.
//!
//! This module defines the `ChannelRuntime` trait, an object-safe wrapper
//! that allows heterogeneous protocol channels to be managed uniformly.

use async_trait::async_trait;

use std::sync::Arc;

use crate::protocols::core::error::{GatewayError, Result};
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
    /// Whether this channel is event-driven (vs polling).
    fn is_event_driven(&self) -> bool {
        false
    }

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
    async fn write_control(&mut self, _commands: &[(u32, f64)]) -> Result<usize> {
        Err(GatewayError::Unsupported(
            "channel does not support control commands".to_string(),
        ))
    }

    /// Write adjustment commands.
    async fn write_adjustment(&mut self, _adjustments: &[(u32, f64)]) -> Result<usize> {
        Err(GatewayError::Unsupported(
            "channel does not support adjustment commands".to_string(),
        ))
    }

    // === Event-Driven Support ===

    /// Take the sole data-event receiver (event-driven channels only).
    ///
    /// Returns `None` for polling-only channels or after it has already been taken.
    fn take_event_receiver(&mut self) -> Option<DataEventReceiver> {
        None
    }

    /// Start event streaming (event-driven channels only).
    async fn start_events(&mut self) -> Result<()> {
        Ok(())
    }

    /// Stop event streaming (event-driven channels only).
    async fn stop_events(&mut self) -> Result<()> {
        Ok(())
    }

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
}
