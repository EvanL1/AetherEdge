//! HTTP/OpenAPI DTOs for the uplink service.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::config_model::UplinkConfig;

#[derive(Debug, Deserialize, ToSchema)]
pub struct NetConfig {
    pub product_sn: String,
    pub device_sn: String,
    pub broker_host: String,
    pub broker_port: u16,
    pub broker_keepalive_secs: u64,
    pub client_id: String,
    #[serde(default)]
    pub username: Option<String>,
    #[schema(write_only)]
    #[serde(default)]
    pub password: Option<String>,
    pub ssl_enabled: bool,
    pub reconnect_delay_secs: u64,
    pub reconnect_max_attempts: u32,
    pub report_interval_secs: u64,
    pub report_batch_size: usize,
    pub system_monitor_enabled: bool,
    pub system_monitor_interval_secs: u64,
    #[serde(default)]
    pub telemetry_enabled: bool,
    #[serde(default = "default_telemetry_interval_secs")]
    pub telemetry_interval_secs: u64,
    pub subscribe_patterns: Vec<String>,
    pub exclude_patterns: Vec<String>,
    pub alarm_url: String,
    pub automation_url: String,
}

impl NetConfig {
    pub fn into_runtime(self, current: &UplinkConfig) -> UplinkConfig {
        let mut config = UplinkConfig {
            product_sn: self.product_sn,
            device_sn: self.device_sn,
            broker_host: self.broker_host,
            broker_port: self.broker_port,
            broker_keepalive_secs: self.broker_keepalive_secs,
            client_id: self.client_id,
            username: self.username,
            password: self.password.or_else(|| current.password.clone()),
            ssl_enabled: self.ssl_enabled,
            reconnect_delay_secs: self.reconnect_delay_secs,
            reconnect_max_attempts: self.reconnect_max_attempts,
            report_interval_secs: self.report_interval_secs,
            report_batch_size: self.report_batch_size,
            system_monitor_enabled: self.system_monitor_enabled,
            system_monitor_interval_secs: self.system_monitor_interval_secs,
            telemetry_enabled: self.telemetry_enabled,
            telemetry_interval_secs: self.telemetry_interval_secs,
            subscribe_patterns: self.subscribe_patterns,
            exclude_patterns: self.exclude_patterns,
            alarm_url: self.alarm_url,
            automation_url: self.automation_url,
        };
        config.normalize();
        config
    }
}

#[derive(Debug, Serialize, ToSchema)]
pub struct NetConfigView {
    pub product_sn: String,
    pub device_sn: String,
    pub broker_host: String,
    pub broker_port: u16,
    pub broker_keepalive_secs: u64,
    pub client_id: String,
    pub username: Option<String>,
    pub ssl_enabled: bool,
    pub reconnect_delay_secs: u64,
    pub reconnect_max_attempts: u32,
    pub report_interval_secs: u64,
    pub report_batch_size: usize,
    pub system_monitor_enabled: bool,
    pub system_monitor_interval_secs: u64,
    pub telemetry_enabled: bool,
    pub telemetry_interval_secs: u64,
    pub subscribe_patterns: Vec<String>,
    pub exclude_patterns: Vec<String>,
    pub alarm_url: String,
    pub automation_url: String,
}

impl From<&UplinkConfig> for NetConfigView {
    fn from(value: &UplinkConfig) -> Self {
        Self {
            product_sn: value.product_sn.clone(),
            device_sn: value.device_sn.clone(),
            broker_host: value.broker_host.clone(),
            broker_port: value.broker_port,
            broker_keepalive_secs: value.broker_keepalive_secs,
            client_id: value.client_id.clone(),
            username: value.username.clone(),
            ssl_enabled: value.ssl_enabled,
            reconnect_delay_secs: value.reconnect_delay_secs,
            reconnect_max_attempts: value.reconnect_max_attempts,
            report_interval_secs: value.report_interval_secs,
            report_batch_size: value.report_batch_size,
            system_monitor_enabled: value.system_monitor_enabled,
            system_monitor_interval_secs: value.system_monitor_interval_secs,
            telemetry_enabled: value.telemetry_enabled,
            telemetry_interval_secs: value.telemetry_interval_secs,
            subscribe_patterns: value.subscribe_patterns.clone(),
            exclude_patterns: value.exclude_patterns.clone(),
            alarm_url: value.alarm_url.clone(),
            automation_url: value.automation_url.clone(),
        }
    }
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct AlarmBroadcastRequest(pub serde_json::Value);

#[allow(dead_code)]
#[derive(ToSchema)]
pub struct CertUploadForm {
    #[schema(example = "ca_cert")]
    pub cert_type: String,
    #[schema(format = Binary, value_type = String)]
    pub file: String,
}

#[allow(dead_code)]
#[derive(Debug, ToSchema)]
pub struct UplinkDataResponse<T> {
    pub success: bool,
    pub message: String,
    pub data: T,
}

#[allow(dead_code)]
#[derive(Debug, ToSchema)]
pub struct AlarmQueuedResponse {
    pub success: bool,
    pub message: String,
    pub outbox_id: u64,
}

const fn default_telemetry_interval_secs() -> u64 {
    30
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(password: Option<String>) -> NetConfig {
        let default = UplinkConfig::default();
        NetConfig {
            product_sn: default.product_sn,
            device_sn: default.device_sn,
            broker_host: default.broker_host,
            broker_port: default.broker_port,
            broker_keepalive_secs: default.broker_keepalive_secs,
            client_id: default.client_id,
            username: default.username,
            password,
            ssl_enabled: default.ssl_enabled,
            reconnect_delay_secs: default.reconnect_delay_secs,
            reconnect_max_attempts: default.reconnect_max_attempts,
            report_interval_secs: default.report_interval_secs,
            report_batch_size: default.report_batch_size,
            system_monitor_enabled: default.system_monitor_enabled,
            system_monitor_interval_secs: default.system_monitor_interval_secs,
            telemetry_enabled: default.telemetry_enabled,
            telemetry_interval_secs: default.telemetry_interval_secs,
            subscribe_patterns: default.subscribe_patterns,
            exclude_patterns: default.exclude_patterns,
            alarm_url: default.alarm_url,
            automation_url: default.automation_url,
        }
    }

    #[test]
    fn response_shape_has_no_password_field() {
        let config = UplinkConfig {
            password: Some("private-broker-secret".to_string()),
            ..UplinkConfig::default()
        };
        let value = serde_json::to_value(NetConfigView::from(&config)).expect("serialize view");
        assert!(value.get("password").is_none());
        assert!(!format!("{config:?}").contains("private-broker-secret"));
    }

    #[test]
    fn omitted_password_is_preserved_and_empty_password_clears_it() {
        let current = UplinkConfig {
            password: Some("private-broker-secret".to_string()),
            ..UplinkConfig::default()
        };
        assert_eq!(
            request(None).into_runtime(&current).password,
            current.password
        );
        assert_eq!(
            request(Some(String::new()))
                .into_runtime(&current)
                .password
                .as_deref(),
            Some("")
        );
    }
}
