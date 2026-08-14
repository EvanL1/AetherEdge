//! HTTP adapter for alarm notifications.

use async_trait::async_trait;
use reqwest::Client;
use serde_json::{Value, json};
use tracing::{debug, warn};

use crate::notification::{
    AlarmCountSnapshot, AlarmNotification, AlarmNotificationDestination, AlarmNotifier,
};

#[derive(Clone)]
pub struct HttpAlarmNotifier {
    client: Client,
    api_url: String,
    uplink_url: String,
    api_broadcast_token: String,
}

impl HttpAlarmNotifier {
    pub fn new(
        client: Client,
        api_url: String,
        uplink_url: String,
        api_broadcast_token: String,
    ) -> Self {
        Self {
            client,
            api_url,
            uplink_url,
            api_broadcast_token,
        }
    }

    fn alarm_payload(notification: &AlarmNotification) -> Value {
        let event_suffix = if notification.status == 0 {
            "_recovery"
        } else {
            ""
        };
        json!({
            "type": "alarm",
            "id": format!("alarm_{:03}{event_suffix}", notification.alert_id),
            "event_id": notification.event_id,
            "timestamp": notification.timestamp,
            "data": {
                "alarm_id": notification.alert_id.to_string(),
                "event_id": notification.event_id,
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

    async fn send_alarm(
        &self,
        destination: AlarmNotificationDestination,
        notification: &AlarmNotification,
    ) -> anyhow::Result<()> {
        let url = match destination {
            AlarmNotificationDestination::Api => {
                format!("{}/api/internal/alarm-events", self.api_url)
            },
            AlarmNotificationDestination::Uplink => {
                format!("{}/api/internal/alarm-events", self.uplink_url)
            },
        };
        let request = self
            .client
            .post(&url)
            .header("Idempotency-Key", &notification.event_id)
            .header("X-Aether-Event-ID", &notification.event_id)
            .json(&Self::alarm_payload(notification));
        let response = request
            .bearer_auth(&self.api_broadcast_token)
            .timeout(std::time::Duration::from_secs(3))
            .send()
            .await
            .map_err(|error| anyhow::anyhow!("alarm delivery to {url} failed: {error}"))?;
        if !response.status().is_success() {
            anyhow::bail!(
                "alarm delivery to {url} returned HTTP {}",
                response.status()
            );
        }
        debug!(
            event_id = notification.event_id,
            destination = destination.as_str(),
            "Alarm transition delivered"
        );
        Ok(())
    }

    async fn broadcast_all(&self, payload: &Value) {
        let destinations = [format!("{}/api/internal/alarm-events", self.api_url)];
        futures::future::join_all(destinations.into_iter().map(|url| {
            let client = self.client.clone();
            let payload = payload.clone();
            let api_broadcast_token = self.api_broadcast_token.clone();
            async move {
                let request = client.post(&url).json(&payload);
                let request = request.bearer_auth(api_broadcast_token);
                match request
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
}

#[async_trait]
impl AlarmNotifier for HttpAlarmNotifier {
    async fn deliver_alarm(
        &self,
        destination: AlarmNotificationDestination,
        notification: &AlarmNotification,
    ) -> anyhow::Result<()> {
        self.send_alarm(destination, notification).await
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
