//! Matter protocol adapter configuration.

use serde::{Deserialize, Serialize};
use std::time::Duration;

/// Matter channel parameters (from database config JSON).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MatterParamsConfig {
    /// Matter Node ID of the target device
    pub device_id: u64,

    /// Fabric ID (optional, for multi-fabric setups)
    #[serde(default)]
    pub fabric_id: Option<u64>,

    /// Pairing PIN code (for first-time commissioning)
    #[serde(default)]
    pub pin_code: Option<u32>,

    /// Device discriminator (for first-time commissioning)
    #[serde(default)]
    pub discriminator: Option<u16>,

    /// Known IP address (skip mDNS discovery)
    #[serde(default)]
    pub ip_address: Option<String>,

    /// Device port (default 5540)
    #[serde(default = "default_port")]
    pub port: u16,

    /// Subscription minimum interval in seconds
    #[serde(default = "default_subscribe_min")]
    pub subscribe_min_interval: u16,

    /// Subscription maximum interval in seconds
    #[serde(default = "default_subscribe_max")]
    pub subscribe_max_interval: u16,

    /// Connection timeout in milliseconds
    #[serde(default = "default_connect_timeout_ms")]
    pub connect_timeout_ms: u64,

    /// Reconnect interval in milliseconds
    #[serde(default = "default_reconnect_interval_ms")]
    pub reconnect_interval_ms: u64,
}

fn default_port() -> u16 {
    5540
}
fn default_subscribe_min() -> u16 {
    1
}
fn default_subscribe_max() -> u16 {
    60
}
fn default_connect_timeout_ms() -> u64 {
    10000
}
fn default_reconnect_interval_ms() -> u64 {
    5000
}

impl Default for MatterParamsConfig {
    fn default() -> Self {
        Self {
            device_id: 0,
            fabric_id: None,
            pin_code: None,
            discriminator: None,
            ip_address: None,
            port: default_port(),
            subscribe_min_interval: default_subscribe_min(),
            subscribe_max_interval: default_subscribe_max(),
            connect_timeout_ms: default_connect_timeout_ms(),
            reconnect_interval_ms: default_reconnect_interval_ms(),
        }
    }
}

/// Matter runtime configuration.
#[derive(Debug, Clone)]
pub struct MatterConfig {
    pub device_id: u64,
    pub fabric_id: Option<u64>,
    pub pin_code: Option<u32>,
    pub discriminator: Option<u16>,
    pub ip_address: Option<String>,
    pub port: u16,
    pub subscribe_min_interval: u16,
    pub subscribe_max_interval: u16,
    pub connect_timeout: Duration,
    pub reconnect_interval: Duration,
}

impl MatterParamsConfig {
    pub fn to_config(&self) -> MatterConfig {
        MatterConfig {
            device_id: self.device_id,
            fabric_id: self.fabric_id,
            pin_code: self.pin_code,
            discriminator: self.discriminator,
            ip_address: self.ip_address.clone(),
            port: self.port,
            subscribe_min_interval: self.subscribe_min_interval,
            subscribe_max_interval: self.subscribe_max_interval,
            connect_timeout: Duration::from_millis(self.connect_timeout_ms),
            reconnect_interval: Duration::from_millis(self.reconnect_interval_ms),
        }
    }
}

#[cfg(test)]
#[allow(clippy::disallowed_methods)]
mod tests {
    use super::*;

    #[test]
    fn test_matter_params_default() {
        let params = MatterParamsConfig::default();
        assert_eq!(params.device_id, 0);
        assert_eq!(params.port, 5540);
        assert_eq!(params.subscribe_min_interval, 1);
        assert_eq!(params.subscribe_max_interval, 60);
        assert_eq!(params.connect_timeout_ms, 10000);
        assert_eq!(params.reconnect_interval_ms, 5000);
        assert!(params.ip_address.is_none());
        assert!(params.fabric_id.is_none());
        assert!(params.pin_code.is_none());
        assert!(params.discriminator.is_none());
    }

    #[test]
    fn test_matter_params_deserialize() {
        let json = r#"{
            "device_id": 12345,
            "fabric_id": 1,
            "pin_code": 20202021,
            "discriminator": 3840,
            "ip_address": "192.168.1.100",
            "port": 5540,
            "subscribe_min_interval": 5,
            "subscribe_max_interval": 120
        }"#;

        let params: MatterParamsConfig = serde_json::from_str(json).unwrap();
        assert_eq!(params.device_id, 12345);
        assert_eq!(params.fabric_id, Some(1));
        assert_eq!(params.pin_code, Some(20202021));
        assert_eq!(params.discriminator, Some(3840));
        assert_eq!(params.ip_address, Some("192.168.1.100".to_string()));
        assert_eq!(params.port, 5540);
        assert_eq!(params.subscribe_min_interval, 5);
        assert_eq!(params.subscribe_max_interval, 120);
    }

    #[test]
    fn test_matter_params_deserialize_minimal() {
        let json = r#"{"device_id": 42}"#;

        let params: MatterParamsConfig = serde_json::from_str(json).unwrap();
        assert_eq!(params.device_id, 42);
        assert_eq!(params.port, 5540);
        assert!(params.ip_address.is_none());
    }

    #[test]
    fn test_matter_params_to_config() {
        let params = MatterParamsConfig {
            device_id: 100,
            connect_timeout_ms: 5000,
            reconnect_interval_ms: 3000,
            ..Default::default()
        };

        let config = params.to_config();
        assert_eq!(config.device_id, 100);
        assert_eq!(config.connect_timeout, Duration::from_millis(5000));
        assert_eq!(config.reconnect_interval, Duration::from_millis(3000));
        assert_eq!(config.port, 5540);
    }

    #[test]
    fn test_matter_config_custom_timeouts() {
        let json = r#"{
            "device_id": 999,
            "connect_timeout_ms": 30000,
            "reconnect_interval_ms": 15000
        }"#;

        let params: MatterParamsConfig = serde_json::from_str(json).unwrap();
        assert_eq!(params.connect_timeout_ms, 30000);
        assert_eq!(params.reconnect_interval_ms, 15000);

        let config = params.to_config();
        assert_eq!(config.connect_timeout, Duration::from_millis(30000));
        assert_eq!(config.reconnect_interval, Duration::from_millis(15000));

        // Verify other fields got defaults
        assert_eq!(config.port, 5540);
        assert_eq!(config.subscribe_min_interval, 1);
        assert_eq!(config.subscribe_max_interval, 60);
        assert!(config.ip_address.is_none());
    }

    #[test]
    fn test_matter_config_with_pairing() {
        let json = r#"{
            "device_id": 12345,
            "pin_code": 20202021,
            "discriminator": 3840,
            "ip_address": "192.168.1.200",
            "port": 5541
        }"#;

        let params: MatterParamsConfig = serde_json::from_str(json).unwrap();
        assert_eq!(params.pin_code, Some(20202021));
        assert_eq!(params.discriminator, Some(3840));

        let config = params.to_config();
        assert_eq!(config.device_id, 12345);
        assert_eq!(config.pin_code, Some(20202021));
        assert_eq!(config.discriminator, Some(3840));
        assert_eq!(config.ip_address, Some("192.168.1.200".to_string()));
        assert_eq!(config.port, 5541);

        // Verify fabric_id is None by default
        assert!(config.fabric_id.is_none());
    }
}
