//! Internal storage, query, and runtime models for the alarm service.

// ============================================================================
// Core domain models (map 1:1 to database tables)
// ============================================================================

#[derive(Debug, Clone, sqlx::FromRow)]
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

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Alert {
    pub id: i64,
    pub rule_id: i64,
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

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct AlertEvent {
    pub id: i64,
    pub rule_id: i64,
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

#[derive(Debug, Clone, Copy)]
pub struct PageRequest {
    pub limit: i64,
    pub offset: i64,
    pub page: i64,
    pub page_size: i64,
}

impl PageRequest {
    pub fn resolve(page: Option<i64>, page_size: Option<i64>, skip: i64, limit: i64) -> Self {
        const MAX_PAGE_SIZE: i64 = 200;
        let page_size = page_size.unwrap_or(limit).clamp(1, MAX_PAGE_SIZE);
        match page {
            Some(page) => {
                let page = page.max(1);
                Self {
                    limit: page_size,
                    offset: (page - 1) * page_size,
                    page,
                    page_size,
                }
            },
            None => {
                let offset = skip.max(0);
                Self {
                    limit: page_size,
                    offset,
                    page: offset / page_size + 1,
                    page_size,
                }
            },
        }
    }
}

#[derive(Debug)]
pub struct RuleFilter {
    pub keyword: Option<String>,
    pub service_type: Option<String>,
    pub channel_id: Option<i64>,
    pub data_type: Option<String>,
    pub enabled: Option<bool>,
    pub warning_level: Option<i64>,
    pub page: PageRequest,
}

#[derive(Debug)]
pub struct AlertFilter {
    pub warning_level: Option<i64>,
    pub service_type: Option<String>,
    pub channel_id: Option<i64>,
    pub keyword: Option<String>,
    pub page: PageRequest,
}

#[derive(Debug)]
pub struct EventFilter {
    pub keyword: Option<String>,
    pub rule_id: Option<i64>,
    pub event_type: Option<String>,
    pub service_type: Option<String>,
    pub warning_level: Option<i64>,
    pub start_time: Option<i64>,
    pub end_time: Option<i64>,
    pub page: PageRequest,
}

#[derive(Debug)]
pub struct Page<T> {
    pub total: i64,
    pub list: Vec<T>,
    pub page: i64,
    pub page_size: i64,
}

#[derive(Debug, Clone)]
pub struct MonitorStatus {
    pub running: bool,
    pub last_check_time: Option<i64>,
    pub check_interval: u64,
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
