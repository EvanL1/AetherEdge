use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use utoipa::ToSchema;

// ── MQTT publish payloads ─────────────────────────────────────────────────────

/// Uploaded to `property/{productSN}/{deviceSN}`.
#[derive(Serialize)]
pub struct PropertyPayload {
    pub timestamp: i64,
    pub property: Vec<PropertyEntry>,
}

#[derive(Clone, Serialize)]
pub struct PropertyEntry {
    pub source: String,
    pub device: String,
    pub data_type: String,
    /// Point-id → current SHM value mapping.
    pub value: HashMap<String, serde_json::Value>,
}

/// Uploaded to `status/{productSN}/{deviceSN}`.
#[derive(Serialize)]
pub struct StatusPayload {
    #[serde(rename = "type")]
    pub msg_type: String,
    pub gateway: String,
    pub timestamp: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

// ── MQTT command payloads (incoming) ─────────────────────────────────────────

/// Incoming single-point read request on `read/{productSN}/{deviceSN}`.
/// Field name in JSON is `key` (matching Python uplink protocol); `msgId` is the
/// correlation ID echoed back in the reply.
#[derive(Debug, Deserialize)]
pub struct ReadRequest {
    pub source: String,
    pub device: String,
    pub data_type: String,
    /// If absent, return every configured point in the logical group.
    #[serde(rename = "key")]
    pub field: Option<String>,
    #[serde(rename = "msgId")]
    pub msg_id: Option<String>,
}

/// Single entry inside a `read-reply` property array.
#[derive(Serialize)]
pub struct ReadReplyProperty {
    pub source: String,
    pub device: String,
    pub data_type: String,
    /// For a keyed read: `{ key: value }`. For a full-group read: all point/value pairs.
    pub value: serde_json::Value,
}

/// Reply to `read-reply/{productSN}/{deviceSN}`.
/// Format matches Python uplink: `{ timestamp, property: [...], msgId }`.
#[derive(Serialize)]
pub struct ReadReply {
    pub timestamp: i64,
    pub property: Vec<ReadReplyProperty>,
    #[serde(rename = "msgId")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub msg_id: Option<String>,
}

/// Incoming single-point write request on `write/{productSN}/{deviceSN}`.
/// Field name in JSON is `key`; `msgId` is the correlation ID.
#[derive(Debug, Deserialize)]
pub struct WriteRequest {
    pub source: String,
    pub device: String,
    pub data_type: String,
    #[serde(rename = "key")]
    pub field: String,
    pub value: serde_json::Value,
    #[serde(rename = "msgId")]
    pub msg_id: Option<String>,
}

/// Reply to `write-reply/{productSN}/{deviceSN}`.
/// Format matches Python uplink: `{ result: "success"|"fail", msgId }`.
#[derive(Serialize)]
pub struct WriteReply {
    pub result: String,
    #[serde(rename = "msgId")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub msg_id: Option<String>,
}

// ── inst-sync ─────────────────────────────────────────────────────────────────

/// One device entry in an `inst-sync-reply` message.
#[derive(Serialize)]
pub struct InstSyncItem {
    pub instance_id: i64,
    pub instance_name: String,
    pub product_name: String,
}

/// Reply payload for `inst-sync-reply/{productSN}/{deviceSN}`.
#[derive(Serialize)]
pub struct InstSyncReply {
    #[serde(rename = "msgId")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub msg_id: Option<String>,
    pub timestamp: i64,
    pub list: Vec<InstSyncItem>,
}

/// Generic command-acknowledgement reply (call-data-reply, call-alarm-reply).
/// Format matches Python uplink: `{ result, message, timestamp, msgId }`.
/// `call-alarm-reply` may use `result: "warning"` when alarm returns a non-2xx status.
#[derive(Serialize)]
pub struct CommandReply {
    pub result: String,
    pub message: String,
    pub timestamp: i64,
    #[serde(rename = "msgId")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub msg_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

// ── Dynamic service configuration ────────────────────────────────────────────

fn always_omit_mqtt_password(_: &Option<String>) -> bool {
    true
}

/// MQTT gateway service configuration (`POST /netApi/mqtt/config`).
///
/// Changes take effect immediately — uplink reconnects to the broker without
/// requiring a service restart.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[schema(example = json!({
    "product_sn": "AetherHub",
    "device_sn": "auto",
    "broker_host": "mqtt.example.com",
    "broker_port": 8883,
    "broker_keepalive_secs": 120,
    "client_id": "auto",
    "username": null,
    "password": null,
    "ssl_enabled": false,
    "reconnect_delay_secs": 10,
    "reconnect_max_attempts": 50,
    "report_interval_secs": 50,
    "report_batch_size": 50,
    "system_monitor_enabled": true,
    "system_monitor_interval_secs": 10,
    "subscribe_patterns": ["inst:*:M", "inst:*:A"],
    "exclude_patterns": [],
    "alarm_url": "http://localhost:6007",
    "automation_url": "http://localhost:6002"
}))]
pub struct NetConfig {
    // -- Device identity --
    /// Product serial number, used to construct MQTT topic prefixes such as
    /// `status/{product_sn}/{device_sn}`.
    #[schema(example = "AetherHub")]
    pub product_sn: String,

