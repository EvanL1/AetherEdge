//! Zigbee protocol adapter configuration.

use serde::{Deserialize, Serialize};
use std::time::Duration;

/// Zigbee gateway type (determines frame encoding).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GatewayType {
    /// Raw ZCL frames (simplest, for gateways that directly forward ZCL)
    Raw,
    /// TI Z-Stack ZNP protocol (CC2652 etc.)
    Znp,
    /// Silicon Labs EZSP protocol (EFR32 etc.)
    Ezsp,
}

impl Default for GatewayType {
    fn default() -> Self {
        Self::Raw
    }
}

/// Zigbee channel parameters (from database config JSON).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ZigbeeParamsConfig {
    /// TCP gateway host address
    pub host: String,

    /// TCP gateway port
    #[serde(default = "default_port")]
    pub port: u16,

    /// Gateway type (determines frame encoding)
    #[serde(default)]
    pub gateway_type: GatewayType,

    /// Zigbee PAN ID (optional)
    #[serde(default)]
    pub pan_id: Option<u16>,

    /// Zigbee channel number (11-26)
    #[serde(default)]
    pub channel: Option<u8>,

    /// Open network for joining on startup
    #[serde(default)]
    pub permit_join_on_start: bool,

    /// Connection timeout in milliseconds
    #[serde(default = "default_connect_timeout_ms")]
    pub connect_timeout_ms: u64,

    /// Reconnect interval in milliseconds
    #[serde(default = "default_reconnect_interval_ms")]
    pub reconnect_interval_ms: u64,
}

fn default_port() -> u16 {
    8888
}

fn default_connect_timeout_ms() -> u64 {
    5000
}

fn default_reconnect_interval_ms() -> u64 {
    5000
}

impl Default for ZigbeeParamsConfig {
    fn default() -> Self {
        Self {
            host: "127.0.0.1".to_string(),
            port: default_port(),
            gateway_type: GatewayType::default(),
            pan_id: None,
            channel: None,
            permit_join_on_start: false,
            connect_timeout_ms: default_connect_timeout_ms(),
            reconnect_interval_ms: default_reconnect_interval_ms(),
        }
    }
}

/// Zigbee runtime configuration.
#[derive(Debug, Clone)]
pub struct ZigbeeConfig {
    pub host: String,
    pub port: u16,
    pub gateway_type: GatewayType,
    pub pan_id: Option<u16>,
    pub channel: Option<u8>,
    pub permit_join_on_start: bool,
    pub connect_timeout: Duration,
    pub reconnect_interval: Duration,
}

impl ZigbeeParamsConfig {
    pub fn to_config(&self) -> ZigbeeConfig {
        ZigbeeConfig {
            host: self.host.clone(),
            port: self.port,
            gateway_type: self.gateway_type,
            pan_id: self.pan_id,
            channel: self.channel,
            permit_join_on_start: self.permit_join_on_start,
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
    fn test_default_config() {
        let params = ZigbeeParamsConfig::default();
        assert_eq!(params.host, "127.0.0.1");
        assert_eq!(params.port, 8888);
        assert_eq!(params.gateway_type, GatewayType::Raw);
        assert!(params.pan_id.is_none());
        assert!(params.channel.is_none());
        assert!(!params.permit_join_on_start);
        assert_eq!(params.connect_timeout_ms, 5000);
        assert_eq!(params.reconnect_interval_ms, 5000);
    }

    #[test]
    fn test_params_deserialize_minimal() {
        let json = r#"{"host": "192.168.1.100"}"#;
        let params: ZigbeeParamsConfig = serde_json::from_str(json).unwrap();
        assert_eq!(params.host, "192.168.1.100");
        assert_eq!(params.port, 8888); // default
        assert_eq!(params.gateway_type, GatewayType::Raw); // default
    }

    #[test]
    fn test_params_deserialize_full() {
        let json = r#"{
            "host": "10.0.0.1",
            "port": 9999,
            "gateway_type": "znp",
            "pan_id": 4660,
            "channel": 15,
            "permit_join_on_start": true,
            "connect_timeout_ms": 3000,
            "reconnect_interval_ms": 10000
        }"#;

        let params: ZigbeeParamsConfig = serde_json::from_str(json).unwrap();
        assert_eq!(params.host, "10.0.0.1");
        assert_eq!(params.port, 9999);
        assert_eq!(params.gateway_type, GatewayType::Znp);
        assert_eq!(params.pan_id, Some(4660));
        assert_eq!(params.channel, Some(15));
        assert!(params.permit_join_on_start);
        assert_eq!(params.connect_timeout_ms, 3000);
        assert_eq!(params.reconnect_interval_ms, 10000);
    }

    #[test]
    fn test_to_config() {
        let params = ZigbeeParamsConfig {
            host: "10.0.0.1".to_string(),
            port: 9999,
            gateway_type: GatewayType::Ezsp,
            pan_id: Some(0x1234),
            channel: Some(20),
            permit_join_on_start: true,
            connect_timeout_ms: 3000,
            reconnect_interval_ms: 10000,
        };

        let config = params.to_config();
        assert_eq!(config.host, "10.0.0.1");
        assert_eq!(config.port, 9999);
        assert_eq!(config.gateway_type, GatewayType::Ezsp);
        assert_eq!(config.pan_id, Some(0x1234));
        assert_eq!(config.channel, Some(20));
        assert!(config.permit_join_on_start);
        assert_eq!(config.connect_timeout, Duration::from_millis(3000));
        assert_eq!(config.reconnect_interval, Duration::from_millis(10000));
    }

    #[test]
    fn test_gateway_type_serde() {
        let types = vec![
            ("\"raw\"", GatewayType::Raw),
            ("\"znp\"", GatewayType::Znp),
            ("\"ezsp\"", GatewayType::Ezsp),
        ];

        for (json, expected) in types {
            let result: GatewayType = serde_json::from_str(json).unwrap();
            assert_eq!(result, expected, "Failed for {}", json);
        }
    }
}
