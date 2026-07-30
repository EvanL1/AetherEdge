//! CAN Protocol Implementation (LYNK Protocol)
//!
//! Implements CAN bus communication for Discover LYNK Serial CAN interface.

#[cfg(target_os = "linux")]
mod client;
pub mod config;
pub mod decoder;

#[cfg(feature = "j1939")]
pub mod j1939;

// Re-export the Linux socket client and cross-platform configuration types.
#[cfg(target_os = "linux")]
pub use client::CanClient;
pub use config::{CanChannelParamsConfig, CanConfig, CanDataType, CanPoint, LynkCanId};

#[cfg(all(feature = "j1939", target_os = "linux"))]
pub use j1939::{J1939Client, J1939Config, J1939PointConfig};
