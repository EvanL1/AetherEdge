//! Data models for the alarm service

use serde::{Deserialize, Serialize, Serializer};
use utoipa::{IntoParams, ToSchema};

/// Serialize a stored JSON string as a parsed JSON value.
/// If the string is not valid JSON, it falls back to the raw string.
fn serialize_json_str<S>(s: &String, ser: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    match serde_json::from_str::<serde_json::Value>(s) {
        Ok(v) => v.serialize(ser),
        Err(_) => s.serialize(ser),
    }
}

// ============================================================================
// Core domain models (map 1:1 to database tables)
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow, ToSchema)]
pub struct AlertRule {
    pub id: i64,
    pub service_type: String,
    pub channel_id: i64,
    pub data_type: String,
    pub point_id: i64,
    pub rule_name: String,
    /// Warning level: 1=low, 2=medium, 3=high
    pub warning_level: i64,
    /// Operator: >, <, >=, <=, ==, !=
    pub operator: String,
    pub value: f64,
    pub enabled: bool,
    pub description: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

impl AlertRule {
    pub fn evaluate(&self, current_value: f64) -> bool {
        match self.operator.as_str() {
            ">" => current_value > self.value,
            "<" => current_value < self.value,
            ">=" => current_value >= self.value,
            "<=" => current_value <= self.value,
            "==" => (current_value - self.value).abs() < 1e-6,
            "!=" => (current_value - self.value).abs() >= 1e-6,
            _ => false,
        }
    }

    /// Sentinel `data_type` that resolves the rule through the channel-health
    /// SHM segment. `channel_id` identifies the monitored channel and
    /// `point_id` is unused. Threshold semantics: `==` 0 fires when the
    /// channel is offline; `==` 1 fires when it is online.
    pub const CHANNEL_ONLINE_DATA_TYPE: &'static str = "online";

    fn is_channel_online_rule(&self) -> bool {
        self.service_type == "io" && self.data_type == Self::CHANNEL_ONLINE_DATA_TYPE
    }

    /// Logical live-state selector: `{service_type}:{channel_id}:{data_type}`.
    ///
    /// Channel-online rules use the dedicated `io:online` namespace because
    /// the reader resolves them through channel-health SHM.
    pub fn logical_key(&self) -> String {
        if self.is_channel_online_rule() {
            return "io:online".to_string();
        }
        format!(
            "{}:{}:{}",
            self.service_type, self.channel_id, self.data_type
        )
    }

    /// Serialise rule metadata as a JSON snapshot for storage in alert/event tables
    pub fn snapshot(&self) -> String {
        serde_json::json!({
            "rule_name": self.rule_name,
            "warning_level": self.warning_level,
            "operator": self.operator,
            "value": self.value,
            "description": self.description,
        })
        .to_string()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow, ToSchema)]
pub struct Alert {
    pub id: i64,
    pub rule_id: i64,
    #[serde(serialize_with = "serialize_json_str")]
    pub rule_snapshot: String,
    pub service_type: String,
    pub channel_id: i64,
    pub data_type: String,
    pub point_id: i64,
    pub rule_name: String,
    pub warning_level: i64,
    pub operator: String,
    pub threshold_value: f64,
    pub current_value: f64,
    /// Always "active" — resolved alerts are deleted and moved to alert_event
    pub status: String,
    pub triggered_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow, ToSchema)]
pub struct AlertEvent {
    pub id: i64,
    pub rule_id: i64,
    #[serde(serialize_with = "serialize_json_str")]
    pub rule_snapshot: String,
    pub service_type: String,
    pub channel_id: i64,
    pub data_type: String,
    pub point_id: i64,
    pub rule_name: String,
    pub warning_level: i64,
    pub operator: String,
    pub threshold_value: f64,
    pub trigger_value: Option<f64>,
    pub recovery_value: Option<f64>,
    /// "trigger" | "recovery"
    pub event_type: String,
    pub triggered_at: Option<i64>,
    pub recovered_at: Option<i64>,
    /// Duration in seconds
    pub duration: Option<i64>,
}

// ============================================================================
// Request DTOs
// ============================================================================

