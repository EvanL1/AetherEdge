//! HTTP broadcast to api (6005) and uplink (6006)

use chrono::Utc;
use reqwest::Client;
use serde_json::Value;
use tracing::{debug, warn};

use crate::db::AlarmCounts;
use crate::models::AlertRule;

pub struct Broadcaster {
    client: Client,
    api_url: String,
    uplink_url: String,
}

impl Broadcaster {
    pub fn new(client: Client, api_url: String, uplink_url: String) -> Self {
        Self {
            client,
            api_url,
            uplink_url,
        }
    }

    pub async fn send_alarm_triggered(&self, alert_id: i64, rule: &AlertRule, current_value: f64) {
        let ts = Utc::now().timestamp();
        let payload = serde_json::json!({
            "type": "alarm",
            "id": format!("alarm_{:03}", alert_id),
            "timestamp": ts,
            "data": {
                "alarm_id": alert_id.to_string(),
                "service_type": rule.service_type,
                "source": rule.service_type,
                "device": rule.channel_id.to_string(),
                "channel_id": rule.channel_id,
                "data_type": rule.data_type,
                "point_id": rule.point_id,
                "status": 1,
                "level": rule.warning_level,
                "value": current_value,
                "message": format!(
                    "{}: {} {} {}",
                    rule.rule_name, current_value, rule.operator, rule.value
                ),
            }
        });
        self.broadcast_all(&payload).await;
    }

    pub async fn send_alarm_recovery(
        &self,
        alert_id: i64,
        rule: &AlertRule,
        recovery_value: Option<f64>,
        reason: &str,
    ) {
        let ts = Utc::now().timestamp();
        let (message, value) = match recovery_value {
            Some(rv) => (
                format!(
                    "{}已恢复: {} (不再满足 {} {})",
                    rule.rule_name, rv, rule.operator, rule.value
                ),
                rv,
            ),
            None => (format!("{}已恢复: {}", rule.rule_name, reason), 0.0),
        };

        let payload = serde_json::json!({
            "type": "alarm",
            "id": format!("alarm_{:03}_recovery", alert_id),
            "timestamp": ts,
            "data": {
                "alarm_id": alert_id.to_string(),
                "service_type": rule.service_type,
                "source": rule.service_type,
                "device": rule.channel_id.to_string(),
                "channel_id": rule.channel_id,
                "data_type": rule.data_type,
                "point_id": rule.point_id,
                "status": 0,
                "level": rule.warning_level,
                "value": value,
                "message": message,
            }
        });
        self.broadcast_all(&payload).await;
    }

    pub async fn send_alarm_count(&self, counts: &AlarmCounts) {
        let ts = Utc::now().timestamp();
        let payload = serde_json::json!({
            "type": "alarm_num",
            "id": format!("alarm_num_{}", ts),
            "timestamp": ts,
            "data": {
                "current_alarms": counts.total,
                "1": counts.low,
                "2": counts.medium,
                "3": counts.high,
                "update_time": ts,
                "server_id": "aether-alarm",
            }
        });
        self.broadcast_all(&payload).await;
    }

    async fn broadcast_all(&self, payload: &Value) {
        let urls = [
            format!("{}/api/v1/broadcast", self.api_url),
            format!("{}/netApi/alarm/broadcast", self.uplink_url),
        ];

        let futures = urls.iter().map(|url| {
            let url = url.clone();
            let client = self.client.clone();
            let payload = payload.clone();
            async move {
                match client
                    .post(&url)
                    .json(&payload)
                    .timeout(std::time::Duration::from_secs(3))
                    .send()
                    .await
                {
                    Ok(resp) if resp.status().is_success() => {
                        debug!("Broadcast ok: {}", url);
                    },
                    Ok(resp) => {
                        warn!("Broadcast failed: {} status={}", url, resp.status());
                    },
                    Err(e) => {
                        warn!("Broadcast error: {} err={}", url, e);
                    },
                }
            }
        });

        futures::future::join_all(futures).await;
    }

    /// Broadcast all currently active alerts (used by /call-data endpoint)
    pub async fn broadcast_active_alerts(
        &self,
        alerts: &[crate::models::Alert],
        rules: &std::collections::HashMap<i64, AlertRule>,
    ) {
        let mut futures_vec = Vec::new();
        for alert in alerts {
            if let Some(rule) = rules.get(&alert.rule_id) {
                let ts = Utc::now().timestamp();
                let payload = serde_json::json!({
                    "type": "alarm",
                    "id": format!("alarm_{:03}", alert.id),
                    "timestamp": ts,
                    "data": {
                        "alarm_id": alert.id.to_string(),
                        "service_type": rule.service_type,
                        "source": rule.service_type,
                        "device": rule.channel_id.to_string(),
                        "channel_id": rule.channel_id,
                        "data_type": rule.data_type,
                        "point_id": rule.point_id,
                        "status": 1,
                        "level": rule.warning_level,
                        "value": alert.current_value,
                        "message": format!(
                            "{}: {} {} {}",
                            rule.rule_name, alert.current_value, rule.operator, rule.value
                        ),
                    }
                });
                // Only broadcast to uplink for manual call-data
                let url = format!("{}/netApi/alarm/broadcast", self.uplink_url);
                let client = self.client.clone();
                futures_vec.push(async move {
                    let _ = client
                        .post(&url)
                        .json(&payload)
                        .timeout(std::time::Duration::from_secs(3))
                        .send()
                        .await;
                });
            }
        }
        futures::future::join_all(futures_vec).await;
    }
}
