//! Channels Module (formerly ComBase)
//!
//! This module provides the base infrastructure for communication protocol implementations.
//! Actual protocol implementations are provided as plugins.

// Core modules
mod channel_creation; // Channel creation/factory methods (private, impl on ChannelManager)
pub mod channel_entry; // Channel entry types: ChannelEntry, ChannelMetadata, ChannelStats
pub mod channel_manager; // Channel lifecycle manager: ChannelManager struct + query/lifecycle
pub mod channel_task; // Unified channel task: async event loop (select! polling + commands)
mod command_guard; // Final fail-closed validation before protocol dispatch
mod runtime_config;
pub(crate) mod runtime_policy;
pub mod shm_listener; // UDS event-driven command listener with producer-side reconnect backoff
pub mod traits; // Core traits and type definitions (re-exports from types)

pub mod types; // Channel communication types (owned by io)

// Adapter-owned point compilation.
pub mod converters; // Config converters: io config → PointConfig

// Re-export data types from local types module
pub use types::{ChannelCommand, ConnectionState};

// Re-export other types from local modules
pub use crate::core::config::FourRemote;
pub(crate) use channel_creation::validate_channel_config_for_runtime;
pub use channel_entry::{ChannelEntry, ChannelMetadata, ChannelStats};
pub use channel_manager::ChannelManager;
pub use runtime_config::RuntimeChannelConfig;
pub use shm_listener::ShmCommandListener;

// Re-export converters
#[cfg(all(feature = "can", target_os = "linux"))]
pub use converters::convert_to_can_point_configs;
#[cfg(feature = "modbus")]
pub use converters::convert_to_modbus_point_configs;
