//! Typed loopback client for the Alarm service.

use anyhow::Context;
use serde::Serialize;

#[derive(Clone)]
pub struct AlarmClient {
    http: reqwest::Client,
}

impl AlarmClient {
    pub fn new(http: reqwest::Client) -> Self {
        Self { http }
    }

    pub async fn request_replay(
        &self,
        alarm_url: &str,
        message_id: Option<&str>,
        timestamp: i64,
    ) -> anyhow::Result<AlarmReplayOutcome> {
        let url = format!("{}/alarmApi/call-data", alarm_url.trim_end_matches('/'));
        let response = self
            .http
            .post(url)
            .json(&AlarmReplayRequest {
                message_id: message_id.unwrap_or(""),
                timestamp,
            })
            .send()
            .await
            .context("request Alarm replay")?;
        if response.status().is_success() {
            Ok(AlarmReplayOutcome::Accepted)
        } else {
            Ok(AlarmReplayOutcome::Rejected(response.status().as_u16()))
        }
    }
}

pub enum AlarmReplayOutcome {
    Accepted,
    Rejected(u16),
}

#[derive(Serialize)]
struct AlarmReplayRequest<'a> {
    #[serde(rename = "msgId")]
    message_id: &'a str,
    timestamp: i64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replay_request_preserves_the_deployed_wire_names() {
        let request = AlarmReplayRequest {
            message_id: "m-1",
            timestamp: 42,
        };
        assert_eq!(
            serde_json::to_value(request).expect("serialize replay request"),
            serde_json::json!({"msgId": "m-1", "timestamp": 42})
        );
    }
}
