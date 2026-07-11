//! BLE protocol adapter configuration.

use serde::{Deserialize, Serialize};
use std::time::Duration;

/// BLE channel parameters (from database config JSON).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BleParamsConfig {
    /// Target device MAC address (e.g., "AA:BB:CC:DD:EE:FF")
    pub device_address: String,

    /// Bluetooth adapter name (None = auto-detect first available)
    #[serde(default)]
    pub adapter_name: Option<String>,

    /// Scan timeout in milliseconds
    #[serde(default = "default_scan_timeout_ms")]
    pub scan_timeout_ms: u64,

    /// Connection timeout in milliseconds
    #[serde(default = "default_connect_timeout_ms")]
    pub connect_timeout_ms: u64,

    /// Reconnect interval in milliseconds
    #[serde(default = "default_reconnect_interval_ms")]
    pub reconnect_interval_ms: u64,

    /// MTU size for BLE communication (None = use default)
    #[serde(default)]
    pub mtu: Option<u16>,
}

fn default_scan_timeout_ms() -> u64 {
    10000
}

fn default_connect_timeout_ms() -> u64 {
    5000
}

fn default_reconnect_interval_ms() -> u64 {
    5000
}

impl Default for BleParamsConfig {
    fn default() -> Self {
        Self {
            device_address: String::new(),
            adapter_name: None,
            scan_timeout_ms: default_scan_timeout_ms(),
            connect_timeout_ms: default_connect_timeout_ms(),
            reconnect_interval_ms: default_reconnect_interval_ms(),
            mtu: None,
        }
    }
}

/// BLE runtime configuration.
#[derive(Debug, Clone)]
pub struct BleConfig {
    pub device_address: String,
    pub adapter_name: Option<String>,
    pub scan_timeout: Duration,
    pub connect_timeout: Duration,
    pub reconnect_interval: Duration,
    pub mtu: Option<u16>,
}

impl BleParamsConfig {
    /// Convert to runtime configuration.
    pub fn to_config(&self) -> BleConfig {
        BleConfig {
            device_address: self.device_address.clone(),
            adapter_name: self.adapter_name.clone(),
            scan_timeout: Duration::from_millis(self.scan_timeout_ms),
            connect_timeout: Duration::from_millis(self.connect_timeout_ms),
            reconnect_interval: Duration::from_millis(self.reconnect_interval_ms),
            mtu: self.mtu,
        }
    }
}

#[cfg(test)]
#[allow(clippy::disallowed_methods)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let params = BleParamsConfig::default();
        assert_eq!(params.device_address, "");
        assert!(params.adapter_name.is_none());
        assert_eq!(params.scan_timeout_ms, 10000);
        assert_eq!(params.connect_timeout_ms, 5000);
        assert_eq!(params.reconnect_interval_ms, 5000);
        assert!(params.mtu.is_none());
    }

    #[test]
    fn test_deserialize_minimal() {
        let json = r#"{"device_address": "AA:BB:CC:DD:EE:FF"}"#;
        let params: BleParamsConfig = serde_json::from_str(json).unwrap();
        assert_eq!(params.device_address, "AA:BB:CC:DD:EE:FF");
        assert_eq!(params.scan_timeout_ms, 10000);
        assert_eq!(params.connect_timeout_ms, 5000);
    }

    #[test]
    fn test_deserialize_full() {
        let json = r#"{
            "device_address": "AA:BB:CC:DD:EE:FF",
            "adapter_name": "hci0",
            "scan_timeout_ms": 15000,
            "connect_timeout_ms": 8000,
            "reconnect_interval_ms": 3000,
            "mtu": 512
        }"#;
        let params: BleParamsConfig = serde_json::from_str(json).unwrap();
        assert_eq!(params.device_address, "AA:BB:CC:DD:EE:FF");
        assert_eq!(params.adapter_name, Some("hci0".to_string()));
        assert_eq!(params.scan_timeout_ms, 15000);
        assert_eq!(params.connect_timeout_ms, 8000);
        assert_eq!(params.reconnect_interval_ms, 3000);
        assert_eq!(params.mtu, Some(512));
    }

    #[test]
    fn test_to_config() {
        let params = BleParamsConfig {
            device_address: "AA:BB:CC:DD:EE:FF".to_string(),
            adapter_name: Some("hci0".to_string()),
            scan_timeout_ms: 15000,
            connect_timeout_ms: 8000,
            reconnect_interval_ms: 3000,
            mtu: Some(256),
        };
        let config = params.to_config();
        assert_eq!(config.device_address, "AA:BB:CC:DD:EE:FF");
        assert_eq!(config.adapter_name, Some("hci0".to_string()));
        assert_eq!(config.scan_timeout, Duration::from_millis(15000));
        assert_eq!(config.connect_timeout, Duration::from_millis(8000));
        assert_eq!(config.reconnect_interval, Duration::from_millis(3000));
        assert_eq!(config.mtu, Some(256));
    }
}
