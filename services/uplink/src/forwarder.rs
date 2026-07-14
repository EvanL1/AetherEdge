/// Periodic data forwarding: SHM → durable outbox → MQTT property topic.
///
/// Two background tasks:
/// - `run_data_forwarder` – samples configured SHM groups every
///   `report_interval_secs` and
///   publishes to the property topic.
/// - `run_system_monitor` – collects system metrics every
///   `system_monitor_interval_secs` and publishes them.
///
/// `upload_once` is called directly by the call-data handler for on-demand
/// uploads.
use std::sync::Arc;

use chrono::Utc;
use regex::Regex;
use tokio::time::{self, Duration};
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, warn};

use crate::models::{PropertyEntry, PropertyPayload};
use crate::state::AppState;
use crate::system_monitor;

// ── Public entry points ───────────────────────────────────────────────────────

pub async fn run_data_forwarder(state: Arc<AppState>, shutdown: CancellationToken) {
    loop {
        let interval = state.config.read().await.report_interval_secs;

        tokio::select! {
            _ = time::sleep(Duration::from_secs(interval)) => {}
            _ = shutdown.cancelled() => return,
        }

        upload_once(Arc::clone(&state)).await;
    }
}

pub async fn run_system_monitor(state: Arc<AppState>, shutdown: CancellationToken) {
    loop {
        let (enabled, interval) = {
            let cfg = state.config.read().await;
            (cfg.system_monitor_enabled, cfg.system_monitor_interval_secs)
        };

        tokio::select! {
            _ = time::sleep(Duration::from_secs(interval)) => {}
            _ = shutdown.cancelled() => return,
        }

        if !enabled {
            continue;
        }

        let metrics = system_monitor::collect();
        let device_sn = state.device.device_sn.clone();

        let entry = PropertyEntry {
            source: "gateway".to_string(),
            device: device_sn,
            data_type: "T".to_string(),
            value: serde_json::from_value(serde_json::to_value(&metrics).unwrap_or_default())
                .unwrap_or_default(),
        };

        let payload = PropertyPayload {
            timestamp: Utc::now().timestamp(),
            property: vec![entry],
        };

        if let Err(e) = publish_payload(&state, payload).await {
            warn!("System monitor upload failed: {}", e);
        }
    }
}

/// Triggered by `call-data` MQTT command or on timer tick.
pub async fn upload_once(state: Arc<AppState>) {
    let (patterns, excludes, batch_size) = {
        let cfg = state.config.read().await;
        (
            cfg.subscribe_patterns.clone(),
            cfg.exclude_patterns.clone(),
            cfg.report_batch_size,
        )
    };

    let exclude_res: Vec<Regex> = excludes.iter().filter_map(|p| Regex::new(p).ok()).collect();

    // Pin exactly one SQLite-routing + committed-SHM generation for the whole
    // collection pass. A concurrent refresh may serve the next pass only.
    let generation = state.live_topology.load();
    let entries = match generation.collect_entries(&patterns, &exclude_res) {
        Ok(entries) => entries,
        Err(error) => {
            warn!(
                retryable = error.is_retryable(),
                topology_digest = generation.digest(),
                publication_epoch = generation.publication_epoch(),
                "SHM property collection failed: {error}"
            );
            Vec::new()
        },
    };
    if entries.is_empty() {
        debug!("No data to upload");
        return;
    }

    // Publish in batches
    for chunk in entries.chunks(batch_size) {
        let payload = PropertyPayload {
            timestamp: Utc::now().timestamp(),
            property: chunk.to_vec(),
        };
        if let Err(e) = publish_payload(&state, payload).await {
            error!("Data upload failed: {}", e);
        }
    }
}

// ── Internal helpers ──────────────────────────────────────────────────────────

async fn publish_payload(state: &AppState, payload: PropertyPayload) -> anyhow::Result<()> {
    crate::uplink::enqueue_json(state, &state.topics.property, &payload)
        .await
        .map(|_| ())
}