#[derive(Debug, Deserialize, ToSchema)]
#[schema(example = json!({
    "service_type": "io",
    "channel_id": 1001,
    "data_type": "M",
    "point_id": 1,
    "rule_name": "Overvoltage Alarm",
    "warning_level": 2,
    "operator": ">",
    "value": 260.0,
    "enabled": true,
    "description": "Trigger alarm when voltage exceeds 260V"
}))]
pub struct CreateRuleRequest {
    pub service_type: String,
    pub channel_id: i64,
    pub data_type: String,
    pub point_id: i64,
    pub rule_name: String,
    /// Warning level (default: 2)
    #[serde(default = "default_warning_level")]
    pub warning_level: i64,
    /// Operator: >, <, >=, <=, ==, !=
    pub operator: String,
    pub value: f64,
    /// Whether enabled (default: true)
    #[serde(default = "default_true")]
    pub enabled: bool,
    pub description: Option<String>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct UpdateRuleRequest {
    pub service_type: Option<String>,
    pub channel_id: Option<i64>,
    pub data_type: Option<String>,
    pub point_id: Option<i64>,
    pub rule_name: Option<String>,
    pub warning_level: Option<i64>,
    pub operator: Option<String>,
    pub value: Option<f64>,
    pub enabled: Option<bool>,
    pub description: Option<String>,
}

// ============================================================================
// Query parameter structs
// ============================================================================

#[derive(Debug, Deserialize, Default, IntoParams)]
pub struct RuleQueryParams {
    /// Fuzzy keyword: matches rule_name, description, channel_id, point_id
    pub keyword: Option<String>,
    pub service_type: Option<String>,
    pub channel_id: Option<i64>,
    pub data_type: Option<String>,
    pub enabled: Option<bool>,
    pub warning_level: Option<i64>,
    /// Page number (1-based; takes priority over skip when set)
    pub page: Option<i64>,
    /// Page size (used with page; takes priority over limit when set)
    pub page_size: Option<i64>,
    /// Offset rows (legacy; ignored when page is present)
    #[serde(default)]
    pub skip: i64,
    /// Max rows to return (legacy; ignored when page_size is present)
    #[serde(default = "default_limit")]
    pub limit: i64,
}

#[derive(Debug, Deserialize, Default, IntoParams)]
pub struct AlertQueryParams {
    pub warning_level: Option<i64>,
    pub service_type: Option<String>,
    pub channel_id: Option<i64>,
    pub keyword: Option<String>,
    /// Page number (1-based; takes priority over skip when set)
    pub page: Option<i64>,
    /// Page size (used with page; takes priority over limit when set)
    pub page_size: Option<i64>,
    /// Offset rows (legacy)
    #[serde(default)]
    pub skip: i64,
    /// Max rows to return (legacy)
    #[serde(default = "default_limit")]
    pub limit: i64,
}

#[derive(Debug, Deserialize, Default, IntoParams)]
pub struct EventQueryParams {
    /// Fuzzy keyword: matches rule_name, channel_id, point_id
    pub keyword: Option<String>,
    pub rule_id: Option<i64>,
    /// "trigger" or "recovery"
    pub event_type: Option<String>,
    pub service_type: Option<String>,
    pub warning_level: Option<i64>,
    /// Unix timestamp (seconds)
    pub start_time: Option<i64>,
    pub end_time: Option<i64>,
    /// Page number (1-based; takes priority over skip when set)
    pub page: Option<i64>,
    /// Page size (used with page; takes priority over limit when set)
    pub page_size: Option<i64>,
    /// Offset rows (legacy)
    #[serde(default)]
    pub skip: i64,
    /// Max rows to return (legacy)
    #[serde(default = "default_limit")]
    pub limit: i64,
}

/// Resolve pagination parameters; returns `(effective_limit, offset, resolved_page, resolved_page_size)`.
///
/// Prefers `page`/`page_size`; falls back to `skip`/`limit` when neither page param is present.
pub fn resolve_pagination(
    page: Option<i64>,
    page_size: Option<i64>,
    skip: i64,
    limit: i64,
) -> (i64, i64, i64, i64) {
    const MAX_PAGE_SIZE: i64 = 200;
    match page {
        Some(p) => {
            let p = p.max(1);
            let ps = page_size.unwrap_or(limit).clamp(1, MAX_PAGE_SIZE);
            let offset = (p - 1) * ps;
            (ps, offset, p, ps)
        },
        None => {
            let ps = page_size.unwrap_or(limit).clamp(1, MAX_PAGE_SIZE);
            let offset = skip.max(0);
            // Convert skip/limit back to a logical page number (best-effort)
            let p = if ps > 0 { offset / ps + 1 } else { 1 };
            (ps, offset, p, ps)
        },
    }
}

// ============================================================================
// Response wrappers
// ============================================================================

#[derive(Debug, Serialize, ToSchema)]
pub struct ApiResponse<T: Serialize> {
    pub success: bool,
    pub message: String,
    pub data: T,
}

/// Success payload returned after creating a rule.
#[derive(Debug, Serialize, ToSchema)]
pub struct CreateRuleData {
    pub rule_id: u64,
    pub rule_name: String,
    pub logical_key: Option<String>,
    pub point_id: i64,
    pub monitoring: bool,
    pub rule: Option<AlertRule>,
    pub request_id: String,
    pub audit: CompletionAuditData,
}

/// Compatibility payload used by legacy single-item endpoints.
#[derive(Debug, Serialize, ToSchema)]
pub struct SingleItemData<T: Serialize> {
    pub total: i64,
    pub list: Vec<T>,
}

/// Success payload returned after updating a rule.
#[derive(Debug, Serialize, ToSchema)]
pub struct RuleIdData {
    pub rule_id: u64,
    pub request_id: String,
    pub audit: CompletionAuditData,
}

/// Success payload returned after an active alert is manually resolved.
#[derive(Debug, Serialize, ToSchema)]
pub struct AlertResolutionData {
    pub alert_id: u64,
    pub rule_id: u64,
    pub resolved_at_ms: u64,
    pub request_id: String,
    pub audit: CompletionAuditData,
}

/// Public terminal-audit state for an already accepted non-idempotent command.
#[derive(Debug, Serialize, ToSchema)]
pub struct CompletionAuditData {
    pub status: String,
    pub retryable: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

impl<T: Serialize> ApiResponse<T> {
    pub fn ok(message: impl Into<String>, data: T) -> Self {
        Self {
            success: true,
            message: message.into(),
            data,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct PagedData<T: Serialize> {
    pub total: i64,
    pub list: Vec<T>,
    /// Current page number (1-based)
    pub page: i64,
    /// Page size used for this query
    pub page_size: i64,
}

// ============================================================================
// Monitor state
// ============================================================================

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct MonitorStatus {
    pub running: bool,
    pub last_check_time: Option<i64>,
    pub check_interval: u64,
}

// ============================================================================
// Helpers
// ============================================================================

fn default_warning_level() -> i64 {
    2
}

fn default_true() -> bool {
    true
}

fn default_limit() -> i64 {
    20
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(service_type: &str, channel_id: i64, data_type: &str, point_id: i64) -> AlertRule {
        AlertRule {
            id: 1,
            service_type: service_type.to_string(),
            channel_id,
            data_type: data_type.to_string(),
            point_id,
            rule_name: "t".to_string(),
            warning_level: 2,
            operator: "==".to_string(),
            value: 0.0,
            enabled: true,
            description: None,
            created_at: 0,
            updated_at: 0,
        }
    }

    #[test]
    fn channel_data_rule_exposes_logical_key() {
        let r = rule("io", 1001, "T", 5);
        assert_eq!(r.logical_key(), "io:1001:T");
        assert_eq!(r.point_id, 5);
    }

    #[test]
    fn channel_online_rule_uses_health_namespace() {
        let r = rule("io", 1001, AlertRule::CHANNEL_ONLINE_DATA_TYPE, 0);
        assert_eq!(r.logical_key(), "io:online");
        assert_eq!(r.channel_id, 1001);
    }

    #[test]
    fn channel_online_rule_evaluates_offline_as_zero() {
        // A rule configured as `== 0.0` fires for an offline health sample.
        let r = AlertRule {
            value: 0.0,
            operator: "==".to_string(),
            ..rule("io", 1001, AlertRule::CHANNEL_ONLINE_DATA_TYPE, 0)
        };
        assert!(r.evaluate(0.0), "offline value 0 must trigger");
        assert!(!r.evaluate(1.0), "online value 1 must not trigger");
    }

    #[test]
    fn instance_rule_with_online_data_type_does_not_get_singleton_treatment() {
        // The sentinel is scoped to service_type == "io"; an "inst:online"
        // rule (nonsensical but possible via the API) keeps its own namespace
        // rather than silently pointing at channel health.
        let r = rule("inst", 42, AlertRule::CHANNEL_ONLINE_DATA_TYPE, 7);
        assert_eq!(r.logical_key(), "inst:42:online");
        assert_eq!(r.point_id, 7);
    }
}