    /// Device serial number. Set to `"auto"` to read from hardware (tried in
    /// order: `/proc/device-tree/serial-number`, env var `DEVICE_SN`, hostname).
    #[schema(example = "auto")]
    pub device_sn: String,

    // -- MQTT broker --
    /// MQTT broker host address (IP or hostname).
    #[schema(example = "mqtt.example.com")]
    pub broker_host: String,

    /// MQTT broker port. Typically 8883 for TLS, 1883 for plain-text.
    #[schema(example = 8883, minimum = 1, maximum = 65535)]
    pub broker_port: u16,

    /// MQTT keep-alive interval in seconds. The connection is re-established if
    /// no heartbeat is received within this period.
    #[schema(example = 120, minimum = 5)]
    pub broker_keepalive_secs: u64,

    /// MQTT client ID. Set to `"auto"` to use the resolved `device_sn`.
    #[schema(example = "auto")]
    pub client_id: String,

    // -- Auth --
    /// MQTT username (optional). Omit or set to null to connect without credentials.
    #[schema(example = json!(null))]
    #[serde(default)]
    pub username: Option<String>,

    /// MQTT password (optional). Used together with `username`. Omit or send
    /// `null` on update to retain the stored secret; send an empty string to
    /// clear it. The value is never returned by read APIs.
    #[schema(write_only, example = json!(null))]
    #[serde(default, skip_serializing_if = "always_omit_mqtt_password")]
    pub password: Option<String>,

    // -- TLS --
    /// Enable TLS/SSL encrypted connection. The certificate directory is fixed
    /// at `/app/config/cert` inside the container (override with `CERT_DIR` env
    /// var); it is bind-mounted from the host at startup and cannot be changed
    /// via this API.
    #[schema(example = false)]
    pub ssl_enabled: bool,

    // -- Reconnect --
    /// Seconds to wait before attempting a reconnect after a disconnect.
    #[schema(example = 10, minimum = 1)]
    pub reconnect_delay_secs: u64,

    /// Maximum number of reconnect attempts before giving up. Reconnection
    /// resumes only after a manual trigger.
    #[schema(example = 50, minimum = 1)]
    pub reconnect_max_attempts: u32,

    // -- Data forwarding --
    /// Telemetry reporting interval in seconds. SHM data is batch-published to
    /// MQTT at this cadence.
    #[schema(example = 50, minimum = 1)]
    pub report_interval_secs: u64,

    /// Maximum number of data points per reporting batch. Excess points are
    /// deferred to the next interval.
    #[schema(example = 50, minimum = 1)]
    pub report_batch_size: usize,

    // -- System monitor --
    /// Enable periodic system resource monitoring (CPU, memory, disk, network)
    /// published via MQTT.
    #[schema(example = true)]
    pub system_monitor_enabled: bool,

    /// System resource sampling interval in seconds.
    #[schema(example = 10, minimum = 1)]
    pub system_monitor_interval_secs: u64,

    // -- Live-state source --
    /// Logical group subscription patterns using `*` and `?` glob wildcards.
    ///
    /// Only SQLite-discovered groups matching at least one pattern are sampled.
    /// Example: `inst:*:M` matches all instance measurement groups.
    #[schema(example = json!(["inst:*:M", "inst:*:A"]))]
    pub subscribe_patterns: Vec<String>,

    /// Exclusion patterns (**regular-expression syntax**, unlike the glob patterns above).
    ///
    /// Any key matching at least one pattern is excluded from reporting. Example:
    /// `["^inst:0:"]` excludes all keys for channel 0.
    #[schema(example = json!([]))]
    pub exclude_patterns: Vec<String>,

    // -- Service URLs --
    /// alarm base URL, used for reverse alarm-broadcast notifications.
    #[schema(example = "http://localhost:6007")]
    pub alarm_url: String,

    /// automation 服务地址，用于设备同步查询
    #[schema(example = "http://localhost:6002")]
    pub automation_url: String,
}

impl Default for NetConfig {
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
            subscribe_patterns: vec!["inst:*:M".to_string(), "inst:*:A".to_string()],
            exclude_patterns: vec![],
            alarm_url: "http://localhost:6007".to_string(),
            automation_url: "http://localhost:6002".to_string(),
        }
    }
}

