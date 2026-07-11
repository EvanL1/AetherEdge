//! CAN Protocol Implementation (LYNK Protocol)
//!
//! Implements CAN bus communication for Discover LYNK Serial CAN interface.

mod client;
mod config;
mod decoder;

#[cfg(feature = "j1939")]
pub mod j1939;

// Re-export client and config types
pub use client::CanClient;
pub use config::{CanChannelParamsConfig, CanConfig, CanDataType, CanPoint, LynkCanId};

#[cfg(feature = "j1939")]
pub use j1939::{J1939Client, J1939Config};
