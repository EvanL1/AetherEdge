//! Runtime lifecycle management
//!
//! Provides orchestration functions for service startup, shutdown, and maintenance tasks
//! as part of the runtime orchestration layer

use std::time::Duration;

use crate::core::channels::ChannelManager;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info};

// ============================================================================
// Lifecycle timing constants
// ============================================================================

/// Per-channel timeout during graceful shutdown
const CHANNEL_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);

/// Interval between periodic cleanup/statistics cycles
const CLEANUP_INTERVAL: Duration = Duration::from_secs(300);

/// Heartbeat timeout: if a task hasn't updated its heartbeat in this duration,
/// it's considered stuck and will be force-aborted.
const WATCHDOG_HEARTBEAT_TIMEOUT_SECS: i64 = 120;

/// Overall timeout for the service shutdown sequence
const SERVICE_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(30);

/// Gracefully shutdown all communication channels concurrently with per-channel timeout.
/// # Lock-free channel_manager
pub async fn shutdown_handler(channel_manager: Arc<ChannelManager>) {
    info!("Starting graceful shutdown...");

    // Get all channel IDs (Direct access without RwLock)
    let channel_ids = channel_manager.get_channel_ids();

    let total_channels = channel_ids.len();
    if total_channels == 0 {
        info!("No channels to shutdown");
        return;
    }

    info!("Stopping {} channels concurrently...", total_channels);

    // Stop all channels concurrently with per-channel timeout
    use futures::future::join_all;

    let shutdown_futures: Vec<_> = channel_ids
        .into_iter()
        .map(|channel_id| {
            let channel_manager = Arc::clone(&channel_manager);
            async move {
                // Direct access without RwLock (lock-free)
                // Add timeout to prevent single channel from blocking entire shutdown
                let result = tokio::time::timeout(
                    CHANNEL_SHUTDOWN_TIMEOUT,
                    channel_manager.remove_channel(channel_id),
                )
                .await;

                match result {
                    Ok(Ok(_)) => {
                        debug!("Channel {} stopped successfully", channel_id);
                        Ok(channel_id)
                    },
                    Ok(Err(e)) => {
                        error!("Error stopping channel {}: {}", channel_id, e);
                        Err((channel_id, format!("{}", e)))
                    },
                    Err(_) => {
                        error!(
                            "Channel {} shutdown timed out after {:?}",
                            channel_id, CHANNEL_SHUTDOWN_TIMEOUT
                        );
                        Err((channel_id, "timeout".to_string()))
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

    for result in results {
        match result {
            Ok(_) => successful_stops += 1,
            Err(_) => failed_stops += 1,
        }
    }

    info!(
        "Shutdown completed: {} channels stopped successfully, {} failed",
        successful_stops, failed_stops
    );
}

/// Start a periodic background task that logs channel statistics every 5 minutes.
///
/// Returns `(JoinHandle, CancellationToken)` for task lifecycle management.
/// # Lock-free channel_manager
pub fn start_cleanup_task(
    channel_manager: Arc<ChannelManager>,
    channel_reconciler: Option<Arc<dyn aether_ports::ChannelReconciler>>,
) -> (tokio::task::JoinHandle<()>, CancellationToken) {
    let token = CancellationToken::new();
    let task_token = token.clone();

    let handle = tokio::spawn(async move {
        let mut interval = tokio::time::interval(CLEANUP_INTERVAL);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        loop {
            tokio::select! {
                _ = interval.tick() => {
                    // Direct access without RwLock (lock-free)

                    // Log statistics
                    let all_stats = channel_manager.get_all_channel_stats();
                    let now_ms = crate::core::channels::channel_entry::unix_timestamp_ms();
                    let timeout_ms = WATCHDOG_HEARTBEAT_TIMEOUT_SECS * 1000;

                    // Watchdog submits repair through the same reconciler that owns
                    // CRUD/reload lifecycle serialization. It never revives a cached
                    // runtime configuration directly.
                    for stat in &all_stats {
                        // Skip channels that haven't started yet (heartbeat = 0)
                        if stat.watchdog_heartbeat_ms == 0 {
                            continue;
                        }
                        let age_ms = now_ms - stat.watchdog_heartbeat_ms;
                        if age_ms > timeout_ms {
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
                            match &channel_reconciler {
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
    });

    (handle, token)
}

/// Wait for shutdown signal (Ctrl+C or SIGTERM on Unix)
///
/// Re-exports the common shutdown handler for backwards compatibility.
pub async fn wait_for_shutdown() {
    common::shutdown::wait_for_shutdown().await
}

/// Perform graceful shutdown of all services
///
/// # Lock-free channel_manager
pub async fn shutdown_services(
    channel_manager: Arc<ChannelManager>,
    shutdown_token: CancellationToken,
    cleanup_token: CancellationToken,
    cleanup_handle: tokio::task::JoinHandle<()>,
    server_handle: tokio::task::JoinHandle<()>,
) {
    info!("Received shutdown signal, starting graceful shutdown...");

    // First shutdown the communication channels
    shutdown_handler(channel_manager).await;

    // Signal all tasks to shutdown
    shutdown_token.cancel();

    // Cancel cleanup task
    cleanup_token.cancel();
    cleanup_handle.abort();

    // Wait for tasks with timeout
    let shutdown_timeout = SERVICE_SHUTDOWN_TIMEOUT;

    // Wait for server task
    match tokio::time::timeout(shutdown_timeout, server_handle).await {
        Ok(Ok(())) => info!("Server shut down gracefully"),
        Ok(Err(e)) => error!("Server task failed: {}", e),
        Err(_) => error!("Server shutdown timed out"),
    }

    info!("Service shutdown complete");
}
#[cfg(test)]
#[allow(clippy::disallowed_methods)] // Test code - unwrap is acceptable
mod tests {
    use std::collections::HashMap;
    use std::sync::Arc;

    use serde_json::json;

    use super::*;
    use crate::core::channels::RuntimeChannelConfig;
    use crate::core::config::{ChannelConfig, ChannelCore, ChannelLoggingConfig};

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

        shutdown_handler(Arc::clone(&manager)).await;

        assert_eq!(manager.channel_count(), 0);
    }

    #[tokio::test]
    async fn shutdown_is_safe_without_channels_and_is_idempotent() {
        let manager = channel_manager();

        shutdown_handler(Arc::clone(&manager)).await;
        shutdown_handler(manager).await;
    }

    #[tokio::test]
    async fn cleanup_task_stops_on_cancellation() {
        let (handle, token) = start_cleanup_task(channel_manager(), None);

        assert!(!handle.is_finished());
        token.cancel();

        tokio::time::timeout(Duration::from_secs(2), handle)
            .await
            .unwrap()
            .unwrap();
    }
}
