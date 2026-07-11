//! `aether-io` — device protocol and field I/O service.
//!
//! A high-performance, async-first industrial communication service written in Rust.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

#[cfg(feature = "swagger-ui")]
use aether_io::api::routes::IoApiDoc;
use axum::serve;
use clap::Parser;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};
#[cfg(feature = "swagger-ui")]
use utoipa::OpenApi;
#[cfg(feature = "swagger-ui")]
use utoipa_swagger_ui::SwaggerUi;

use aether_io::core::config::DEFAULT_PORT;
use common::service_bootstrap::ServiceInfo;
use errors::AetherResult;

// aether-io imports
use aether_io::{
    api::{
        command_cache::CommandTxCache,
        routes::{create_api_routes, set_service_start_time},
    },
    core::{
        bootstrap::{self, Args},
        channels::ChannelManager,
        config::ConfigManager,
    },
    error::IoError,
    runtime::{start_cleanup_task, start_communication_service},
    shutdown_services, wait_for_shutdown,
};
use aether_routing::load_routing_maps;
use aether_rtdb_shm::{
    AUTOMATION_POINT_WATCH_UDS_PATH, PointWatchSignaler, SubscriptionBitmap,
    automation_bitmap_path_from_shm, bitmap_path_for_consumer,
};
use aether_rtdb_shm::{ChannelToSlotIndex, SharedConfig, ShmHandle, UnifiedWriter};
use aether_rtdb_shm::{SnapshotConfig, SnapshotManager, is_shm_available, snapshot_exists};
use aether_shm_bridge::{
    ChannelHealthManifest, ShmChannelHealthWriter, channel_health_path_from_shm,
    point_watch_socket_from_shm,
};

