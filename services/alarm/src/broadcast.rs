//! HTTP adapter for alarm notifications.

use async_trait::async_trait;
use reqwest::Client;
use serde_json::{Value, json};
use tracing::{debug, warn};

use crate::notification::{AlarmCountSnapshot, AlarmNotification, AlarmNotifier};

#[derive(Clone)]
pub struct HttpAlarmNotifier {
    client: Client,
    api_url: String,
    uplink_url: String,
}

impl HttpAlarmNotifier {
    pub fn new(client: Client, api_url: String, uplink_url: String) -> Self {
        Self {
            client,
            api_url,
            uplink_url,
        }
    }

    fn alarm_payload(notification: AlarmNotification) -> Value {
        let event_suffix = if notification.status == 0 {
            "_recovery"
        } else {
            ""
        };
        json!({
            "type": "alarm",
            "id": format!("alarm_{:03}{event_suffix}", notification.alert_id),
            "timestamp": notification.timestamp,
            "data": {
                "alarm_id": notification.alert_id.to_string(),
                "service_type": notification.service_type,
                "source": notification.service_type,
                "device": notification.channel_id.to_string(),
                "channel_id": notification.channel_id,
                "data_type": notification.data_type,
                "point_id": notification.point_id,
                "status": notification.status,
                "level": notification.warning_level,
                "value": notification.value,
                "message": notification.message,
            }
        })
    }

    async fn broadcast_all(&self, payload: &Value) {
        let urls = [
            format!("{}/api/v1/broadcast", self.api_url),
            format!("{}/netApi/alarm/broadcast", self.uplink_url),
        ];
        futures::future::join_all(urls.into_iter().map(|url| {
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
                    Ok(response) if response.status().is_success() => {
                        debug!("Broadcast ok: {}", url);
                    },
                    Ok(response) => {
                        warn!("Broadcast failed: {} status={}", url, response.status());
                    },
                    Err(error) => {
                        warn!("Broadcast error: {} err={}", url, error);
                    },
                }
            }
        }))
        .await;
    }

    async fn send_uplink(&self, payload: &Value) {
        let url = format!("{}/netApi/alarm/broadcast", self.uplink_url);
        if let Err(error) = self
            .client
            .post(&url)
            .json(payload)
            .timeout(std::time::Duration::from_secs(3))
            .send()
            .await
        {
            warn!("Alarm replay broadcast error: {} err={}", url, error);
        }
    }
}

#[async_trait]
impl AlarmNotifier for HttpAlarmNotifier {
    async fn publish_alarm(&self, notification: AlarmNotification) {
        self.broadcast_all(&Self::alarm_payload(notification)).await;
    }

    async fn replay_alarm(&self, notification: AlarmNotification) {
        self.send_uplink(&Self::alarm_payload(notification)).await;
    }

    async fn publish_counts(&self, counts: AlarmCountSnapshot) {
        let timestamp = chrono::Utc::now().timestamp();
        let payload = json!({
            "type": "alarm_num",
            "id": format!("alarm_num_{timestamp}"),
            "timestamp": timestamp,
            "data": {
                "current_alarms": counts.total,
                "1": counts.low,
                "2": counts.medium,
                "3": counts.high,
                "update_time": timestamp,
                "server_id": "aether-alarm",
            }
        });
        self.broadcast_all(&payload).await;
    }
}
