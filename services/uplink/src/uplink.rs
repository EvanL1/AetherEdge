//! Durable local outbox to MQTT delivery adapter.

use std::sync::Arc;
use std::sync::atomic::Ordering;

use aether_application::OutboxForwarder;
use aether_domain::TimestampMs;
use aether_ports::{
    OutboxId, OutboxMessage, PortError, PortErrorKind, PortResult, UplinkPublisher,
};
use async_trait::async_trait;
use rumqttc::{AsyncClient, QoS};
use tokio::sync::Mutex;
use tokio::time::{self, Duration, MissedTickBehavior};
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

use crate::state::AppState;

const OUTBOX_COMPACTION_INTERVAL: Duration = Duration::from_secs(60 * 60);

struct MqttUplinkPublisher {
    client: Arc<Mutex<Option<AsyncClient>>>,
}

#[async_trait]
impl UplinkPublisher for MqttUplinkPublisher {
    async fn publish(&self, message: &OutboxMessage) -> PortResult<()> {
        // Clone the cheap AsyncClient handle and release the mutex before the
        // network-facing await. Reconnection can update AppState concurrently.
        let client =
            self.client.lock().await.clone().ok_or_else(|| {
                PortError::new(PortErrorKind::Unavailable, "MQTT is not connected")
            })?;
        client
            .publish(
                message.destination(),
                QoS::AtLeastOnce,
                false,
                message.payload(),
            )
            .await
            .map_err(|error| {
                PortError::new(
                    PortErrorKind::Unavailable,
                    format!("MQTT publish queue rejected message: {error}"),
                )
            })
    }
}

/// Serializes a message and commits it to the local outbox before returning.
pub async fn enqueue_json(
    state: &AppState,
    destination: &str,
    value: &impl serde::Serialize,
) -> anyhow::Result<OutboxId> {
    let payload = serde_json::to_vec(value)?;
    let created_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0);
    state
        .outbox
        .enqueue(OutboxMessage::new(
            destination,
            payload,
            TimestampMs::new(created_at),
        ))
        .await
        .map_err(anyhow::Error::new)
}

/// Drains recovered and newly queued messages whenever MQTT is available.
pub async fn run_outbox_forwarder(state: Arc<AppState>, shutdown: CancellationToken) {
    let publisher: Arc<dyn UplinkPublisher> = Arc::new(MqttUplinkPublisher {
        client: Arc::clone(&state.mqtt_client),
    });
    let forwarder = OutboxForwarder::new(Arc::clone(&state.outbox), publisher);
    let mut tick = time::interval(Duration::from_millis(500));
    tick.set_missed_tick_behavior(MissedTickBehavior::Skip);

    loop {
        tokio::select! {
            _ = tick.tick() => {},
            _ = shutdown.cancelled() => return,
        }

        if !state.mqtt_connected.load(Ordering::Relaxed) {
            continue;
        }

        match forwarder.drain_once(128).await {
            Ok(report) if report.delivered() > 0 => {
                debug!(
                    delivered = report.delivered(),
                    "durable MQTT outbox batch submitted"
                );
            },
            Ok(_) => {},
            Err(error) if error.is_retryable() => {
                debug!(%error, "MQTT outbox paused until uplink recovers");
            },
            Err(error) => {
                warn!(%error, "MQTT outbox drain requires operator attention");
            },
        }
    }
}

/// Reclaims acknowledged journal records at startup and then hourly.
pub async fn run_outbox_maintenance(
    outbox: Arc<aether_store_local::FileOutbox>,
    shutdown: CancellationToken,
) {
    let mut tick = time::interval(OUTBOX_COMPACTION_INTERVAL);
    tick.set_missed_tick_behavior(MissedTickBehavior::Skip);

    loop {
        tokio::select! {
            _ = tick.tick() => {},
            _ = shutdown.cancelled() => return,
        }

        if let Err(error) = outbox.compact().await {
            warn!(%error, "MQTT outbox compaction failed");
        }
    }
}