#[tokio::main]
async fn main() -> AetherResult<()> {
    // Parse arguments and initialize
    let args = Args::parse();
    let service_args = args.clone().into();

    let service_info = ServiceInfo::new(
        "aether-io",
        "Industrial Communication Service - Multi-Protocol Support",
        DEFAULT_PORT,
    );

    // Bootstrap: logging (API logging enabled by default), banner, system checks
    // Note: Config not loaded yet, use AETHER_LOG_DIR env or default
    bootstrap::initialize_logging(&service_args, &service_info, None)?;
    // Enable SIGHUP-triggered log reopen
    common::logging::enable_sighup_log_reopen();
    if !args.no_color {
        common::service_bootstrap::print_startup_banner(&service_info);
    }
    bootstrap::check_system_requirements()?;

    // Validation mode: validate and exit
    if args.validate {
        bootstrap::validate_configuration().await?;
        info!("Validation completed successfully");
        return Ok(());
    }

    // Load configuration from unified database
    let db_path = service_args.get_db_path("aether-io");
    info!(
        "Loading configuration from unified SQLite database: {}",
        db_path
    );
    let config_manager = Arc::new(ConfigManager::load().await?);
    let app_config = config_manager.config();

    // Create SQLite pool for API endpoints (foreign_keys=ON via shared helper)
    let sqlite_pool = sqlx::sqlite::SqlitePoolOptions::new()
        .connect_with(common::bootstrap_database::sqlite_connect_options(&db_path))
        .await
        .map_err(|e| IoError::ConfigError(format!("Failed to create SQLite pool: {}", e)))?;

    // Load routing configuration from the unified SQLite database.
    info!("Loading routing cache from unified database...");
    let routing_cache = {
        // Load routing maps from shared library
        let maps = load_routing_maps(&sqlite_pool)
            .await
            .map_err(|e| IoError::ConfigError(format!("Failed to load routing: {}", e)))?;

        info!("Loaded routing cache: {} total routes", maps.total_routes());

        Arc::new(aether_routing::RoutingCache::from_maps(
            maps.c2m, maps.m2c, maps.c2c,
        ))
    };

    // Shutdown token — created here so the SHM block can capture it for the
    // PointWatch drain task spawned during UnifiedWriter initialization.
    let shutdown_token = CancellationToken::new();

    // ============ Phase 2.5: Initialize UnifiedWriter (shared memory) ============
    // UnifiedWriter: creates shared memory with indexes from RoutingCache
    // Simplified: no SlotMeta, indexes are Vec in process memory
    // Now with snapshot restore/save support
    let (shm_handle, snapshot_manager_handle, snapshot_shutdown_tx, point_watch_drain_handle) = {
        // Load SharedConfig parameters from database
        let config = {
            let mut cfg = SharedConfig::default();

            // Helper to load usize value from service_config
            async fn load_usize(pool: &sqlx::SqlitePool, key: &str) -> Option<usize> {
                sqlx::query_scalar::<_, String>(
                    "SELECT value FROM service_config WHERE service_name = 'global' AND key = ?",
                )
                .bind(key)
                .fetch_optional(pool)
                .await
                .ok()
                .flatten()
                .and_then(|s| s.parse().ok())
            }

            if let Some(v) = load_usize(&sqlite_pool, "shared_memory.max_slots").await {
                cfg = cfg.with_max_slots(v);
            }

            // Apply snapshot configuration from environment
            cfg = cfg.with_snapshot_from_env();

            debug!(
                "SharedConfig: max_slots={:?}, snapshot_path={:?}, snapshot_interval={:?}",
                cfg.max_slots(),
                cfg.snapshot_path(),
                cfg.snapshot_interval()
            );
            cfg
        };

        // Load channel point counts for SHM layout (routing-independent)
        let channel_points = aether_rtdb_shm::ChannelPointCounts::load_from_db(&sqlite_pool)
            .await
            .map_err(|error| {
                IoError::config(format!(
                    "failed to load authoritative SHM point layout from SQLite: {error}"
                ))
            })?;

        // Best-effort cleanup of orphan per-generation staging files left
        // behind by a crashed `ShmHandle::rebuild_via_swap` in a previous
        // run. Safe to run unconditionally: matches only
        // `{stem}-{digits}{.ext}` files in canonical's parent dir, never
        // the canonical file itself. Steady-state operation leaves no
        // such files (the rename consumes the staging name atomically).
        match aether_rtdb_shm::core::config::cleanup_orphan_generation_files(config.path()) {
            Ok(0) => {},
            Ok(n) => info!("removed {n} orphan SHM generation file(s) from previous run"),
            Err(e) => warn!("orphan SHM file cleanup failed (non-fatal): {e}"),
        }

        // Create UnifiedWriter from channel points (automatic slot allocation)
        // is_shm_available checks if parent directory exists (Docker mount point)
        if is_shm_available(&config) {
            // Try to restore from snapshot first (if enabled and snapshot exists)
            let writer = if config.restore_on_start() {
                if let Some(snapshot_path) = config.snapshot_path() {
                    if snapshot_exists(snapshot_path) {
                        info!("Attempting to restore from snapshot: {:?}", snapshot_path);
                        match UnifiedWriter::restore_from_snapshot_published(
                            &config,
                            snapshot_path,
                            &channel_points,
                        ) {
                            Ok(w) => {
                                info!(
                                    "UnifiedWriter: restored from snapshot with {} slots",
                                    w.slot_count()
                                );
                                Some(w)
                            },
                            Err(e) => {
                                warn!("Snapshot restore failed, creating fresh: {}", e);
                                None
                            },
                        }
                    } else {
                        debug!(
                            "No snapshot file found at {:?}, creating fresh",
                            snapshot_path
                        );
                        None
                    }
                } else {
                    None
                }
            } else {
                debug!("Snapshot restore disabled, creating fresh");
                None
            };

            // If restore failed or not attempted, create fresh
            let writer = match writer {
                Some(w) => Ok(w),
                None => UnifiedWriter::create_published(&config, &channel_points),
            };

            match writer {
                Ok(mut writer) => {
                    info!(
                        "UnifiedWriter: ready with {} slots (Header + PointSlots only)",
                        writer.slot_count()
                    );

                    // Build channel → slot index from writer's layouts
                    let index = ChannelToSlotIndex::from_unified_writer(&writer);
                    info!("ChannelToSlotIndex: {} mappings", index.len());

                    // ── PointWatch bootstrap (io side) ───────────────────────────
                    // 1. Derive bitmap path from SHM path.
                    // 2. Create zero-filled SubscriptionBitmap (automation will write bits).
                    // 3. Build ReverseSlotIndex from the forward index (O(slots) once).
                    // 4. Spawn drain task + create PointWatchSignaler.
                    // 5. Attach signaler to writer BEFORE it moves into ShmHandle.
                    // 6. Register signaler with ShmHandle so rebuild_via_swap re-attaches it.
                    //
                    // Graceful degradation: any failure disables PointWatch; io still
                    // functions and consumers retain their polling reconciliation path.
                    let pw_drain_handle = {
                        let bitmap_path = automation_bitmap_path_from_shm(config.path());
                        match SubscriptionBitmap::open_or_create(&bitmap_path) {
                            Ok(bitmap) => {
                                let bitmap = Arc::new(bitmap);
                                let slot_count = writer.slot_count();
                                let reverse_index =
                                    Arc::new(aether_rtdb_shm::ReverseSlotIndex::from_forward(
                                        &index, slot_count,
                                    ));
                                let automation_socket =
                                    std::env::var("AETHER_AUTOMATION_POINT_WATCH_SOCKET")
                                        .unwrap_or_else(|_| {
                                            AUTOMATION_POINT_WATCH_UDS_PATH.to_string()
                                        });
                                let mut fanout_targets =
                                    vec![(Arc::clone(&bitmap), automation_socket)];
                                let alarm_bitmap_path =
                                    bitmap_path_for_consumer(config.path(), "alarm");
                                match SubscriptionBitmap::open_or_create(&alarm_bitmap_path) {
                                    Ok(alarm_bitmap) => {
                                        let alarm_socket =
                                            std::env::var("AETHER_ALARM_POINT_WATCH_SOCKET")
                                                .map(std::path::PathBuf::from)
                                                .unwrap_or_else(|_| {
                                                    point_watch_socket_from_shm(
                                                        config.path(),
                                                        "alarm",
                                                    )
                                                });
                                        fanout_targets.push((
                                            Arc::new(alarm_bitmap),
                                            alarm_socket.to_string_lossy().into_owned(),
                                        ));
                                    },
                                    Err(error) => warn!(
                                        "alarm PointWatch target disabled (bitmap create failed): {error}"
                                    ),
                                }
                                let api_bitmap_path =
                                    bitmap_path_for_consumer(config.path(), "api");
                                match SubscriptionBitmap::open_or_create(&api_bitmap_path) {
                                    Ok(api_bitmap) => {
                                        let api_socket =
                                            std::env::var("AETHER_API_POINT_WATCH_SOCKET")
                                                .map(std::path::PathBuf::from)
                                                .unwrap_or_else(|_| {
                                                    point_watch_socket_from_shm(
                                                        config.path(),
                                                        "api",
                                                    )
                                                });
                                        fanout_targets.push((
                                            Arc::new(api_bitmap),
                                            api_socket.to_string_lossy().into_owned(),
                                        ));
                                    },
                                    Err(error) => warn!(
                                        "api PointWatch target disabled (bitmap create failed): {error}"
                                    ),
                                }
                                let (signaler, drain_handle) = PointWatchSignaler::new_with_fanout(
                                    fanout_targets,
                                    reverse_index,
                                    shutdown_token.clone(),
                                );
                                // Attach before writer is consumed by ShmHandle::new.
                                writer.set_point_watcher(Some(Arc::clone(&signaler)));
                                Some((signaler, drain_handle))
                            },
                            Err(e) => {
                                warn!("PointWatch disabled (bitmap create failed): {}", e);
                                None
                            },
                        }
                    };

                    // Create ShmHandle (runtime-swappable writer + index)
                    let handle = Arc::new(ShmHandle::new(config.clone(), writer, index));

                    // Register signaler with ShmHandle so rebuild_via_swap re-attaches it.
                    let pw_drain_handle = if let Some((signaler, drain_h)) = pw_drain_handle {
                        handle.store_point_watcher(Arc::clone(&signaler));
                        info!("PointWatch fanout enabled: signaler attached to SHM writer");
                        Some(drain_h)
                    } else {
                        None
                    };

                    // Start SnapshotManager if configured
                    // SnapshotManager holds Arc<ShmHandle> — always snapshots the latest writer after rebuild
                    let (snapshot_handle, snapshot_tx) = if let (Some(path), Some(interval)) =
                        (config.snapshot_path(), config.snapshot_interval())
                    {
                        let (tx, rx) = tokio::sync::watch::channel(false);
                        let snapshot_config = SnapshotConfig::new(path.clone(), interval);
                        let manager =
                            SnapshotManager::new(Arc::clone(&handle), snapshot_config, rx);
                        let snap_handle = manager.start();
                        info!(
                            "SnapshotManager started: interval={:?}, path={:?}",
                            interval, path
                        );
                        (Some(snap_handle), Some(tx))
                    } else {
                        debug!("SnapshotManager not started (snapshot disabled)");
                        (None, None)
                    };

                    (handle, snapshot_handle, snapshot_tx, pw_drain_handle)
                },
                Err(e) => {
                    return Err(IoError::ConfigError(format!(
                        "authoritative SHM writer initialization failed: {e}"
                    ))
                    .into());
                },
            }
        } else {
            return Err(IoError::ConfigError(format!(
                "authoritative SHM parent directory is unavailable: {}",
                config.path().display()
            ))
            .into());
        }
    };

    // Writer liveness belongs to the SHM authority itself, not to any
    // commissioned channel. Keep it fresh even on the intentionally empty
    // default site so readers can distinguish a live writer from a valid but
    // abandoned mmap file.
    let shm_heartbeat_handle = {
        let heartbeat_handle = Arc::clone(&shm_handle);
        let heartbeat_shutdown = shutdown_token.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(1));
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tokio::select! {
                    _ = interval.tick() => {
                        if let Some(layout_guard) = heartbeat_handle.layout()
                            && let Some(layout) = layout_guard.as_ref()
                        {
                            layout.writer.update_heartbeat(
                                aether_rtdb_shm::core::config::timestamp_ms(),
                            );
                        }
                    },
                    _ = heartbeat_shutdown.cancelled() => break,
                }
            }
        })
    };

    // CommandTxCache for O(1) hot path access
    // Bypasses ChannelManager RwLock for Control/Adjustment writes
    let command_tx_cache = Arc::new(CommandTxCache::new());
    info!("CommandTxCache initialized (O(1) hot path for Control/Adjustment)");

    let (shm_listener_shutdown_tx, shm_listener_shutdown_rx) = tokio::sync::watch::channel(false);

    let channel_health_writer = {
        match sqlx::query_scalar::<_, i64>("SELECT channel_id FROM channels ORDER BY channel_id")
            .fetch_all(&sqlite_pool)
            .await
        {
            Ok(raw_ids) => {
                let channel_ids: Vec<u32> = raw_ids
                    .into_iter()
                    .filter_map(|channel_id| match u32::try_from(channel_id) {
                        Ok(channel_id) => Some(channel_id),
                        Err(_) => {
                            warn!("invalid channel_id {channel_id} excluded from health SHM");
                            None
                        },
                    })
                    .collect();
                let manifest = Arc::new(ChannelHealthManifest::from_channel_ids(channel_ids));
                let main_shm_path = shm_handle.config().path().clone();
                let health_path = std::env::var("AETHER_CHANNEL_HEALTH_SHM_PATH")
                    .map(std::path::PathBuf::from)
                    .unwrap_or_else(|_| channel_health_path_from_shm(&main_shm_path));
                match ShmChannelHealthWriter::create(&health_path, manifest) {
                    Ok(writer) => {
                        info!("Channel health SHM ready: {}", health_path.display());
                        Some(Arc::new(writer))
                    },
                    Err(error) => {
                        warn!("Channel health SHM disabled: {error}");
                        None
                    },
                }
            },
            Err(error) => {
                warn!("Channel health SHM disabled; failed to load channel ids: {error}");
                None
            },
        }
    };

    // Create channel manager over the mandatory SHM writer.
    // Lock-free architecture - no RwLock wrapper needed
    let channel_manager = ChannelManager::with_shared_memory(
        routing_cache,
        sqlite_pool.clone(),
        Arc::clone(&shm_handle),
        channel_health_writer,
        Some(Arc::clone(&command_tx_cache)),
    )?;

    // Configure SHM listener for event-driven M2C dispatch.
    let channel_manager = channel_manager.with_shm_listener(shm_listener_shutdown_rx);

    let channel_manager = Arc::new(channel_manager);

    // Determine bind address and start server
    let bind_address = bootstrap::determine_bind_address(
        args.bind_address,
        &app_config.api.host,
        app_config.api.port,
    );
    let addr: SocketAddr = bind_address.parse().map_err(|e| {
        IoError::ConfigError(format!("Invalid bind address '{}': {}", bind_address, e))
    })?;

    info!("Starting {} service", app_config.service.name);

    // Start communication channels
    let configured_count =
        start_communication_service(config_manager.clone(), Arc::clone(&channel_manager)).await?;

    // Start SHM command listener for event-driven M2C dispatch
    // This must be started after channels are created (so they can be registered)
    let shm_listener_handle = channel_manager.start_shm_listener();
    if shm_listener_handle.is_some() {
        info!("ShmCommandListener started for event-driven M2C dispatch (~1-2ms latency)");
    }

    let (cleanup_handle, cleanup_token) =
        start_cleanup_task(Arc::clone(&channel_manager), configured_count);
    // Start routing cache polling task (auto-detect routing changes from SQLite)
    let poll_pool = sqlite_pool.clone();
    let poll_cache = Arc::clone(&channel_manager.routing_cache);
    let poll_token = shutdown_token.clone();
    tokio::spawn(async move {
        let mut last_hash = poll_cache.content_hash();
        info!(
            "Routing poll started (2s interval, hash=0x{:016X})",
            last_hash
        );
        loop {
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_secs(2)) => {},
                _ = poll_token.cancelled() => break,
            }
            match aether_routing::load_routing_maps(&poll_pool).await {
                Ok(maps) => {
                    poll_cache.update(maps.c2m, maps.m2c, maps.c2c);
                    let new_hash = poll_cache.content_hash();
                    if new_hash != last_hash {
                        info!(
                            "Routing cache updated: 0x{:016X} → 0x{:016X}",
                            last_hash, new_hash
                        );
                        last_hash = new_hash;
                    }
                },
                Err(e) => {
                    tracing::warn!("Routing poll failed: {}", e);
                },
            }
        }
        info!("Routing poll stopped");
    });

    // Start API server
    set_service_start_time(chrono::Utc::now());
    let app = create_api_routes(
        Arc::clone(&channel_manager),
        sqlite_pool,
        Arc::clone(&command_tx_cache),
    );

    #[cfg(feature = "swagger-ui")]
    let app = {
        info!("Swagger UI feature ENABLED - initializing at /docs");
        let openapi = IoApiDoc::openapi();
        let merged = app.merge(SwaggerUi::new("/docs").url("/openapi.json", openapi));
        info!("Swagger UI configured successfully");
        merged
    };

    #[cfg(not(feature = "swagger-ui"))]
    info!("Swagger UI feature DISABLED");

    // Note: HTTP request logging middleware is applied in create_api_routes()

    let socket = tokio::net::TcpSocket::new_v4()
        .map_err(|e| IoError::ConnectionError(format!("Failed to create socket: {}", e)))?;
    socket
        .set_reuseaddr(true)
        .map_err(|e| IoError::ConnectionError(format!("Failed to set SO_REUSEADDR: {}", e)))?;
    socket
        .bind(addr)
        .map_err(|e| IoError::ConnectionError(format!("Failed to bind to {}: {}", addr, e)))?;
    let listener = socket
        .listen(1024)
        .map_err(|e| IoError::ConnectionError(format!("Failed to listen: {}", e)))?;

    info!("API server listening on http://{}", addr);
    info!("Health check: http://{}/health", addr);

    let server = serve(listener, app);
    let server_token = shutdown_token.clone();
    let server_handle = tokio::spawn(async move {
        let shutdown = async move { server_token.cancelled().await };
        if let Err(e) = server.with_graceful_shutdown(shutdown).await {
            error!("Server error: {}", e);
        }
    });

    // Wait for shutdown and cleanup
    wait_for_shutdown().await;

    // Signal SHM listener to shutdown
    let _ = shm_listener_shutdown_tx.send(true);

    // Signal SnapshotManager to shutdown and save final snapshot
    if let Some(tx) = snapshot_shutdown_tx {
        let _ = tx.send(true);
        info!("Signaled SnapshotManager to save final snapshot");
    }

    shutdown_services(
        channel_manager,
        shutdown_token,
        cleanup_token,
        cleanup_handle,
        server_handle,
    )
    .await;

    match tokio::time::timeout(Duration::from_secs(2), shm_heartbeat_handle).await {
        Ok(Ok(())) => info!("SHM writer heartbeat task stopped"),
        Ok(Err(error)) => error!("SHM writer heartbeat task failed: {error}"),
        Err(_) => error!("SHM writer heartbeat task shutdown timed out"),
    }

    // Wait for SHM listener task to complete (if it was started)
    if let Some(handle) = shm_listener_handle {
        let _ = handle.await;
        info!("ShmCommandListener shutdown complete");
    }

    // Wait for SnapshotManager to complete (saves final snapshot)
    if let Some(handle) = snapshot_manager_handle {
        match tokio::time::timeout(std::time::Duration::from_secs(10), handle).await {
            Ok(Ok(())) => info!("SnapshotManager shutdown complete"),
            Ok(Err(e)) => error!("SnapshotManager task failed: {}", e),
            Err(_) => error!("SnapshotManager shutdown timed out"),
        }
    }

    // Wait for PointWatch drain task to flush remaining events and stop.
    if let Some(handle) = point_watch_drain_handle {
        match tokio::time::timeout(std::time::Duration::from_secs(2), handle).await {
            Ok(_) => info!("PointWatch drain task stopped"),
            Err(_) => warn!("PointWatch drain task shutdown timed out"),
        }
    }

    Ok(())
}
