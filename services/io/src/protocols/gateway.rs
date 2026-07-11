//! Gateway module - Channel runtime abstraction layer
//!
//! This module provides:
//! - `ChannelRuntime` trait: Unified protocol channel interface
//! - `factory`: Factory functions for creating channels from configuration
//! - Configuration types and address parsing
//!
//! Protocol adapters (e.g., ModbusChannel, Iec104Channel) implement
//! the `ChannelRuntime` trait directly, without extra wrapper layers.

mod address;
mod config;
pub mod factory;
mod runtime;

// Public exports
pub use address::parse_address;
pub use config::{
    ChannelConfig, ChannelModeConfig, ConfigError, GatewayConfig, GatewayGlobalConfig, PointDef,
};
pub use runtime::{ChannelMode, ChannelRuntime};
