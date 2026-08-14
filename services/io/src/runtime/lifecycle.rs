//! Runtime lifecycle management
//!
//! Provides orchestration functions for service startup, shutdown, and maintenance tasks
//! as part of the runtime orchestration layer

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use crate::core::channels::ChannelManager;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info};

// ============================================================================
// Lifecycle timing constants
// ============================================================================

/// Interval between watchdog scans. This stays well below the stale threshold
/// so a newly-stale task is discovered within one additional scan period.
const WATCHDOG_CHECK_INTERVAL: Duration = Duration::from_secs(30);

/// Interval between periodic statistics logs.
const STATISTICS_LOG_INTERVAL: Duration = Duration::from_secs(300);

/// Heartbeat timeout: if a task hasn't updated its heartbeat in this duration,
/// it's considered stuck and will be force-aborted.
const WATCHDOG_HEARTBEAT_TIMEOUT_SECS: i64 = 120;

/// Do not repeatedly submit the same unchanged stale task every scan. A failed
/// repair remains retryable after one full stale timeout.
const WATCHDOG_REPAIR_COOLDOWN_MS: i64 = WATCHDOG_HEARTBEAT_TIMEOUT_SECS * 1000;

/// Gracefully shutdown all communication channels concurrently.
/// # Lock-free channel_manager
pub async fn shutdown_handler(channel_manager: Arc<ChannelManager>) -> crate::error::Result<()> {
    info!("Starting graceful shutdown...");

    if let Err(error) = channel_manager.begin_shutdown() {
        error!("Failed to establish channel shutdown fence: {error}");
    }

    // Get all channel IDs (Direct access without RwLock)
    let channel_ids = channel_manager.get_channel_ids();

    let total_channels = channel_ids.len();
    if total_channels == 0 {
        info!("No channels to shutdown");
        drain_file_logging(channel_manager).await?;
        return Ok(());
    }

    info!("Stopping {} channels concurrently...", total_channels);

    // `remove_channel` owns its bounded graceful-stop/forced-abort sequence.
    // Do not wrap it in another timeout: cancelling that future after it has
    // taken the entry's JoinHandle could detach the task during shutdown.
    use futures::future::join_all;

    let shutdown_futures: Vec<_> = channel_ids
        .into_iter()
        .map(|channel_id| {
            let channel_manager = Arc::clone(&channel_manager);
            async move {
                match channel_manager.remove_channel(channel_id).await {
                    Ok(_) => {
                        debug!("Channel {} stopped successfully", channel_id);
                        Ok(channel_id)
                    },
                    Err(e) => {
                        error!("Error stopping channel {}: {}", channel_id, e);
                        Err((channel_id, format!("{}", e)))
                    },
                }
            }
        })
        .collect();

    // Wait for all channels to stop.
    let results = join_all(shutdown_futures).await;

    // Summarize stop results.
    let mut successful_stops = 0;
    let mut failed_stops = 0;
    let mut failed_channel_ids = Vec::new();

    for result in results {
        match result {
            Ok(_) => successful_stops += 1,
            Err((channel_id, _)) => {
                failed_stops += 1;
                failed_channel_ids.push(channel_id);
            },
        }
    }

    info!(
        "Shutdown completed: {} channels stopped successfully, {} failed",
        successful_stops, failed_stops
    );

    // A failed removal restores its entry solely so this final forced pass can
    // recover the JoinHandle. Observe every abort before the composition root
    // is allowed to take its final snapshot.
    let mut final_reconcile_failed = false;
    for channel_id in failed_channel_ids {
        if let Some(entry) = channel_manager.get_channel(channel_id)
            && let Some(handle) = entry.take_task_handle()
        {
            handle.abort();
            let _ = handle.await;
            info!("Channel {channel_id} forced shutdown observed");
        }
        if let Err(error) = channel_manager
            .reconcile_stopped_channel_commands(channel_id)
            .await
        {
            final_reconcile_failed = true;
            error!(channel_id, %error, "final stopped-channel ledger reconcile failed");
        }
        channel_manager.clear_channel_slot_after_forced_shutdown(channel_id);
    }

    drain_file_logging(Arc::clone(&channel_manager)).await?;
    if final_reconcile_failed {
        return Err(crate::error::IoError::storage(
            "one or more stopped channels retained unresolved durable command outcomes",
        ));
    }
    Ok(())
}

