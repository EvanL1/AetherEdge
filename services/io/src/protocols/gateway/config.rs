//! Gateway configuration types.
//!
//! Defines the TOML-friendly configuration format for the gateway.

use aether_model::PointType;
use serde::{Deserialize, Serialize};

use crate::protocols::core::point::TransformConfig;

/// Gateway configuration (top-level).
///
/// # Example TOML
///
/// ```toml
/// [gateway]
/// name = "My Gateway"
/// default_poll_interval_ms = 1000
///
/// [[channels]]
/// id = 1
/// name = "PLC1"
/// protocol = "modbus"
/// enabled = true
///
/// [channels.parameters]
/// host = "192.168.1.100"
/// port = 502
///
/// [[channels.points]]
/// id = 1001
/// name = "Temperature"
/// address = "1:100"
/// ```
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct GatewayConfig {
    /// Gateway global settings.
    pub gateway: GatewayGlobalConfig,

    /// Channel configurations.
    #[serde(default)]
    pub channels: Vec<ChannelConfig>,
}

/// Gateway global settings.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct GatewayGlobalConfig {
    /// Gateway name for identification.
    pub name: String,

    /// Default polling interval in milliseconds.
    #[serde(default = "default_poll_interval")]
    pub default_poll_interval_ms: u64,

    /// Diagnostics snapshot interval in milliseconds.
    #[serde(default = "default_diagnostics_interval")]
    pub diagnostics_interval_ms: u64,

    /// Enable JSON Lines output for events.
    #[serde(default)]
    pub jsonl_output: bool,
}

fn default_poll_interval() -> u64 {
    1000
}

fn default_diagnostics_interval() -> u64 {
    5000
}

impl Default for GatewayGlobalConfig {
    fn default() -> Self {
        Self {
            name: "Voltage".to_string(),
            default_poll_interval_ms: default_poll_interval(),
            diagnostics_interval_ms: default_diagnostics_interval(),
            jsonl_output: false,
        }
    }
}

/// Channel configuration.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ChannelConfig {
    /// Channel unique identifier.
    pub id: u32,

    /// Channel display name.
    pub name: String,

    /// Protocol type: "modbus", "iec104", "opcua", "can", "gpio", "virtual".
    pub protocol: String,

    /// Whether this channel is enabled.
    #[serde(default = "default_true")]
    pub enabled: bool,

    /// Channel mode: "polling", "event", or "hybrid".
    #[serde(default)]
    pub mode: ChannelModeConfig,

    /// Polling interval override (uses gateway default if not set).
    pub poll_interval_ms: Option<u64>,

    /// Protocol-specific parameters (JSON object).
    #[serde(default)]
    pub parameters: serde_json::Value,

    /// Point definitions.
    #[serde(default)]
    pub points: Vec<PointDef>,
}

fn default_true() -> bool {
    true
}

/// Channel mode configuration.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ChannelModeConfig {
    /// Polling mode (default for most protocols).
    #[default]
    Polling,
    /// Event-driven mode (for IEC104, OPC UA, CAN).
    Event,
    /// Hybrid mode (both polling and events).
    Hybrid,
}

/// Point definition with simplified address format.
///
/// The `address` field uses a protocol-specific shorthand format:
/// - Modbus: "slave_id:register" (e.g., "1:100")
/// - IEC104: "ioa" (e.g., "1001")
/// - OPC UA: "ns=N;i=ID" or "ns=N;s=Name" (e.g., "ns=2;i=1234")
/// - CAN: "can_id:byte_offset:bit_pos:bit_len" (e.g., "0x100:0:0:16")
/// - GPIO: "pin_number" (e.g., "17")
/// - Virtual: "key" (e.g., "temperature")
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PointDef {
    /// Point unique identifier.
    pub id: u32,

    /// SCADA point type (T/S/C/A). Defaults to Telemetry.
    #[serde(default = "default_point_type")]
    pub point_type: PointType,

    /// Point display name.
    pub name: String,

    /// Protocol-specific address (shorthand format).
    pub address: String,

    /// Data transformation configuration.
    #[serde(default)]
    pub transform: TransformConfig,

    /// Whether this point is enabled.
    #[serde(default = "default_true")]
    pub enabled: bool,
}

fn default_point_type() -> PointType {
    PointType::Telemetry
}

/// Configuration error.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("IO error: {0}")]
    Io(String),

    #[error("Parse error: {0}")]
    Parse(String),

    #[error("Validation error: {0}")]
    Validation(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_channel_mode_default() {
        let mode = ChannelModeConfig::default();
        assert_eq!(mode, ChannelModeConfig::Polling);
    }
}
