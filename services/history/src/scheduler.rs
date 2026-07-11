/// Background tasks:
/// - **collector_task** – ticks every second, checks which patterns are due
///   (each pattern may have its own interval), and appends data points to
///   the shared buffer.
/// - **flush_task** – drains the buffer every `flush_interval_secs` and
///   writes to storage in batches.
/// - **cleanup_task** – runs daily at approximately 02:00 UTC and removes
///   data older than `cleanup_older_than_days`.
use std::collections::HashMap;
use std::sync::Arc;

use chrono::Utc;
use tokio::time::{self, Duration, Instant};
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

use crate::collector;
use crate::state::AppState;

/// Spawn all background tasks. Each task honours the given `CancellationToken`.
pub fn spawn_all(state: Arc<AppState>, shutdown: CancellationToken) {
    {
        let s = Arc::clone(&state);
        let sd = shutdown.clone();
        tokio::spawn(async move { collector_task(s, sd).await });
    }
    {
        let s = Arc::clone(&state);
        let sd = shutdown.clone();
        tokio::spawn(async move { flush_task(s, sd).await });
    }
    {
        let s = state;
        let sd = shutdown;
        tokio::spawn(async move { cleanup_task(s, sd).await });
    }
}

async fn collector_task(state: Arc<AppState>, shutdown: CancellationToken) {
    // last_collected: pattern → Instant of most recent collection
    let mut last_collected: HashMap<String, Instant> = HashMap::new();

    loop {
        // Tick every second – lightweight; just looks up a few HashMap entries.
        tokio::select! {
            _ = time::sleep(Duration::from_secs(1)) => {}
            _ = shutdown.cancelled() => {
                info!("Collector task shutting down");
                return;
            }
        }

        if !storage_is_active(&state).await {
            continue;
        }

        let cfg = {
            let guard = state.config.read().await;
            guard.clone()
        };
        let default_interval = cfg.collection_interval_secs;
        let now = Instant::now();

        // Determine which patterns are due for collection this tick.
        let due: Vec<_> = cfg
            .subscribe_patterns
            .iter()
            .filter(|entry| {
                let interval = entry.effective_interval(default_interval);
                match last_collected.get(&entry.pattern) {
                    None => true, // never collected → immediately due
                    Some(t) => now.duration_since(*t).as_secs() >= interval,
                }
            })
            .cloned()
            .collect();

        if due.is_empty() {
            continue;
        }

        // Stamp collection time before the (async) collect so we don't drift.
        for entry in &due {
            last_collected.insert(entry.pattern.clone(), now);
        }

        // Remove stale entries for patterns that no longer exist in config.
        last_collected.retain(|k, _| cfg.subscribe_patterns.iter().any(|e| &e.pattern == k));

        let points = match collector::build_shm_history_collector(&state.sqlite, &state.env).await {
            Ok(collector) => collector.collect_patterns(&cfg, &due),
            Err(error) => {
                warn!("Cannot rebuild historical SHM catalogue: {error}");
                Vec::new()
            },
        };
        if !points.is_empty() {
            let mut buf = state.buffer.lock().await;
            buf.extend(points);
        }
    }
}

async fn flush_task(state: Arc<AppState>, shutdown: CancellationToken) {
    loop {
        let interval = {
            let cfg = state.config.read().await;
            cfg.flush_interval_secs
        };

        tokio::select! {
            _ = time::sleep(Duration::from_secs(interval)) => {}
            _ = shutdown.cancelled() => {
                // Final flush before exit
                flush_buffer(&state).await;
                info!("Flush task shutting down");
                return;
            }
        }

        flush_buffer(&state).await;
    }
}

async fn flush_buffer(state: &AppState) {
    let batch_size = state.config.read().await.batch_size;
    if !storage_is_active(state).await {
        return;
    }

    let points = {
        let mut buf = state.buffer.lock().await;
        if buf.is_empty() {
            return;
        }
        std::mem::take(&mut *buf)
    };

    let backend = state.storage.read().await.clone();
    let total = points.len();
    let mut failed: Vec<_> = Vec::new();
    for chunk in points.chunks(batch_size) {
        match backend.write_batch(chunk.to_vec()).await {
            Ok(n) => info!("Flushed {} data points to {}", n, backend.name()),
            Err(e) => {
                error!(
                    "Flush failed, {} points will be retried: {}",
                    chunk.len(),
                    e
                );
                failed.extend_from_slice(chunk);
            },
        }
    }
    // Put failed points back at the front of the buffer so they are retried next cycle.
    let failed_count = failed.len();
    if failed_count > 0 {
        let mut buf = state.buffer.lock().await;
        failed.extend(buf.drain(..));
        *buf = failed;
    }
    info!(
        "Flush complete: {}/{} points written",
        total - failed_count,
        total
    );
}

async fn cleanup_task(state: Arc<AppState>, shutdown: CancellationToken) {
    loop {
        // Wait until approximately 02:00 UTC next day
        let sleep_secs = secs_until_02_utc();

        tokio::select! {
            _ = time::sleep(Duration::from_secs(sleep_secs)) => {}
            _ = shutdown.cancelled() => {
                info!("Cleanup task shutting down");
                return;
            }
        }

        let (cleanup_enabled, days) = {
            let cfg = state.config.read().await;
            (cfg.cleanup_enabled, cfg.cleanup_older_than_days)
        };

        if !cleanup_enabled || !storage_is_active(&state).await {
            continue;
        }

        let backend = state.storage.read().await.clone();
        match backend.cleanup_old_data(days).await {
            Ok(n) => info!("Cleanup: removed {} rows older than {} days", n, days),
            Err(e) => warn!("Cleanup failed: {}", e),
        }
    }
}

async fn storage_is_active(state: &AppState) -> bool {
    let enabled = state.storage_settings.read().await.enabled;
    if !enabled {
        return false;
    }
    let backend = state.storage.read().await;
    storage_should_run(enabled, backend.name())
}

fn storage_should_run(enabled: bool, active_backend: &str) -> bool {
    enabled && active_backend != "disabled"
}

/// How many seconds until the next 02:00 UTC (minimum 60s to avoid tight loops).
fn secs_until_02_utc() -> u64 {
    let now = Utc::now();
    let Some(today_02_naive) = now.date_naive().and_hms_opt(2, 0, 0) else {
        return 60;
    };
    let today_02: chrono::DateTime<Utc> = today_02_naive.and_utc();

    let target = if now < today_02 {
        today_02
    } else {
        today_02 + chrono::Duration::days(1)
    };

    (target - now).num_seconds().max(60) as u64
}

#[cfg(test)]
mod tests {
    use super::storage_should_run;

    #[test]
    fn configured_but_disconnected_storage_does_not_fill_the_buffer() {
        assert!(!storage_should_run(true, "disabled"));
        assert!(!storage_should_run(false, "sqlite"));
        assert!(storage_should_run(true, "sqlite"));
        assert!(storage_should_run(true, "postgres"));
    }
}
