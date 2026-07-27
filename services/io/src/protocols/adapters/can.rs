//! CAN Protocol Implementation (LYNK Protocol)
//!
//! Implements CAN bus communication for Discover LYNK Serial CAN interface.

mod client;
mod config;
mod decoder;

// Re-export client and config types
pub use client::CanClient;
pub use config::{CanChannelParamsConfig, CanConfig, CanDataType, CanPoint, LynkCanId};