async fn drain_file_logging(channel_manager: Arc<ChannelManager>) -> crate::error::Result<()> {
    let result = tokio::task::spawn_blocking(move || channel_manager.shutdown_file_logging()).await;
    match result {
        Ok(Ok(())) => {
            info!("Channel file-log worker drained and flushed");
            Ok(())
        },
        Ok(Err(error)) => Err(error),
        Err(error) => Err(crate::error::IoError::resource(format!(
            "channel file-log drain task failed: {error}"
        ))),
    }
}

#[derive(Clone, Copy)]
struct WatchdogRepairAttempt {
    heartbeat_ms: i64,
    attempted_at_ms: i64,
}

fn should_attempt_watchdog_repair(
    attempts: &HashMap<u32, WatchdogRepairAttempt>,
    channel_id: u32,
    heartbeat_ms: i64,
    now_ms: i64,
) -> bool {
    attempts.get(&channel_id).is_none_or(|attempt| {
        attempt.heartbeat_ms != heartbeat_ms
            || now_ms.saturating_sub(attempt.attempted_at_ms) >= WATCHDOG_REPAIR_COOLDOWN_MS
    })
}

async fn reconcile_stale_channels(
    channel_manager: &ChannelManager,
    channel_reconciler: Option<&Arc<dyn aether_ports::ChannelReconciler>>,
    all_stats: &[crate::core::channels::ChannelStats],
    now_ms: i64,
    attempts: &mut HashMap<u32, WatchdogRepairAttempt>,
) {
    let active_ids: HashSet<_> = all_stats.iter().map(|stat| stat.channel_id).collect();
    attempts.retain(|channel_id, _| active_ids.contains(channel_id));
    let timeout_ms = WATCHDOG_HEARTBEAT_TIMEOUT_SECS * 1000;

    // This loop intentionally awaits each repair before inspecting the next
    // stale channel. The reconciler owns channel lifecycle serialization; the
    // watchdog must not create an overlapping repair fan-out around it.
    for stat in all_stats {
        if stat.watchdog_progress_tick_ms == 0 {
            continue;
        }
        let age_ms = now_ms.saturating_sub(stat.watchdog_progress_tick_ms);
        if age_ms < timeout_ms
            || !should_attempt_watchdog_repair(
                attempts,
                stat.channel_id,
                stat.watchdog_progress_tick_ms,
                now_ms,
            )
        {
            continue;
        }

        attempts.insert(
            stat.channel_id,
            WatchdogRepairAttempt {
                heartbeat_ms: stat.watchdog_progress_tick_ms,
                attempted_at_ms: now_ms,
            },
        );
        let channel = channel_manager.get_channel(stat.channel_id);
        let channel_name = channel
            .as_ref()
            .map_or("<removed>", |entry| entry.metadata.name.as_str());
        error!(
            "Ch{} ({}) watchdog: heartbeat stale for {}s, respawning task",
            stat.channel_id,
            channel_name,
            age_ms / 1000
        );
        match channel_reconciler {
            Some(reconciler) => {
                if let Err(error) = reconciler
                    .reconcile(aether_ports::ChannelReconciliationScope::One(
                        aether_domain::ChannelId::new(stat.channel_id),
                    ))
                    .await
                {
                    error!(
                        "Ch{} ({}) watchdog reconciliation failed: {}",
                        stat.channel_id, channel_name, error
                    );
                }
            },
            None => {
                error!(
                    "Ch{} ({}) watchdog repair deferred: reconciler unavailable",
                    stat.channel_id, channel_name
                );
            },
        }
    }
}

