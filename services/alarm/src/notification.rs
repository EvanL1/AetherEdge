//! Transport-neutral alarm notification seam.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::models::{Alert, AlertRule};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AlarmNotification {
    /// Stable transition identifier used as the downstream idempotency key.
    pub event_id: String,
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
    pub fn triggered_at(
        alert_id: i64,
        rule: &AlertRule,
        current_value: f64,
        timestamp: i64,
    ) -> Self {
        Self {
            event_id: format!("alarm-trigger-{alert_id}"),
            alert_id,
            timestamp,
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

    pub fn recovered_at(
        event_id: i64,
        alert: &Alert,
        recovery_value: Option<f64>,
        reason: &str,
        timestamp: i64,
    ) -> Self {
        let (message, value) = match recovery_value {
            Some(value) => (
                format!(
                    "{} recovered: {} (no longer {} {})",
                    alert.rule_name, value, alert.operator, alert.threshold_value
                ),
                value,
            ),
            None => (format!("{} recovered: {}", alert.rule_name, reason), 0.0),
        };
        Self {
            event_id: format!("alarm-recovery-{event_id}"),
            alert_id: alert.id,
            timestamp,
            service_type: alert.service_type.clone(),
            channel_id: alert.channel_id,
            data_type: alert.data_type.clone(),
            point_id: alert.point_id,
            warning_level: alert.warning_level,
            status: 0,
            value,
            message,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AlarmNotificationDestination {
    Api,
    Uplink,
}

impl AlarmNotificationDestination {
    pub const ALL: [Self; 2] = [Self::Api, Self::Uplink];

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Api => "api",
            Self::Uplink => "uplink",
        }
    }

    pub fn parse(value: &str) -> anyhow::Result<Self> {
        match value {
            "api" => Ok(Self::Api),
            "uplink" => Ok(Self::Uplink),
            other => anyhow::bail!("unknown alarm notification destination: {other}"),
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
    /// Delivers one durable alarm transition to exactly one destination.
    ///
    /// A successful return only acknowledges that destination. The caller
    /// retains other destinations independently and may retry an ambiguous
    /// response with the same `event_id`.
    async fn deliver_alarm(
        &self,
        destination: AlarmNotificationDestination,
        notification: &AlarmNotification,
    ) -> anyhow::Result<()>;

    /// Best-effort, periodically rebuilt aggregate snapshot.
    async fn publish_counts(&self, counts: AlarmCountSnapshot);
}
