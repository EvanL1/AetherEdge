/// Background tasks:
/// - **collector_task** – ticks every second, checks which patterns are due
///   (each pattern may have its own interval), and appends data points to
///   the shared buffer.
/// - **flush_task** – drains the buffer every `flush_interval_secs` and
///   writes to storage in batches.
/// - **cleanup_task** – runs daily at approximately 02:00 UTC and removes
///   data older than `cleanup_older_than_days`.
use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use chrono::Utc;
use tokio::time::{self, Duration, Instant};
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

use crate::collector;
use crate::state::AppState;

/// Handle for the critical-task supervisor.
///
/// The supervisor makes an unexpected collector/topology/cleanup/flush exit
/// cancel the service, and drains the final flush during normal shutdown.
pub struct BackgroundTasks {
    supervisor: tokio::task::JoinHandle<anyhow::Result<()>>,
    final_flush_complete: Arc<AtomicBool>,
}

impl BackgroundTasks {
    /// Wait for the final flush, giving up after `timeout`. Returns whether it finished.
    pub async fn join_flush(self, timeout: Duration) -> bool {
        let mut supervisor = self.supervisor;
        match time::timeout(timeout, &mut supervisor).await {
            Ok(Ok(Ok(()))) => {
                let complete = self.final_flush_complete.load(Ordering::Acquire);
                if !complete {
                    error!("History final flush left unwritten buffered points");
                }
                complete
            },
            Ok(Ok(Err(error))) => {
                error!("History background task failed: {error}");
                false
            },
            Ok(Err(error)) => {
                error!("History task supervisor ended abnormally: {error}");
                false
            },
            Err(_) => {
                error!(
                    timeout_secs = timeout.as_secs(),
                    "Final flush did not finish in time; buffered points were not written"
                );
                // Dropping a JoinHandle detaches it. Explicitly abort and
                // observe the supervisor so no history writer can outlive the
                // service's declared shutdown deadline.
                supervisor.abort();
                let _ = supervisor.await;
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
const MAX_FLUSH_POINTS_PER_CYCLE: usize = 50_000;
const STORAGE_WRITE_TIMEOUT: Duration = Duration::from_secs(10);

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
    let final_flush_complete = Arc::new(AtomicBool::new(false));
    let mut supervisor = common::task_supervisor::CriticalTaskSupervisor::new(Duration::from_secs(
        crate::FINAL_FLUSH_TIMEOUT_SECS.saturating_sub(1).max(1),
    ));
    {
        let collector = Arc::clone(&state.collector);
        let pool = state.sqlite.clone();
        let config = state.env.as_ref().clone();
        let sd = shutdown.clone();
        supervisor.spawn("history-topology-refresh", async move {
            collector::run_history_topology_refresh(collector, pool, config, sd).await;
        });
    }
    {
        let s = Arc::clone(&state);
        let sd = shutdown.clone();
        supervisor.spawn("history-collector", async move {
            collector_task(s, sd).await;
        });
    }
    {
        let s = Arc::clone(&state);
        let sd = shutdown.clone();
        let flush_complete = Arc::clone(&final_flush_complete);
        supervisor.spawn("history-flush", async move {
            flush_task(s, sd, flush_complete).await;
        });
    }
    {
        let s = state;
        let sd = shutdown.clone();
        supervisor.spawn("history-cleanup", async move {
            cleanup_task(s, sd).await;
        });
    }
    let supervisor_task = tokio::spawn(async move { supervisor.run(shutdown).await });

    BackgroundTasks {
        supervisor: supervisor_task,
        final_flush_complete,
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
            // Cancellation and this check are ordered with the final flush by
            // the same buffer mutex. If collection finishes after shutdown,
            // it must not append behind a flush that already observed an empty
            // buffer and exited.
            let dropped = match append_if_running(&mut buf, points, &shutdown) {
                Ok(dropped) => dropped,
                Err(discarded) => {
                    info!(
                        discarded,
                        "Collector discarded a post-cancellation sample batch"
                    );
                    return;
                },
            };
            if dropped > 0 {
                state.runtime_metrics.record_dropped(dropped);
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

/// Appends one collected batch while the caller holds the shared buffer lock.
///
/// The lock plus the cancellation check form the ordering fence with the final
/// flush. `Err(n)` means shutdown won and `n` points were deliberately not
/// appended; `Ok(n)` reports points dropped by the bounded-buffer policy.
fn append_if_running(
    buffer: &mut Vec<crate::models::DataPoint>,
    points: Vec<crate::models::DataPoint>,
    shutdown: &CancellationToken,
) -> Result<usize, usize> {
    if shutdown.is_cancelled() {
        return Err(points.len());
    }
    buffer.extend(points);
    Ok(enforce_buffer_cap(buffer))
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

async fn flush_task(
    state: Arc<AppState>,
    shutdown: CancellationToken,
    final_flush_complete: Arc<AtomicBool>,
) {
    loop {
        let interval = {
            let cfg = state.config.read().await;
            cfg.flush_interval_secs
        };

        tokio::select! {
            _ = time::sleep(Duration::from_secs(interval)) => {}
            _ = shutdown.cancelled() => {
                // A periodic cycle is deliberately capped, but shutdown must
                // continue through every successful cycle so a full retry
                // buffer is not truncated to the first 50k points.
                let outcome = drain_final_buffer(&state).await;
                if outcome.remaining > 0 {
                    warn!(
                        remaining = outcome.remaining,
                        "Final history flush stopped with unwritten buffered points"
                    );
                }
                final_flush_complete.store(outcome.remaining == 0, Ordering::Release);
                info!("Flush task shutting down");
                return;
            }
        }

        flush_buffer(&state).await;
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct FlushCycleOutcome {
    written: usize,
    remaining: usize,
    blocked: bool,
}

async fn drain_final_buffer(state: &AppState) -> FlushCycleOutcome {
    drain_final_buffer_with(|| flush_buffer(state)).await
}

type TimedStorageWrite = Result<anyhow::Result<usize>, time::error::Elapsed>;

/// Own one atomic write attempt while retaining the exact admitted identities
/// for an ambiguous-result retry. This helper is intentionally the sole place
/// the in-flight batch is cloned.
async fn attempt_storage_write<F, Fut>(
    batch: Vec<crate::models::DataPoint>,
    timeout: Duration,
    write: F,
) -> (TimedStorageWrite, Vec<crate::models::DataPoint>)
where
    F: FnOnce(Vec<crate::models::DataPoint>) -> Fut,
    Fut: Future<Output = anyhow::Result<usize>>,
{
    let retry = batch.clone();
    let result = time::timeout(timeout, write(batch)).await;
    (result, retry)
}

async fn drain_final_buffer_with<F, Fut>(mut flush_once: F) -> FlushCycleOutcome
where
    F: FnMut() -> Fut,
    Fut: Future<Output = FlushCycleOutcome>,
{
    loop {
        let outcome = flush_once().await;
        if outcome.remaining == 0 || outcome.blocked || outcome.written == 0 {
            return outcome;
        }
    }
}

async fn flush_buffer(state: &AppState) -> FlushCycleOutcome {
    let flush_started = Instant::now();
    let batch_size = state
        .config
        .read()
        .await
        .batch_size
        .clamp(1, crate::models::MAX_WRITE_BATCH_POINTS);
    if !storage_is_active(state).await {
        return FlushCycleOutcome {
            remaining: state.buffer.lock().await.len(),
            blocked: true,
            ..FlushCycleOutcome::default()
        };
    }

    let backend = state.storage.read().await.clone();
    let mut attempted = 0usize;
    let mut written = 0usize;
    let mut failed_count = 0usize;
    let mut blocked = false;

    while attempted < MAX_FLUSH_POINTS_PER_CYCLE {
        // Drain only one bounded write batch. While it is in flight the
        // collector can fill the shared buffer, but total resident data stays
        // bounded by MAX_BUFFERED_POINTS plus at most two write batches (the
        // owned call and its retry copy).
        let batch = {
            let mut buffer = state.buffer.lock().await;
            let take = buffer
                .len()
                .min(batch_size)
                .min(MAX_FLUSH_POINTS_PER_CYCLE - attempted);
            if take == 0 {
                break;
            }
            buffer.drain(..take).collect::<Vec<_>>()
        };
        let batch_len = batch.len();
        attempted += batch_len;

        let (result, retry) = attempt_storage_write(batch, STORAGE_WRITE_TIMEOUT, |batch| {
            backend.write_batch(batch)
        })
        .await;
        match result {
            Ok(Ok(count)) if count == batch_len => {
                written += count;
                state.runtime_metrics.record_written(count);
                info!("Flushed {} data points to {}", count, backend.name());
            },
            Ok(Ok(count)) => {
                state.runtime_metrics.record_flush_failure();
                error!(
                    expected = batch_len,
                    written = count,
                    backend = backend.name(),
                    "Storage backend reported a partial atomic batch; retaining it for retry"
                );
                failed_count = batch_len;
                requeue_failed_batch(state, retry).await;
                blocked = true;
                break;
            },
            Ok(Err(error)) => {
                state.runtime_metrics.record_flush_failure();
                error!(
                    points = batch_len,
                    backend = backend.name(),
                    "Flush failed; points will be retried: {error}"
                );
                failed_count = batch_len;
                requeue_failed_batch(state, retry).await;
                blocked = true;
                break;
            },
            Err(_) => {
                state.runtime_metrics.record_flush_timeout();
                error!(
                    points = batch_len,
                    backend = backend.name(),
                    timeout_secs = STORAGE_WRITE_TIMEOUT.as_secs(),
                    "Flush timed out; points will be retried"
                );
                failed_count = batch_len;
                requeue_failed_batch(state, retry).await;
                blocked = true;
                break;
            },
        }
    }
    info!(
        attempted,
        written,
        failed = failed_count,
        "Flush cycle complete"
    );
    state
        .runtime_metrics
        .record_flush_duration(flush_started.elapsed());
    FlushCycleOutcome {
        written,
        remaining: state.buffer.lock().await.len(),
        blocked,
    }
}

async fn requeue_failed_batch(state: &AppState, mut failed: Vec<crate::models::DataPoint>) {
    let mut buffer = state.buffer.lock().await;
    failed.extend(buffer.drain(..));
    *buffer = failed;
    let dropped = enforce_buffer_cap(&mut buffer);
    if dropped > 0 {
        state.runtime_metrics.record_dropped(dropped);
        warn!(
            dropped,
            retained = buffer.len(),
            "History retry buffer is at its ceiling; dropped the oldest points."
        );
    }
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
    use std::collections::{HashMap, VecDeque};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};

    use aether_ports::{PortError, PortErrorKind};
    use tokio::time::{Duration, Instant};
    use tokio_util::sync::CancellationToken;

    use super::BackgroundTasks;

    use crate::backend_sqlite::SqliteHistoryBackend;
    use crate::models::{DataPoint, PatternEntry};
    use crate::storage::StorageBackend;

    use super::{
        FlushCycleOutcome, MAX_BUFFERED_POINTS, append_if_running, attempt_storage_write,
        drain_final_buffer_with, enforce_buffer_cap, finish_collection, storage_should_run,
    };

    struct DropMarker(Arc<AtomicBool>);

    impl Drop for DropMarker {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    fn point(seq: usize) -> DataPoint {
        DataPoint::new(
            chrono::Utc::now(),
            "inst:1:M",
            seq.to_string(),
            Some(seq as f64),
            None,
        )
    }

    #[tokio::test]
    async fn ambiguous_commit_retry_preserves_the_admitted_identity_end_to_end() {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("open embedded history database");
        let inspect_pool = pool.clone();
        let backend = SqliteHistoryBackend::new(pool);
        backend.init_schema().await.expect("initialize schema");
        let batch = vec![point(7)];
        let identity = batch[0].ingestion_id();

        let (first, retry) = attempt_storage_write(batch, Duration::from_secs(1), |batch| {
            let backend = &backend;
            async move {
                backend.write_batch(batch).await?;
                anyhow::bail!("simulated lost commit acknowledgement")
            }
        })
        .await;
        assert!(matches!(first, Ok(Err(_))));
        assert_eq!(retry.len(), 1);
        assert_eq!(retry[0].ingestion_id(), identity);

        let (second, _) = attempt_storage_write(retry, Duration::from_secs(1), |batch| {
            backend.write_batch(batch)
        })
        .await;
        assert!(matches!(second, Ok(Ok(1))));
        let stored: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM history")
            .fetch_one(&inspect_pool)
            .await
            .expect("count deduplicated retry");
        assert_eq!(stored, 1);
    }

    #[tokio::test]
    async fn shutdown_waits_for_the_final_flush_to_finish() {
        // The bug: `spawn_all` dropped every JoinHandle, so `main` returned as soon
        // as axum drained and the runtime was torn down mid-flush.
        let flushed = Arc::new(AtomicBool::new(false));
        let marker = Arc::clone(&flushed);
        let shutdown = CancellationToken::new();
        let task_shutdown = shutdown.clone();
        let mut supervisor =
            common::task_supervisor::CriticalTaskSupervisor::new(Duration::from_secs(1));
        supervisor.spawn("flush", async move {
            task_shutdown.cancelled().await;
            tokio::time::sleep(Duration::from_millis(50)).await;
            marker.store(true, Ordering::SeqCst);
        });
        let tasks = BackgroundTasks {
            supervisor: tokio::spawn(supervisor.run(shutdown.clone())),
            final_flush_complete: Arc::new(AtomicBool::new(true)),
        };
        shutdown.cancel();
        assert!(tasks.join_flush(Duration::from_secs(5)).await);
        assert!(
            flushed.load(Ordering::SeqCst),
            "shutdown must not return before the final flush has written"
        );
    }

    #[tokio::test]
    async fn shutdown_gives_up_on_a_flush_that_will_not_finish() {
        let shutdown = CancellationToken::new();
        let dropped = Arc::new(AtomicBool::new(false));
        let marker = DropMarker(Arc::clone(&dropped));
        let mut supervisor =
            common::task_supervisor::CriticalTaskSupervisor::new(Duration::from_secs(60));
        supervisor.spawn("flush", async move {
            let _marker = marker;
            std::future::pending::<()>().await;
        });
        let tasks = BackgroundTasks {
            supervisor: tokio::spawn(supervisor.run(shutdown.clone())),
            final_flush_complete: Arc::new(AtomicBool::new(false)),
        };
        shutdown.cancel();
        assert!(!tasks.join_flush(Duration::from_millis(50)).await);
        assert!(
            dropped.load(Ordering::SeqCst),
            "timed-out background work must be aborted and observed, not detached"
        );
    }

    #[tokio::test]
    async fn unwritten_final_buffer_is_not_reported_as_a_clean_shutdown() {
        let tasks = BackgroundTasks {
            supervisor: tokio::spawn(async { Ok(()) }),
            final_flush_complete: Arc::new(AtomicBool::new(false)),
        };

        assert!(!tasks.join_flush(Duration::from_secs(1)).await);
    }

    #[tokio::test]
    async fn final_flush_repeats_successful_bounded_cycles_until_empty() {
        let outcomes = Arc::new(Mutex::new(VecDeque::from([
            FlushCycleOutcome {
                written: 50_000,
                remaining: 150_000,
                blocked: false,
            },
            FlushCycleOutcome {
                written: 50_000,
                remaining: 100_000,
                blocked: false,
            },
            FlushCycleOutcome {
                written: 50_000,
                remaining: 50_000,
                blocked: false,
            },
            FlushCycleOutcome {
                written: 50_000,
                remaining: 0,
                blocked: false,
            },
        ])));
        let scripted = Arc::clone(&outcomes);

        let outcome = drain_final_buffer_with(move || {
            let outcome = scripted
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .pop_front()
                .expect("one scripted flush outcome");
            std::future::ready(outcome)
        })
        .await;

        assert_eq!(outcome.remaining, 0);
        assert!(
            outcomes
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .is_empty(),
            "all four bounded cycles must run"
        );
    }

    #[tokio::test]
    async fn final_flush_stops_after_a_blocked_cycle_without_spinning() {
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let observed = Arc::clone(&calls);

        let outcome = drain_final_buffer_with(move || {
            observed.fetch_add(1, Ordering::SeqCst);
            std::future::ready(FlushCycleOutcome {
                written: 0,
                remaining: 100_000,
                blocked: true,
            })
        })
        .await;

        assert_eq!(outcome.remaining, 100_000);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn a_post_cancellation_collection_cannot_append_behind_the_final_flush() {
        let shutdown = CancellationToken::new();
        shutdown.cancel();
        let mut buffer = vec![point(1)];

        let result = append_if_running(&mut buffer, vec![point(2), point(3)], &shutdown);

        assert_eq!(result, Err(2));
        assert_eq!(buffer.len(), 1);
        assert_eq!(buffer[0].point_id, "1");
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
