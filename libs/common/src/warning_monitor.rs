//! Warning Monitor Module
//!
//! Subscribes to Redis Pub/Sub channels for real-time warning notifications
//! from Redis Lua functions (queue overflow, unmapped points, etc.)

use futures::StreamExt;
use redis::{Client, RedisResult};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

/// Queue overflow warning data
#[derive(Debug, Serialize, Deserialize)]
pub struct QueueOverflowWarning {
    pub service: String,
    pub channel_id: u16,
    pub point_type: String,
    pub queue_length: usize,
    pub timestamp: i64,
    pub severity: String,
}

/// Unmapped points warning data
#[derive(Debug, Serialize, Deserialize)]
pub struct UnmappedPointsWarning {
    pub service: String,
    pub channel_id: u16,
    pub telemetry_type: String,
    pub unmapped_count: u32,
    pub routed_count: u32,
    pub timestamp: i64,
    pub severity: String,
}

/// Warning statistics for tracking (queryable via health endpoints)
#[derive(Debug, Default, Clone, serde::Serialize)]
pub struct WarningStats {
    pub queue_overflow_count: u64,
    pub queue_high_count: u64,
    pub unmapped_points_count: u64,
    pub last_queue_overflow: Option<i64>,
    pub last_unmapped_points: Option<i64>,
}

/// Shared handle to warning stats, readable from health endpoints
pub type WarningStatsHandle = Arc<RwLock<WarningStats>>;

/// Start the warning monitor that subscribes to Redis warning channels.
///
/// Returns a shared handle to live warning statistics, queryable from health endpoints.
/// The monitoring loop runs as a background task and stops when `token` is cancelled.
pub async fn start_warning_monitor(
    redis_url: String,
    token: CancellationToken,
) -> RedisResult<WarningStatsHandle> {
    let client = Client::open(redis_url.as_str())?;
    let mut pubsub = client.get_async_pubsub().await?;

    pubsub
        .subscribe(&[
            "warnings:queue_overflow",
            "warnings:queue_high",
            "warnings:unmapped_points",
        ])
        .await?;

    info!("WarnMonitor started");

    let stats: WarningStatsHandle = Arc::new(RwLock::new(WarningStats::default()));
    let stats_clone = Arc::clone(&stats);

    tokio::spawn(async move {
        let mut pubsub_stream = pubsub.on_message();
        loop {
            tokio::select! {
                Some(msg) = pubsub_stream.next() => {
                    let channel = msg.get_channel_name();
                    let Ok(payload) = msg.get_payload::<String>() else {
                        continue;
                    };
                    process_warning(&stats_clone, channel, &payload).await;
                }
                _ = token.cancelled() => {
                    debug!("WarnMonitor stopping");
                    break;
                }
            }
        }
        let s = stats_clone.read().await;
        info!(
            "WarnMonitor stats: overflow={} high={} unmapped={}",
            s.queue_overflow_count, s.queue_high_count, s.unmapped_points_count
        );
    });

    Ok(stats)
}

async fn process_warning(stats: &WarningStatsHandle, channel: &str, payload: &str) {
    match channel {
        "warnings:queue_overflow" => {
            if let Ok(data) = serde_json::from_str::<QueueOverflowWarning>(payload) {
                error!(
                    "QUEUE OVERFLOW: {} Ch{}:{} len={}",
                    data.service, data.channel_id, data.point_type, data.queue_length
                );
                let mut s = stats.write().await;
                s.queue_overflow_count += 1;
                s.last_queue_overflow = Some(data.timestamp);
            }
        },
        "warnings:queue_high" => {
            if let Ok(data) = serde_json::from_str::<QueueOverflowWarning>(payload) {
                warn!(
                    "Queue high: {} Ch{}:{} len={}",
                    data.service, data.channel_id, data.point_type, data.queue_length
                );
                let mut s = stats.write().await;
                s.queue_high_count += 1;
            }
        },
        "warnings:unmapped_points" => {
            if let Ok(data) = serde_json::from_str::<UnmappedPointsWarning>(payload) {
                if data.unmapped_count > 10 {
                    warn!(
                        "Unmapped: {} Ch{}:{} unmapped={} routed={}",
                        data.service,
                        data.channel_id,
                        data.telemetry_type,
                        data.unmapped_count,
                        data.routed_count
                    );
                } else {
                    debug!(
                        "Unmapped: {} Ch{}:{} unmapped={} routed={}",
                        data.service,
                        data.channel_id,
                        data.telemetry_type,
                        data.unmapped_count,
                        data.routed_count
                    );
                }
                let mut s = stats.write().await;
                s.unmapped_points_count += data.unmapped_count as u64;
                s.last_unmapped_points = Some(data.timestamp);
            }
        },
        _ => {
            debug!("Unknown warning channel: {}", channel);
        },
    }
}
