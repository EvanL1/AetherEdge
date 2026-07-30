//! Runtime and persistence-owned uplink configuration.

#[derive(Clone)]
pub struct UplinkConfig {
    pub product_sn: String,
    pub device_sn: String,
    pub broker_host: String,
    pub broker_port: u16,
    pub broker_keepalive_secs: u64,
    pub client_id: String,
    pub username: Option<String>,
    pub password: Option<String>,
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

impl std::fmt::Debug for UplinkConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("UplinkConfig")
            .field("product_sn", &self.product_sn)
            .field("device_sn", &self.device_sn)
            .field("broker_host", &self.broker_host)
            .field("broker_port", &self.broker_port)
            .field("broker_keepalive_secs", &self.broker_keepalive_secs)
            .field("client_id", &self.client_id)
            .field("username", &self.username)
            .field("password", &self.password.as_ref().map(|_| "[REDACTED]"))
            .field("ssl_enabled", &self.ssl_enabled)
            .field("reconnect_delay_secs", &self.reconnect_delay_secs)
            .field("reconnect_max_attempts", &self.reconnect_max_attempts)
            .field("report_interval_secs", &self.report_interval_secs)
            .field("report_batch_size", &self.report_batch_size)
            .field("system_monitor_enabled", &self.system_monitor_enabled)
            .field(
                "system_monitor_interval_secs",
                &self.system_monitor_interval_secs,
            )
            .field("telemetry_enabled", &self.telemetry_enabled)
            .field("telemetry_interval_secs", &self.telemetry_interval_secs)
            .field("subscribe_patterns", &self.subscribe_patterns)
            .field("exclude_patterns", &self.exclude_patterns)
            .field("alarm_url", &self.alarm_url)
            .field("automation_url", &self.automation_url)
            .finish()
    }
}

impl Default for UplinkConfig {
    fn default() -> Self {
        Self {
            product_sn: "AetherHub".to_string(),
            device_sn: "auto".to_string(),
            broker_host: "localhost".to_string(),
            broker_port: 8883,
            broker_keepalive_secs: 120,
            client_id: "auto".to_string(),
            username: None,
            password: None,
            ssl_enabled: false,
            reconnect_delay_secs: 10,
            reconnect_max_attempts: 50,
            report_interval_secs: 50,
            report_batch_size: 50,
            system_monitor_enabled: true,
            system_monitor_interval_secs: 10,
            telemetry_enabled: false,
            telemetry_interval_secs: 30,
            subscribe_patterns: vec!["inst:*:M".to_string(), "inst:*:A".to_string()],
            exclude_patterns: Vec::new(),
            alarm_url: "http://localhost:6007".to_string(),
            automation_url: "http://localhost:6002".to_string(),
        }
    }
}

impl UplinkConfig {
    pub fn normalize(&mut self) {
        self.broker_port = self.broker_port.max(1);
        self.broker_keepalive_secs = self.broker_keepalive_secs.max(1);
        self.reconnect_delay_secs = self.reconnect_delay_secs.max(1);
        self.reconnect_max_attempts = self.reconnect_max_attempts.max(1);
        self.report_interval_secs = self.report_interval_secs.max(1);
        self.report_batch_size = self.report_batch_size.max(1);
        self.system_monitor_interval_secs = self.system_monitor_interval_secs.max(1);
        self.telemetry_interval_secs = self.telemetry_interval_secs.max(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_clamps_zero_runtime_values() {
        let mut config = UplinkConfig {
            broker_port: 0,
            broker_keepalive_secs: 0,
            reconnect_delay_secs: 0,
            reconnect_max_attempts: 0,
            report_interval_secs: 0,
            report_batch_size: 0,
            system_monitor_interval_secs: 0,
            telemetry_interval_secs: 0,
            ..UplinkConfig::default()
        };
        config.normalize();
        assert_eq!(config.broker_port, 1);
        assert_eq!(config.broker_keepalive_secs, 1);
        assert_eq!(config.reconnect_delay_secs, 1);
        assert_eq!(config.reconnect_max_attempts, 1);
        assert_eq!(config.report_interval_secs, 1);
        assert_eq!(config.report_batch_size, 1);
        assert_eq!(config.system_monitor_interval_secs, 1);
        assert_eq!(config.telemetry_interval_secs, 1);
    }
}
