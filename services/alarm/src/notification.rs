//! Transport-neutral alarm notification seam.

use async_trait::async_trait;

use crate::models::AlertRule;

#[derive(Debug)]
pub struct AlarmNotification {
    pub alert_id: i64,
    pub timestamp: i64,
    pub service_type: String,
    pub channel_id: i64,
    pub data_type: String,
    pub point_id: i64,
    pub warning_level: i64,
    pub status: u8,
    pub value: f64,
    pub message: String,
}

impl AlarmNotification {
    pub fn triggered(alert_id: i64, rule: &AlertRule, current_value: f64) -> Self {
        Self {
            alert_id,
            timestamp: chrono::Utc::now().timestamp(),
            service_type: rule.service_type.clone(),
            channel_id: rule.channel_id,
            data_type: rule.data_type.clone(),
            point_id: rule.point_id,
            warning_level: rule.warning_level,
            status: 1,
            value: current_value,
            message: format!(
                "{}: {} {} {}",
                rule.rule_name, current_value, rule.operator, rule.value
            ),
        }
    }

    pub fn recovered(
        alert_id: i64,
        rule: &AlertRule,
        recovery_value: Option<f64>,
        reason: &str,
    ) -> Self {
        let (message, value) = match recovery_value {
            Some(value) => (
                format!(
                    "{} recovered: {} (no longer {} {})",
                    rule.rule_name, value, rule.operator, rule.value
                ),
                value,
            ),
            None => (format!("{} recovered: {}", rule.rule_name, reason), 0.0),
        };
        Self {
            alert_id,
            timestamp: chrono::Utc::now().timestamp(),
            service_type: rule.service_type.clone(),
            channel_id: rule.channel_id,
            data_type: rule.data_type.clone(),
            point_id: rule.point_id,
            warning_level: rule.warning_level,
            status: 0,
            value,
            message,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct AlarmCountSnapshot {
    pub total: i64,
    pub low: i64,
    pub medium: i64,
    pub high: i64,
}

impl From<&crate::db::AlarmCounts> for AlarmCountSnapshot {
    fn from(value: &crate::db::AlarmCounts) -> Self {
        Self {
            total: value.total,
            low: value.low,
            medium: value.medium,
            high: value.high,
        }
    }
}

#[async_trait]
pub trait AlarmNotifier: Send + Sync {
    async fn publish_alarm(&self, notification: AlarmNotification);
    async fn replay_alarm(&self, notification: AlarmNotification);
    async fn publish_counts(&self, counts: AlarmCountSnapshot);
}