/// Run watchdog reconciliation and statistics until cancellation.
///
/// The composition root uses this unspawned form so its critical-task
/// supervisor owns the actual future and cannot accidentally detach it while
/// aborting a wrapper JoinHandle.
pub async fn run_cleanup_task(
    channel_manager: Arc<ChannelManager>,
    channel_reconciler: Option<Arc<dyn aether_ports::ChannelReconciler>>,
    task_token: CancellationToken,
) {
    let mut watchdog_interval = tokio::time::interval(WATCHDOG_CHECK_INTERVAL);
    watchdog_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut statistics_interval = tokio::time::interval(STATISTICS_LOG_INTERVAL);
    statistics_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut watchdog_attempts = HashMap::new();

    loop {
        tokio::select! {
            _ = watchdog_interval.tick() => {
                let all_stats = channel_manager.get_all_channel_stats();
                let now_ms = crate::core::channels::channel_entry::monotonic_timestamp_ms();
                reconcile_stale_channels(
                    channel_manager.as_ref(),
                    channel_reconciler.as_ref(),
                    &all_stats,
                    now_ms,
                    &mut watchdog_attempts,
                ).await;
            }
            _ = statistics_interval.tick() => {
                // Direct access without RwLock (lock-free). Statistics remain
                // low-frequency and do not set the watchdog recovery latency.
                let all_stats = channel_manager.get_all_channel_stats();
                let active_count = all_stats.iter().filter(|s| s.is_connected).count();
                let failed_count = all_stats.iter().filter(|s| s.reconnect_failed).count();

                info!(
                    "Channel stats: initialized={}, active={}, failed={}",
                    all_stats.len(),
                    active_count,
                    failed_count,
                );
            }
            () = task_token.cancelled() => {
                info!("Cleanup task received cancellation signal, shutting down");
                break;
            }
        }
    }

    info!("Cleanup task terminated");
}

#[cfg(test)]
#[allow(clippy::disallowed_methods)] // Test code - unwrap is acceptable
mod tests {
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;
    use serde_json::json;

    use super::*;
    use crate::core::channels::RuntimeChannelConfig;
    use crate::core::config::{ChannelConfig, ChannelCore, ChannelLoggingConfig};

    #[derive(Default)]
    struct RecordingReconciler {
        scopes: Mutex<Vec<aether_ports::ChannelReconciliationScope>>,
        active: AtomicUsize,
        max_active: AtomicUsize,
    }

    #[async_trait]
    impl aether_ports::ChannelReconciler for RecordingReconciler {
        async fn reconcile(
            &self,
            scope: aether_ports::ChannelReconciliationScope,
        ) -> aether_ports::PortResult<aether_ports::ChannelReconciliationReceipt> {
            let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
            self.max_active.fetch_max(active, Ordering::SeqCst);
            tokio::task::yield_now().await;
            self.scopes.lock().unwrap().push(scope);
            self.active.fetch_sub(1, Ordering::SeqCst);
            Ok(aether_ports::ChannelReconciliationReceipt::new(
                scope,
                Vec::new(),
            ))
        }
    }

    fn channel_manager() -> Arc<ChannelManager> {
        Arc::new(
            ChannelManager::new(
                crate::test_utils::create_test_shm_handle(),
                crate::test_utils::create_test_routing_cache(),
            )
            .unwrap(),
        )
    }

    async fn channel_manager_with_runtime() -> Arc<ChannelManager> {
        let manager = channel_manager();
        let config = ChannelConfig {
            core: ChannelCore {
                id: 1001,
                name: "Test Channel".to_string(),
                description: None,
                protocol: "modbus_tcp".to_string(),
                enabled: true,
            },
            parameters: HashMap::from([
                ("host".to_string(), json!("127.0.0.1")),
                ("port".to_string(), json!(502)),
            ]),
            logging: ChannelLoggingConfig::default(),
        };
        manager
            .create_channel(RuntimeChannelConfig::from_base(config))
            .unwrap();
        manager
    }

    #[tokio::test]
    async fn shutdown_removes_active_channels() {
        let manager = channel_manager_with_runtime().await;

        shutdown_handler(Arc::clone(&manager))
            .await
            .expect("shutdown succeeds");

        assert_eq!(manager.channel_count(), 0);
    }

    #[tokio::test]
    async fn shutdown_is_safe_without_channels_and_is_idempotent() {
        let manager = channel_manager();

        shutdown_handler(Arc::clone(&manager))
            .await
            .expect("first shutdown succeeds");
        shutdown_handler(manager)
            .await
            .expect("second shutdown succeeds");
    }

