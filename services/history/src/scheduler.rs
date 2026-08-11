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

/// Handles for the spawned background tasks.
///
/// The flush handle is kept separately because shutdown must wait for its final
/// flush: dropping it detaches the task, and the runtime then cancels it at the
/// next await point, discarding whatever the buffer had already given up.
pub struct BackgroundTasks {
    flush: tokio::task::JoinHandle<()>,
    others: Vec<tokio::task::JoinHandle<()>>,
}

impl BackgroundTasks {
    /// Wait for the final flush, giving up after `timeout`. Returns whether it finished.
    pub async fn join_flush(self, timeout: Duration) -> bool {
        for handle in self.others {
            handle.abort();
        }
        match time::timeout(timeout, self.flush).await {
            Ok(Ok(())) => true,
            Ok(Err(e)) => {
                error!("Final flush task ended abnormally: {e}");
                false
            },
            Err(_) => {
                error!(
                    timeout_secs = timeout.as_secs(),
                    "Final flush did not finish in time; buffered points were not written"
                );
                false
            },
        }
    }
}

/// Ceiling on buffered points held for retry while storage is unavailable.
///
/// A `DataPoint` is roughly 100 bytes once its strings are counted, so this caps
/// the retry buffer in the low tens of megabytes — survivable on an edge host,
/// where the alternative is the kernel OOM-killing the whole service (taking SHM
/// acquisition down with it) after a long backend outage.
const MAX_BUFFERED_POINTS: usize = 200_000;

/// Drop the oldest points once the buffer exceeds [`MAX_BUFFERED_POINTS`],
/// returning how many were discarded.
fn enforce_buffer_cap(buffer: &mut Vec<crate::models::DataPoint>) -> usize {
    let excess = buffer.len().saturating_sub(MAX_BUFFERED_POINTS);
    if excess > 0 {
        buffer.drain(..excess);
    }
    excess
}

/// Spawn all background tasks. Each task honours the given `CancellationToken`.
pub fn spawn_all(state: Arc<AppState>, shutdown: CancellationToken) -> BackgroundTasks {
    let topology = {
        let collector = Arc::clone(&state.collector);
        let pool = state.sqlite.clone();
        let config = state.env.as_ref().clone();
        let sd = shutdown.clone();
        tokio::spawn(async move {
            collector::run_history_topology_refresh(collector, pool, config, sd).await;
        })
    };
    let collect = {
        let s = Arc::clone(&state);
        let sd = shutdown.clone();
        tokio::spawn(async move { collector_task(s, sd).await })
    };
    let flush = {
        let s = Arc::clone(&state);
        let sd = shutdown.clone();
        tokio::spawn(async move { flush_task(s, sd).await })
    };
    let cleanup = {
        let s = state;
        let sd = shutdown;
        tokio::spawn(async move { cleanup_task(s, sd).await })
    };

    BackgroundTasks {
        flush,
        others: vec![topology, collect, cleanup],
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

        // Remove stale entries for patterns that no longer exist in config.
        last_collected.retain(|k, _| cfg.subscribe_patterns.iter().any(|e| &e.pattern == k));

        let points = match finish_collection(
            &mut last_collected,
            &due,
            now,
            state.collector.collect_patterns(&cfg, &due),
        ) {
            Ok(points) => points,
            Err(error) => {
                warn!(
                    retryable = error.is_retryable(),
                    "Historical SHM batch retained for retry: {error}"
                );
                Vec::new()
            },
        };
        if !points.is_empty() {
            let mut buf = state.buffer.lock().await;
            buf.extend(points);
            let dropped = enforce_buffer_cap(&mut buf);
            if dropped > 0 {
                warn!(
                    dropped,
                    retained = buf.len(),
                    "History buffer is at its ceiling; dropped the oldest points. \
                     Storage has been unable to accept writes."
                );
            }
        }
    }
}

