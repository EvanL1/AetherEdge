use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::trace_context::{self, TraceParent};

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

/// Gateway metrics on the MQTT property wire.
#[derive(Serialize)]
pub struct SystemMetricsPayload {
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

impl From<crate::system_monitor::SystemMetricsSnapshot> for SystemMetricsPayload {
    fn from(value: crate::system_monitor::SystemMetricsSnapshot) -> Self {
        Self {
            cpu_usage_percent: value.cpu_usage_percent,
            memory_total_gb: value.memory_total_gb,
            memory_used_gb: value.memory_used_gb,
            memory_available_gb: value.memory_available_gb,
            memory_usage_percent: value.memory_usage_percent,
            disk_total_gb: value.disk_total_gb,
            disk_used_gb: value.disk_used_gb,
            disk_free_gb: value.disk_free_gb,
            disk_usage_percent: value.disk_usage_percent,
            network_bytes_sent: value.network_bytes_sent,
            network_bytes_recv: value.network_bytes_recv,
            system_uptime_hours: value.system_uptime_hours,
        }
    }
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
    /// W3C trace context established by the caller. Echoed on the
    /// reply so the cloud can attribute latency to a hop.
    #[serde(default, deserialize_with = "trace_context::deserialize_optional")]
    pub traceparent: Option<TraceParent>,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub traceparent: Option<TraceParent>,
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
    /// W3C trace context established by the caller. Echoed on the
    /// reply and forwarded on the loopback hop to automation.
    #[serde(default, deserialize_with = "trace_context::deserialize_optional")]
    pub traceparent: Option<TraceParent>,
}

/// Reply to `write-reply/{productSN}/{deviceSN}`.
/// Format matches Python uplink: `{ result: "success"|"fail", msgId }`.
#[derive(Serialize)]
pub struct WriteReply {
    pub result: String,
    #[serde(rename = "msgId")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub msg_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub traceparent: Option<TraceParent>,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub traceparent: Option<TraceParent>,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub traceparent: Option<TraceParent>,
}

/// Trace-context propagation across the cloud↔gateway envelope.
///
/// Every assertion here is a compatibility claim about a wire format that is
/// already deployed. Fielded gateways talk to clouds that predate this field.
#[cfg(test)]
mod envelope_tests {
    use super::*;

    const TP: &str = "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01";

    #[test]
    fn cloud_traceparent_is_accepted_on_write_and_echoed_on_the_reply() {
        let request: WriteRequest = serde_json::from_value(serde_json::json!({
            "source": "inst", "device": "3", "data_type": "A",
            "key": "setpoint", "value": 42.0, "msgId": "m-1", "traceparent": TP
        }))
        .expect("request parses");

        let reply = WriteReply {
            result: "success".to_string(),
            msg_id: request.msg_id,
            traceparent: request.traceparent,
        };

        assert_eq!(
            serde_json::to_value(&reply).expect("reply serializes"),
            serde_json::json!({"result": "success", "msgId": "m-1", "traceparent": TP})
        );
    }

    /// A cloud deployed before this field exists sends no `traceparent`. The
    /// request must still parse — adding an observability field may not break an
    /// existing control path.
    #[test]
    fn a_request_without_a_traceparent_still_parses() {
        let request: WriteRequest = serde_json::from_value(serde_json::json!({
            "source": "inst", "device": "3", "data_type": "A",
            "key": "setpoint", "value": 42.0, "msgId": "m-1"
        }))
        .expect("legacy request parses");

        assert_eq!(request.traceparent, None);

        let read: ReadRequest = serde_json::from_value(serde_json::json!({
            "source": "inst", "device": "3", "data_type": "M"
        }))
        .expect("legacy read parses");
        assert_eq!(read.traceparent, None);
    }

    /// And the reply to such a request must be byte-identical to what that cloud
    /// already parses. Emitting `"traceparent": null` would be a new key in every
    /// existing consumer's payload.
    #[test]
    fn a_reply_without_a_traceparent_gains_no_new_key() {
        let reply = WriteReply {
            result: "success".to_string(),
            msg_id: Some("m-1".to_string()),
            traceparent: None,
        };

        let json = serde_json::to_value(&reply).expect("reply serializes");
        assert_eq!(
            json,
            serde_json::json!({"result": "success", "msgId": "m-1"})
        );
        assert!(json.get("traceparent").is_none(), "no null key emitted");
    }

    /// The malformed case must degrade to "no trace", never to "no command".
    #[test]
    fn a_malformed_traceparent_does_not_reject_the_command() {
        let request: WriteRequest = serde_json::from_value(serde_json::json!({
            "source": "inst", "device": "3", "data_type": "A",
            "key": "setpoint", "value": 42.0, "msgId": "m-1",
            "traceparent": "00-not-hex-01"
        }))
        .expect("command still parses");

        assert_eq!(request.traceparent, None);
        assert_eq!(request.field, "setpoint");
    }
}
