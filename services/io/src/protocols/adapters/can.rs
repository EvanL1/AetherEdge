//! CAN Protocol Implementation (LYNK Protocol)
//!
//! Implements CAN bus communication for Discover LYNK Serial CAN interface.

mod client;

pub use super::can_types::{CanChannelParamsConfig, CanConfig, CanDataType, CanPoint, LynkCanId};
pub use client::CanClient;
