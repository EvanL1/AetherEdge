//! Zigbee protocol adapter configuration.

use aether_config::io::MAX_CHANNEL_TIMING_MS;
use serde::Deserialize;
use std::time::Duration;

use crate::protocols::core::error::{GatewayError, Result};

/// Zigbee channel parameters (from database config JSON).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ZigbeeParamsConfig {
    /// TCP gateway host address
    host: String,

    /// TCP gateway port
    #[serde(default = "default_port")]
    port: u16,

    /// Connection timeout in milliseconds
    #[serde(default = "default_connect_timeout_ms")]
    connect_timeout_ms: u64,
}

fn default_port() -> u16 {
    8888
}

fn default_connect_timeout_ms() -> u64 {
    5000
}

impl Default for ZigbeeParamsConfig {
    fn default() -> Self {
        Self {
            host: "127.0.0.1".to_string(),
            port: default_port(),
            connect_timeout_ms: default_connect_timeout_ms(),
        }
    }
}

/// Zigbee runtime configuration.
#[derive(Debug, Clone)]
pub(crate) struct ZigbeeConfig {
    pub(crate) host: String,
    pub(crate) port: u16,
    pub(crate) connect_timeout: Duration,
}

impl ZigbeeParamsConfig {
    /// Validate persisted parameters and convert the implemented Raw gateway mode.
    pub(crate) fn into_config(self) -> Result<ZigbeeConfig> {
        let host = self.host.trim();
        if host.is_empty() {
            return Err(GatewayError::Config(
                "Zigbee host must not be blank".to_string(),
            ));
        }
        if self.port == 0 {
            return Err(GatewayError::Config(
                "Zigbee port must be greater than zero".to_string(),
            ));
        }
        if !(1..=MAX_CHANNEL_TIMING_MS).contains(&self.connect_timeout_ms) {
            return Err(GatewayError::Config(format!(
                "Zigbee connect_timeout_ms must be between 1 and {MAX_CHANNEL_TIMING_MS} milliseconds"
            )));
        }

        let host = if host.len() == self.host.len() {
            self.host
        } else {
            host.to_owned()
        };
        Ok(ZigbeeConfig {
            host,
            port: self.port,
            connect_timeout: Duration::from_millis(self.connect_timeout_ms),
        })
    }
}

#[cfg(test)]
#[allow(clippy::disallowed_methods)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let params = ZigbeeParamsConfig::default();
        assert_eq!(params.host, "127.0.0.1");
        assert_eq!(params.port, 8888);
        assert_eq!(params.connect_timeout_ms, 5000);
    }

    #[test]
    fn test_params_deserialize_minimal() {
        let json = r#"{"host": "192.168.1.100"}"#;
        let params: ZigbeeParamsConfig = serde_json::from_str(json).unwrap();
        assert_eq!(params.host, "192.168.1.100");
        assert_eq!(params.port, 8888); // default
    }

    #[test]
    fn test_params_deserialize_full() {
        let json = r#"{
            "host": "10.0.0.1",
            "port": 9999,
            "connect_timeout_ms": 3000
        }"#;

        let params: ZigbeeParamsConfig = serde_json::from_str(json).unwrap();
        assert_eq!(params.host, "10.0.0.1");
        assert_eq!(params.port, 9999);
        assert_eq!(params.connect_timeout_ms, 3000);
    }

    #[test]
    fn test_try_to_config() {
        let params = ZigbeeParamsConfig {
            host: "10.0.0.1".to_string(),
            port: 9999,
            connect_timeout_ms: 3000,
        };

        let config = params.into_config().unwrap();
        assert_eq!(config.host, "10.0.0.1");
        assert_eq!(config.port, 9999);
        assert_eq!(config.connect_timeout, Duration::from_millis(3000));
    }

    #[test]
    fn invalid_parameters_fail_closed() {
        for params in [
            ZigbeeParamsConfig {
                host: " ".to_string(),
                ..Default::default()
            },
            ZigbeeParamsConfig {
                port: 0,
                ..Default::default()
            },
            ZigbeeParamsConfig {
                connect_timeout_ms: 0,
                ..Default::default()
            },
            ZigbeeParamsConfig {
                connect_timeout_ms: MAX_CHANNEL_TIMING_MS + 1,
                ..Default::default()
            },
        ] {
            assert!(params.into_config().is_err());
        }
    }

    #[test]
    fn unused_parameters_are_rejected() {
        for field in [
            r#""gateway_type":"raw""#,
            r#""gateway_type":"znp""#,
            r#""gateway_type":"ezsp""#,
            r#""pan_id":4660"#,
            r#""channel":15"#,
            r#""permit_join_on_start":true"#,
            r#""reconnect_interval_ms":3000"#,
        ] {
            let json = format!(r#"{{"host":"127.0.0.1",{field}}}"#);
            assert!(serde_json::from_str::<ZigbeeParamsConfig>(&json).is_err());
        }
    }
}