impl NetConfig {
    pub fn normalize(&mut self) {
        self.broker_port = self.broker_port.max(1);
        self.broker_keepalive_secs = self.broker_keepalive_secs.max(1);
        self.reconnect_delay_secs = self.reconnect_delay_secs.max(1);
        self.reconnect_max_attempts = self.reconnect_max_attempts.max(1);
        self.report_interval_secs = self.report_interval_secs.max(1);
        self.report_batch_size = self.report_batch_size.max(1);
        self.system_monitor_interval_secs = self.system_monitor_interval_secs.max(1);
    }

    pub fn preserve_write_only_secrets_from(&mut self, current: &Self) {
        if self.password.is_none() {
            self.password.clone_from(&current.password);
        }
    }
}

#[cfg(test)]
mod config_tests {
    use super::*;

    #[test]
    fn normalize_clamps_zero_runtime_values() {
        let mut cfg = NetConfig {
            broker_port: 0,
            broker_keepalive_secs: 0,
            reconnect_delay_secs: 0,
            reconnect_max_attempts: 0,
            report_interval_secs: 0,
            report_batch_size: 0,
            system_monitor_interval_secs: 0,
            ..NetConfig::default()
        };

        cfg.normalize();

        assert_eq!(cfg.broker_port, 1);
        assert_eq!(cfg.broker_keepalive_secs, 1);
        assert_eq!(cfg.reconnect_delay_secs, 1);
        assert_eq!(cfg.reconnect_max_attempts, 1);
        assert_eq!(cfg.report_interval_secs, 1);
        assert_eq!(cfg.report_batch_size, 1);
        assert_eq!(cfg.system_monitor_interval_secs, 1);
    }

    #[test]
    fn mqtt_password_is_accepted_but_never_serialized() {
        let mut input =
            serde_json::to_value(NetConfig::default()).expect("serialize default config");
        input["password"] = serde_json::json!("private-broker-secret");
        let config: NetConfig = serde_json::from_value(input).expect("deserialize MQTT password");
        assert_eq!(config.password.as_deref(), Some("private-broker-secret"));

        let serialized = serde_json::to_value(config).expect("serialize redacted config");
        assert!(
            serialized.get("password").is_none(),
            "MQTT password must never appear in API serialization"
        );
    }

    #[test]
    fn omitted_password_is_preserved_and_empty_password_clears_it() {
        let current = NetConfig {
            password: Some("private-broker-secret".to_string()),
            ..NetConfig::default()
        };

        let mut omitted = NetConfig::default();
        omitted.preserve_write_only_secrets_from(&current);
        assert_eq!(omitted.password, current.password);

        let mut explicit_clear = NetConfig {
            password: Some(String::new()),
            ..NetConfig::default()
        };
        explicit_clear.preserve_write_only_secrets_from(&current);
        assert_eq!(explicit_clear.password.as_deref(), Some(""));
    }
}

// ── HTTP API models ───────────────────────────────────────────────────────────

/// Alarm broadcast request body (arbitrary JSON object forwarded as-is to the MQTT alarm topic).
#[derive(Debug, Deserialize, ToSchema)]
pub struct AlarmBroadcastRequest(pub serde_json::Value);

/// TLS certificate upload form (`POST /netApi/certificate/upload`, multipart/form-data).
///
/// Upload one certificate file per request. Use `cert_type` to specify the
/// certificate role; the original filename is ignored.
#[allow(dead_code)]
#[derive(ToSchema)]
pub struct CertUploadForm {
    /// Certificate type. Accepted values: `ca_cert` | `client_cert` | `client_key`.
    #[schema(example = "ca_cert")]
    pub cert_type: String,

    /// Certificate file. Supported formats: .pem .crt .key .cer .p12 .pfx. Maximum 1 MB.
    #[schema(format = Binary, value_type = String)]
    pub file: String,
}

/// System resource snapshot.
#[derive(Debug, Serialize, ToSchema)]
pub struct SystemMetrics {
    pub cpu_usage_percent: f32,
    pub memory_total_gb: f64,
    pub memory_used_gb: f64,
    pub memory_available_gb: f64,
    pub memory_usage_percent: f64,
    pub disk_total_gb: f64,
    pub disk_used_gb: f64,
    pub disk_free_gb: f64,
    pub disk_usage_percent: f64,
    pub network_bytes_sent: u64,
    pub network_bytes_recv: u64,
    pub system_uptime_hours: f64,
}

#[allow(dead_code)] // OpenAPI-only compatibility schema.
#[derive(Debug, ToSchema)]
pub struct UplinkDataResponse<T> {
    pub success: bool,
    pub message: String,
    pub data: T,
}

#[allow(dead_code)] // OpenAPI-only compatibility schema.
#[derive(Debug, ToSchema)]
pub struct AlarmQueuedResponse {
    pub success: bool,
    pub message: String,
    pub outbox_id: u64,
}