fn finish_collection<T>(
    last_collected: &mut HashMap<String, Instant>,
    due: &[crate::models::PatternEntry],
    now: Instant,
    result: aether_ports::PortResult<T>,
) -> aether_ports::PortResult<T> {
    let value = result?;
    for entry in due {
        last_collected.insert(entry.pattern.clone(), now);
    }
    Ok(value)
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
        let dropped = enforce_buffer_cap(&mut buf);
        if dropped > 0 {
            warn!(
                dropped,
                retained = buf.len(),
                "History retry buffer is at its ceiling; dropped the oldest points."
            );
        }
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
    use std::collections::HashMap;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use aether_ports::{PortError, PortErrorKind};
    use tokio::time::{Duration, Instant};

    use super::BackgroundTasks;

    use crate::models::{DataPoint, PatternEntry};

    use super::{MAX_BUFFERED_POINTS, enforce_buffer_cap, finish_collection, storage_should_run};

    fn point(seq: usize) -> DataPoint {
        DataPoint {
            time: chrono::Utc::now(),
            series_key: "inst:1:M".to_string(),
            point_id: seq.to_string(),
            value: Some(seq as f64),
            string_value: None,
        }
    }

    #[tokio::test]
    async fn shutdown_waits_for_the_final_flush_to_finish() {
        // The bug: `spawn_all` dropped every JoinHandle, so `main` returned as soon
        // as axum drained and the runtime was torn down mid-flush.
        let flushed = Arc::new(AtomicBool::new(false));
        let marker = Arc::clone(&flushed);
        let flush = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            marker.store(true, Ordering::SeqCst);
        });

        let tasks = BackgroundTasks {
            flush,
            others: Vec::new(),
        };
        assert!(tasks.join_flush(Duration::from_secs(5)).await);
        assert!(
            flushed.load(Ordering::SeqCst),
            "shutdown must not return before the final flush has written"
        );
    }

    #[tokio::test]
    async fn shutdown_gives_up_on_a_flush_that_will_not_finish() {
        let flush = tokio::spawn(std::future::pending::<()>());

        let tasks = BackgroundTasks {
            flush,
            others: Vec::new(),
        };
        assert!(!tasks.join_flush(Duration::from_millis(50)).await);
    }

    #[test]
    fn buffer_below_the_ceiling_is_left_alone() {
        let mut buf: Vec<DataPoint> = (0..16).map(point).collect();

        assert_eq!(enforce_buffer_cap(&mut buf), 0);
        assert_eq!(buf.len(), 16);
    }

    #[test]
    fn buffer_over_the_ceiling_drops_the_oldest_points() {
        // A storage backend that keeps failing pushes every batch back into the
        // buffer; without a ceiling this grows until the edge host is OOM-killed.
        let mut buf: Vec<DataPoint> = (0..MAX_BUFFERED_POINTS + 500).map(point).collect();

        let dropped = enforce_buffer_cap(&mut buf);

        assert_eq!(dropped, 500);
        assert_eq!(buf.len(), MAX_BUFFERED_POINTS);
        // The newest samples are the ones worth keeping.
        assert_eq!(buf[0].point_id, "500");
        assert_eq!(
            buf[buf.len() - 1].point_id,
            (MAX_BUFFERED_POINTS + 499).to_string()
        );
    }

    #[test]
    fn configured_but_disconnected_storage_does_not_fill_the_buffer() {
        assert!(!storage_should_run(true, "disabled"));
        assert!(!storage_should_run(false, "sqlite"));
        assert!(storage_should_run(true, "sqlite"));
        assert!(storage_should_run(true, "postgres"));
    }

    #[test]
    fn failed_collection_does_not_advance_any_due_pattern() {
        let mut last_collected = HashMap::new();
        let due = vec![PatternEntry::new("inst:*:M"), PatternEntry::new("io:*:T")];
        let now = Instant::now();

        let result = finish_collection::<()>(
            &mut last_collected,
            &due,
            now,
            Err(PortError::new(
                PortErrorKind::Unavailable,
                "injected batch failure",
            )),
        );

        assert!(result.is_err());
        assert!(last_collected.is_empty());

        finish_collection(&mut last_collected, &due, now, Ok(()))
            .expect("successful batch advances all due selectors");
        assert_eq!(last_collected.len(), 2);
    }
}