    #[tokio::test]
    async fn shutdown_drains_retained_file_logger_after_last_channel_is_gone() {
        let manager = channel_manager();
        let worker = manager.file_log_worker().expect("start lazy file logger");
        drop(worker);
        assert_eq!(manager.channel_count(), 0);
        assert!(
            manager
                .file_log_stats()
                .is_some_and(|stats| stats.worker_running)
        );

        shutdown_handler(Arc::clone(&manager))
            .await
            .expect("shutdown succeeds");

        assert!(
            manager
                .file_log_stats()
                .is_some_and(|stats| !stats.worker_running)
        );
        assert!(manager.file_log_worker().is_err());
    }

    #[tokio::test]
    async fn final_file_log_flush_failure_makes_service_shutdown_fail() {
        let manager = channel_manager();
        let worker =
            crate::protocols::core::file_logging::LogWorker::spawn_failing_final_flush_for_test()
                .expect("start failing file-log worker");
        worker.enqueue_record_for_test();
        manager.install_file_log_worker_for_test(worker);

        let error = shutdown_handler(manager)
            .await
            .expect_err("final flush failure must escape service shutdown");

        assert!(error.to_string().contains("synthetic final flush failure"));
    }

    #[tokio::test]
    async fn cleanup_task_stops_on_cancellation() {
        let token = CancellationToken::new();
        let handle = tokio::spawn(run_cleanup_task(channel_manager(), None, token.clone()));

        assert!(!handle.is_finished());
        token.cancel();

        tokio::time::timeout(Duration::from_secs(2), handle)
            .await
            .unwrap()
            .unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn watchdog_repairs_within_150_seconds_without_duplicate_or_parallel_submissions() {
        let manager = channel_manager();
        let recorder = Arc::new(RecordingReconciler::default());
        let reconciler: Arc<dyn aether_ports::ChannelReconciler> = recorder.clone();
        let stats = [
            crate::core::channels::ChannelStats {
                channel_id: 7,
                is_connected: true,
                watchdog_progress_tick_ms: 1,
                reconnect_failed: false,
                reconnect_total_attempts: 0,
                data_event_ingress: None,
            },
            crate::core::channels::ChannelStats {
                channel_id: 8,
                is_connected: true,
                watchdog_progress_tick_ms: 1,
                reconnect_failed: false,
                reconnect_total_attempts: 0,
                data_event_ingress: None,
            },
        ];
        let mut attempts = HashMap::new();
        let started = tokio::time::Instant::now();
        let mut interval = tokio::time::interval(WATCHDOG_CHECK_INTERVAL);
        interval.tick().await;
        let mut first_repair_elapsed = None;

        for _ in 0..5 {
            tokio::time::advance(WATCHDOG_CHECK_INTERVAL).await;
            interval.tick().await;
            let elapsed = started.elapsed();
            reconcile_stale_channels(
                manager.as_ref(),
                Some(&reconciler),
                &stats,
                1 + i64::try_from(elapsed.as_millis()).unwrap(),
                &mut attempts,
            )
            .await;
            if first_repair_elapsed.is_none() && !recorder.scopes.lock().unwrap().is_empty() {
                first_repair_elapsed = Some(elapsed);
            }
        }

        assert!(first_repair_elapsed.is_some_and(|elapsed| elapsed <= Duration::from_secs(150)));
        assert_eq!(recorder.scopes.lock().unwrap().len(), 2);
        assert_eq!(recorder.max_active.load(Ordering::SeqCst), 1);

        // Two more scans are still inside the per-heartbeat repair cooldown;
        // they must not resubmit the same stuck generation every 30 seconds.
        for _ in 0..2 {
            tokio::time::advance(WATCHDOG_CHECK_INTERVAL).await;
            interval.tick().await;
            let elapsed = started.elapsed();
            reconcile_stale_channels(
                manager.as_ref(),
                Some(&reconciler),
                &stats,
                1 + i64::try_from(elapsed.as_millis()).unwrap(),
                &mut attempts,
            )
            .await;
        }
        assert_eq!(recorder.scopes.lock().unwrap().len(), 2);
    }
}
