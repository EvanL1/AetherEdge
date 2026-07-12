//! Runtime lifecycle management
//!
//! Provides orchestration functions for service startup, shutdown, and maintenance tasks
//! as part of the runtime orchestration layer

use std::time::Duration;

use crate::core::channels::ChannelManager;
use crate::core::config::ConfigManager;
use crate::error::Result;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

// ============================================================================
// Lifecycle timing constants
// ============================================================================

/// Brief wait after channel initialization to ensure all channels are ready
const INIT_WAIT: Duration = Duration::from_millis(500);

/// Per-channel timeout during graceful shutdown
const CHANNEL_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);

/// Interval between periodic cleanup/statistics cycles
const CLEANUP_INTERVAL: Duration = Duration::from_secs(300);

/// Heartbeat timeout: if a task hasn't updated its heartbeat in this duration,
/// it's considered stuck and will be force-aborted.
const WATCHDOG_HEARTBEAT_TIMEOUT_SECS: i64 = 120;

/// Overall timeout for the service shutdown sequence
const SERVICE_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(30);

/// Start the communication service: create and connect all configured channels concurrently.
///
/// Individual channel failures don't stop service startup (graceful degradation).
/// Returns the total number of configured channels.
/// # Lock-free channel_manager
pub async fn start_communication_service(
    config_manager: Arc<ConfigManager>,
    channel_manager: Arc<ChannelManager>,
) -> Result<usize> {
    debug!("start_communication_service called");

    // Get channel configurations
    let configs = config_manager.channels();

    if configs.is_empty() {
        warn!("No channels configured");
        return Ok(0);
    }

    let total_configured = configs.len();
    let enabled_count = configs.iter().filter(|c| c.is_enabled()).count();
    let disabled_count = total_configured - enabled_count;

    info!(
        "Found {} channels: {} enabled, {} disabled",
        configs.len(),
        enabled_count,
        disabled_count
    );

    // Record disabled channels.
    for channel in configs.iter().filter(|c| !c.is_enabled()) {
        info!(
            "Channel {} ({}) is disabled, skipping",
            channel.id(),
            channel.name()
        );
    }

    // Create all channels concurrently to improve startup performance.
    use futures::future::join_all;

    // First create all channel instances concurrently without holding the lock.
    let channel_futures: Vec<_> = configs
        .iter()
        .filter(|c| c.is_enabled()) // Only create enabled channels.
        .map(|channel_config| {
            let channel_manager = Arc::clone(&channel_manager);
            // Clone the Arc (cheap reference count increment), not the inner ChannelConfig
            let channel_config = Arc::clone(channel_config);
            async move {
                let channel_id = channel_config.id();
                let channel_name = channel_config.name().to_string();

                info!("Creating channel: {} - {}", channel_id, channel_name);

                // Debug: Verify points are available before creating channel
                debug!(
                    "Channel {} points will be loaded from SQLite at runtime",
                    channel_id
                );

                // Direct access without RwLock (lock-free)
                let result = channel_manager.create_channel(channel_config).await;
                match result {
                    Ok(_) => {
                        info!("Channel created successfully: {}", channel_id);
                        Ok((channel_id, channel_name))
                    },
                    Err(e) => {
                        error!("Failed to create channel {}: {}", channel_id, e);
                        Err((channel_id, channel_name, e))
                    },
                }
            }
        })
        .collect();

    // Wait for all channels to be created.
    let results = join_all(channel_futures).await;

    // Summarize successful and failed channel creations.
    let mut successful_channels = 0;
    let mut failed_channels = 0;
    let mut failed_details = Vec::new();

    for result in results {
        match result {
            Ok((id, name)) => {
                successful_channels += 1;
                debug!("Channel {} ({}) added to successful list", id, name);
            },
            Err((id, name, err)) => {
                failed_channels += 1;
                failed_details.push(format!("Channel {} ({}): {}", id, name, err));
            },
        }
    }

    // If any channel failed, log the details.
    if !failed_details.is_empty() {
        error!("Failed channels details:");
        for detail in &failed_details {
            error!("  - {}", detail);
        }
    }

    info!(
        "Channel initialization completed: {} successful, {} failed",
        successful_channels, failed_channels
    );

    // Wait briefly to ensure all channels are initialized
    tokio::time::sleep(INIT_WAIT).await;

    // Phase 2: Establish connections for all channels in batch
    info!("Starting connection phase for all initialized channels...");
    // Direct access without RwLock (lock-free)
    match channel_manager.connect_all_channels().await {
        Ok(()) => {
            info!("All channel connections completed successfully");
        },
        Err(e) => {
            error!("Some channel connections failed: {}", e);
            // Connection failure should not prevent service startup, continue running
        },
    }

    info!(
        "Communication service started with {} channels successfully initialized",
        successful_channels
    );

    Ok(total_configured)
}

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
    configured_count: usize,
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
                    let all_stats = channel_manager.get_all_channel_stats().await;
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
                            error!(
                                "Ch{} ({}) watchdog: heartbeat stale for {}s, respawning task",
                                stat.channel_id,
                                stat.name,
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
                                            stat.channel_id, stat.name, error
                                        );
                                    }
                                },
                                None => {
                                    error!(
                                        "Ch{} ({}) watchdog repair deferred: reconciler unavailable",
                                        stat.channel_id, stat.name
                                    );
                                },
                            }
                        }
                    }

                    // Collect active channels for display
                    let active_channels: Vec<String> = all_stats
                        .iter()
                        .filter(|s| s.is_connected)
                        .map(|s| format!("{}({})", s.name, s.channel_id))
                        .collect();

                    let failed_count = all_stats.iter().filter(|s| s.reconnect_failed).count();

                    if active_channels.is_empty() {
                        info!(
                            "Channel stats: configured={}, initialized={}, active=0, failed={}",
                            configured_count,
                            all_stats.len(),
                            failed_count,
                        );
                    } else {
                        info!(
                            "Channel stats: configured={}, initialized={}, active={}, failed={} [{}]",
                            configured_count,
                            all_stats.len(),
                            active_channels.len(),
                            failed_count,
                            active_channels.join(", ")
                        );
                    }
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
    use super::*;
    use crate::core::config::ConfigManager;
    use sqlx::SqlitePool;
    use std::sync::Arc;
    use tempfile::TempDir;

    /// Helper: Create a test database with minimal configuration
    async fn create_test_database() -> (TempDir, String) {
        let temp_dir = TempDir::new().unwrap();
        let db_path = temp_dir.path().join("test_lifecycle.db");
        let db_url = format!("sqlite://{}?mode=rwc", db_path.display());

        let pool = SqlitePool::connect(&db_url).await.unwrap();

        // Create service_config table (with service_name column and composite primary key)
        sqlx::query(
            "CREATE TABLE service_config (
                service_name TEXT NOT NULL,
                key TEXT NOT NULL,
                value TEXT NOT NULL,
                type TEXT DEFAULT 'string',
                description TEXT,
                updated_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
                PRIMARY KEY (service_name, key)
            )",
        )
        .execute(&pool)
        .await
        .unwrap();

        // Insert basic service config (with service_name column)
        sqlx::query("INSERT INTO service_config (service_name, key, value) VALUES ('aether-io', 'service_name', 'aether-io')")
            .execute(&pool)
            .await
            .unwrap();

        // Create channels table
        sqlx::query(
            "CREATE TABLE channels (
                channel_id INTEGER PRIMARY KEY,
                name TEXT NOT NULL,
                protocol TEXT NOT NULL,
                enabled BOOLEAN DEFAULT TRUE,
                config TEXT DEFAULT '{}'
            )",
        )
        .execute(&pool)
        .await
        .unwrap();

        // Create point tables
        for table_name in &[
            "telemetry_points",
            "signal_points",
            "control_points",
            "adjustment_points",
        ] {
            sqlx::query(&format!(
                "CREATE TABLE {} (
                    point_id INTEGER PRIMARY KEY,
                    signal_name TEXT NOT NULL,
                    scale REAL DEFAULT 1.0,
                    offset REAL DEFAULT 0.0,
                    unit TEXT DEFAULT '',
                    reverse BOOLEAN DEFAULT FALSE,
                    data_type TEXT DEFAULT 'float32',
                    description TEXT DEFAULT ''
                )",
                table_name
            ))
            .execute(&pool)
            .await
            .unwrap();
        }

        pool.close().await;
        (temp_dir, db_path.to_string_lossy().to_string())
    }

    /// Helper: Add test channels to database
    async fn add_test_channels(db_path: &str, enabled: bool) {
        let pool = SqlitePool::connect(&format!("sqlite://{}", db_path))
            .await
            .unwrap();

        sqlx::query("INSERT INTO channels (channel_id, name, protocol, enabled) VALUES (1001, 'Test Channel 1', 'virtual', ?)")
            .bind(enabled)
            .execute(&pool)
            .await
            .unwrap();

        sqlx::query("INSERT INTO channels (channel_id, name, protocol, enabled) VALUES (1002, 'Test Channel 2', 'virtual', ?)")
            .bind(enabled)
            .execute(&pool)
            .await
            .unwrap();

        pool.close().await;
    }

    // ========================================================================
    // Phase 1: Service Startup Tests
    // ========================================================================

    #[tokio::test]
    async fn test_start_service_success_with_enabled_channels() {
        let (_temp_dir, db_path) = create_test_database().await;
        add_test_channels(&db_path, true).await;

        let config_manager = Arc::new(ConfigManager::from_sqlite(&db_path).await.unwrap());
        let channel_manager = Arc::new(
            ChannelManager::new(
                crate::test_utils::create_test_shm_handle(),
                crate::test_utils::create_test_routing_cache(),
            )
            .unwrap(),
        );

        let result = start_communication_service(config_manager, channel_manager).await;

        assert!(result.is_ok(), "Service startup should succeed");
        let configured_count = result.unwrap();
        assert_eq!(
            configured_count, 2,
            "Should return count of configured channels"
        );
    }

    #[tokio::test]
    async fn test_start_service_with_no_channels() {
        let (_temp_dir, db_path) = create_test_database().await;
        // Don't add any channels

        let config_manager = Arc::new(ConfigManager::from_sqlite(&db_path).await.unwrap());
        let channel_manager = Arc::new(
            ChannelManager::new(
                crate::test_utils::create_test_shm_handle(),
                crate::test_utils::create_test_routing_cache(),
            )
            .unwrap(),
        );

        let result = start_communication_service(config_manager, channel_manager).await;

        assert!(
            result.is_ok(),
            "Service startup should succeed with no channels"
        );
        let configured_count = result.unwrap();
        assert_eq!(configured_count, 0, "Should return 0 for no channels");
    }

    #[tokio::test]
    async fn test_start_service_with_disabled_channels() {
        let (_temp_dir, db_path) = create_test_database().await;
        add_test_channels(&db_path, false).await; // disabled channels

        let config_manager = Arc::new(ConfigManager::from_sqlite(&db_path).await.unwrap());
        let channel_manager = Arc::new(
            ChannelManager::new(
                crate::test_utils::create_test_shm_handle(),
                crate::test_utils::create_test_routing_cache(),
            )
            .unwrap(),
        );

        let result = start_communication_service(config_manager, channel_manager).await;

        assert!(
            result.is_ok(),
            "Service startup should succeed with disabled channels"
        );
        let configured_count = result.unwrap();
        assert_eq!(
            configured_count, 2,
            "Should return configured count even if disabled"
        );
    }

    // ========================================================================
    // Phase 2: Service Shutdown Tests
    // ========================================================================

    #[tokio::test]
    async fn test_shutdown_with_active_channels() {
        let (_temp_dir, db_path) = create_test_database().await;
        add_test_channels(&db_path, true).await;

        let config_manager = Arc::new(ConfigManager::from_sqlite(&db_path).await.unwrap());
        let channel_manager = Arc::new(
            ChannelManager::new(
                crate::test_utils::create_test_shm_handle(),
                crate::test_utils::create_test_routing_cache(),
            )
            .unwrap(),
        );

        // Start service first
        let _ = start_communication_service(config_manager, channel_manager.clone()).await;

        // Now shutdown
        shutdown_handler(channel_manager.clone()).await;

        // Verify all channels are stopped (direct access without RwLock)
        assert_eq!(
            channel_manager.channel_count(),
            0,
            "All channels should be removed after shutdown"
        );
    }

    #[tokio::test]
    async fn test_shutdown_with_no_channels() {
        let channel_manager = Arc::new(
            ChannelManager::new(
                crate::test_utils::create_test_shm_handle(),
                crate::test_utils::create_test_routing_cache(),
            )
            .unwrap(),
        );

        // Shutdown without starting any channels
        shutdown_handler(channel_manager).await;

        // Test passes if no panic occurs
    }

    #[tokio::test]
    async fn test_shutdown_idempotency() {
        let (_temp_dir, db_path) = create_test_database().await;
        add_test_channels(&db_path, true).await;

        let config_manager = Arc::new(ConfigManager::from_sqlite(&db_path).await.unwrap());
        let channel_manager = Arc::new(
            ChannelManager::new(
                crate::test_utils::create_test_shm_handle(),
                crate::test_utils::create_test_routing_cache(),
            )
            .unwrap(),
        );

        // Start service
        let _ = start_communication_service(config_manager, channel_manager.clone()).await;

        // Shutdown twice
        shutdown_handler(channel_manager.clone()).await;
        shutdown_handler(channel_manager).await;

        // Test passes if no panic occurs on second shutdown
    }

    // ========================================================================
    // Phase 3: Cleanup Task Tests
    // ========================================================================

    #[tokio::test]
    async fn test_cleanup_task_starts() {
        let channel_manager = Arc::new(
            ChannelManager::new(
                crate::test_utils::create_test_shm_handle(),
                crate::test_utils::create_test_routing_cache(),
            )
            .unwrap(),
        );

        let (handle, cancel_token) = start_cleanup_task(channel_manager, 0, None);

        // Verify handle is valid
        assert!(!handle.is_finished(), "Cleanup task should be running");

        // Cancel and wait for completion
        cancel_token.cancel();
        let _ = tokio::time::timeout(tokio::time::Duration::from_secs(2), handle).await;
    }

    #[tokio::test]
    async fn test_cleanup_task_cancellation() {
        let channel_manager = Arc::new(
            ChannelManager::new(
                crate::test_utils::create_test_shm_handle(),
                crate::test_utils::create_test_routing_cache(),
            )
            .unwrap(),
        );

        let (handle, cancel_token) = start_cleanup_task(channel_manager, 0, None);

        // Cancel immediately
        cancel_token.cancel();

        // Wait for task to complete
        let result = tokio::time::timeout(tokio::time::Duration::from_secs(2), handle).await;
        assert!(result.is_ok(), "Task should complete after cancellation");
    }

    #[tokio::test]
    async fn test_cleanup_task_with_channels() {
        let (_temp_dir, db_path) = create_test_database().await;
        add_test_channels(&db_path, true).await;

        let config_manager = Arc::new(ConfigManager::from_sqlite(&db_path).await.unwrap());
        let channel_manager = Arc::new(
            ChannelManager::new(
                crate::test_utils::create_test_shm_handle(),
                crate::test_utils::create_test_routing_cache(),
            )
            .unwrap(),
        );

        // Start service
        let configured_count = start_communication_service(config_manager, channel_manager.clone())
            .await
            .unwrap();

        // Start cleanup task
        let (handle, cancel_token) =
            start_cleanup_task(channel_manager.clone(), configured_count, None);

        // Let it run briefly
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

        // Cancel and cleanup
        cancel_token.cancel();
        let _ = tokio::time::timeout(tokio::time::Duration::from_secs(2), handle).await;
    }

    // ========================================================================
    // Phase 4: Connection Phase Tests
    // ========================================================================

    #[tokio::test]
    async fn test_service_connection_phase_completes() {
        let (_temp_dir, db_path) = create_test_database().await;
        add_test_channels(&db_path, true).await;

        let config_manager = Arc::new(ConfigManager::from_sqlite(&db_path).await.unwrap());
        let channel_manager = Arc::new(
            ChannelManager::new(
                crate::test_utils::create_test_shm_handle(),
                crate::test_utils::create_test_routing_cache(),
            )
            .unwrap(),
        );

        // Start service includes connection phase
        let result = start_communication_service(config_manager, channel_manager).await;

        assert!(
            result.is_ok(),
            "Service startup with connection phase should succeed"
        );
    }

    #[tokio::test]
    async fn test_connection_phase_does_not_block_startup() {
        let (_temp_dir, db_path) = create_test_database().await;
        add_test_channels(&db_path, true).await;

        let config_manager = Arc::new(ConfigManager::from_sqlite(&db_path).await.unwrap());
        let channel_manager = Arc::new(
            ChannelManager::new(
                crate::test_utils::create_test_shm_handle(),
                crate::test_utils::create_test_routing_cache(),
            )
            .unwrap(),
        );

        // Even if connections fail, startup should succeed
        let result = start_communication_service(config_manager, channel_manager).await;

        assert!(
            result.is_ok(),
            "Service startup should succeed even if connections fail"
        );
    }

    #[tokio::test]
    async fn test_parallel_channel_creation() {
        let (_temp_dir, db_path) = create_test_database().await;

        // Add multiple channels
        let pool = SqlitePool::connect(&format!("sqlite://{}", db_path))
            .await
            .unwrap();
        for i in 1001..1006 {
            sqlx::query("INSERT INTO channels (channel_id, name, protocol, enabled) VALUES (?, ?, 'virtual', true)")
                .bind(i)
                .bind(format!("Channel {}", i))
                .execute(&pool)
                .await
                .unwrap();
        }
        pool.close().await;

        let config_manager = Arc::new(ConfigManager::from_sqlite(&db_path).await.unwrap());
        let channel_manager = Arc::new(
            ChannelManager::new(
                crate::test_utils::create_test_shm_handle(),
                crate::test_utils::create_test_routing_cache(),
            )
            .unwrap(),
        );

        let start_time = std::time::Instant::now();
        let result = start_communication_service(config_manager, channel_manager).await;
        let elapsed = start_time.elapsed();

        assert!(result.is_ok(), "Parallel channel creation should succeed");
        // Parallel creation should be faster than sequential (< 5s for 5 channels)
        assert!(
            elapsed < tokio::time::Duration::from_secs(5),
            "Parallel creation should complete quickly"
        );
    }
}
