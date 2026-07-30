//! BLE protocol adapter configuration.

use aether_config::io::MAX_CHANNEL_TIMING_MS;
use btleplug::api::BDAddr;
use serde::Deserialize;
use std::time::Duration;

use crate::protocols::core::error::{GatewayError, Result};

/// BLE channel parameters (from database config JSON).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct BleParamsConfig {
    /// Target device MAC address (e.g., "AA:BB:CC:DD:EE:FF")
    device_address: String,

    /// Bluetooth adapter name (None = auto-detect first available)
    #[serde(default)]
    adapter_name: Option<String>,

    /// Scan timeout in milliseconds
    #[serde(default = "default_scan_timeout_ms")]
    scan_timeout_ms: u64,

    /// Connection timeout in milliseconds
    #[serde(default = "default_connect_timeout_ms")]
    connect_timeout_ms: u64,
}

fn default_scan_timeout_ms() -> u64 {
    10000
}

fn default_connect_timeout_ms() -> u64 {
    5000
}

impl Default for BleParamsConfig {
    fn default() -> Self {
        Self {
            device_address: String::new(),
            adapter_name: None,
            scan_timeout_ms: default_scan_timeout_ms(),
            connect_timeout_ms: default_connect_timeout_ms(),
        }
    }
}

/// BLE runtime configuration.
#[derive(Debug, Clone)]
pub(crate) struct BleConfig {
    pub(crate) device_address: BDAddr,
    pub(crate) adapter_name: Option<String>,
    pub(crate) scan_timeout: Duration,
    pub(crate) connect_timeout: Duration,
}

impl BleParamsConfig {
    /// Validate persisted parameters and convert them to runtime configuration.
    pub(crate) fn into_config(self) -> Result<BleConfig> {
        validate_timeout("scan_timeout_ms", self.scan_timeout_ms)?;
        validate_timeout("connect_timeout_ms", self.connect_timeout_ms)?;
        let device_address = self.device_address.parse::<BDAddr>().map_err(|error| {
            GatewayError::Config(format!(
                "invalid BLE device_address '{}': {error}",
                self.device_address
            ))
        })?;
        let adapter_name = self
            .adapter_name
            .map(|name| {
                let trimmed = name.trim();
                if trimmed.is_empty() {
                    Err(GatewayError::Config(
                        "BLE adapter_name must not be blank".to_string(),
                    ))
                } else if trimmed.len() == name.len() {
                    Ok(name)
                } else {
                    Ok(trimmed.to_owned())
                }
            })
            .transpose()?;

        Ok(BleConfig {
            device_address,
            adapter_name,
            scan_timeout: Duration::from_millis(self.scan_timeout_ms),
            connect_timeout: Duration::from_millis(self.connect_timeout_ms),
        })
    }
}

fn validate_timeout(name: &str, value: u64) -> Result<()> {
    if !(1..=MAX_CHANNEL_TIMING_MS).contains(&value) {
        return Err(GatewayError::Config(format!(
            "BLE {name} must be between 1 and {MAX_CHANNEL_TIMING_MS} milliseconds"
        )));
    }
    Ok(())
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
            "connect_timeout_ms": 8000
        }"#;
        let params: BleParamsConfig = serde_json::from_str(json).unwrap();
        assert_eq!(params.device_address, "AA:BB:CC:DD:EE:FF");
        assert_eq!(params.adapter_name, Some("hci0".to_string()));
        assert_eq!(params.scan_timeout_ms, 15000);
        assert_eq!(params.connect_timeout_ms, 8000);
    }

    #[test]
    fn test_try_to_config() {
        let params = BleParamsConfig {
            device_address: "AA:BB:CC:DD:EE:FF".to_string(),
            adapter_name: Some("hci0".to_string()),
            scan_timeout_ms: 15000,
            connect_timeout_ms: 8000,
        };
        let config = params.into_config().unwrap();
        assert_eq!(config.device_address.to_string(), "AA:BB:CC:DD:EE:FF");
        assert_eq!(config.adapter_name, Some("hci0".to_string()));
        assert_eq!(config.scan_timeout, Duration::from_millis(15000));
        assert_eq!(config.connect_timeout, Duration::from_millis(8000));
    }

    #[test]
    fn invalid_parameters_fail_closed() {
        for json in [
            r#"{"device_address":"not-a-mac"}"#,
            r#"{"device_address":"AA:BB:CC:DD:EE:FF","adapter_name":" "}"#,
            r#"{"device_address":"AA:BB:CC:DD:EE:FF","scan_timeout_ms":0}"#,
            r#"{"device_address":"AA:BB:CC:DD:EE:FF","connect_timeout_ms":86400001}"#,
        ] {
            let params: BleParamsConfig = serde_json::from_str(json).unwrap();
            assert!(params.into_config().is_err(), "{json}");
        }
    }

    #[test]
    fn unused_parameters_are_rejected() {
        for field in [
            r#""mtu":256"#,
            r#""reconnect_interval_ms":3000"#,
            r#""unknown":true"#,
        ] {
            let json = format!(r#"{{"device_address":"AA:BB:CC:DD:EE:FF",{field}}}"#);
            assert!(serde_json::from_str::<BleParamsConfig>(&json).is_err());
        }
    }
}
