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
        routes::{create_api_routes_with_channel_applications, set_service_start_time},
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
use aether_shm_bridge::{
    AcquisitionCommitObserver, DEFAULT_MAX_SLOTS, PointWatchPublisher,
    ShmChannelHealthWriterHandle, ShmRuntimeConfig, ShmWriterHandle, SubscriptionBitmap,
    automation_bitmap_path_from_shm, begin_topology_publication, bitmap_path_for_consumer,
    channel_health_path_from_shm, cleanup_orphan_generation_files, default_shm_path,
    point_watch_socket_from_shm, timestamp_ms,
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

    let (initial_point_manifest, initial_health_manifest) =
        aether_store_local::load_sqlite_shm_topology(&sqlite_pool)
            .await
            .map_err(|error| {
                IoError::config(format!(
                    "failed to load authoritative SHM topology from SQLite: {error}"
                ))
            })?
            .into_manifests();
    // ============ Phase 2.5: publish the authoritative SHM generation ============
    let (
        shm_handle,
        snapshot_manager_handle,
        snapshot_shutdown_tx,
        point_watch_drain_handle,
        initial_topology_publication,
        initial_publication_epoch,
        initial_health_path,
    ) = {
        let shm_path = default_shm_path();
        let health_path = std::env::var("AETHER_CHANNEL_HEALTH_SHM_PATH")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|_| channel_health_path_from_shm(&shm_path));
        let max_slots = sqlx::query_scalar::<_, String>(
            "SELECT value FROM service_config WHERE service_name = 'global' AND key = 'shared_memory.max_slots'",
        )
        .fetch_optional(&sqlite_pool)
        .await
        .ok()
        .flatten()
        .and_then(|value| value.parse::<u32>().ok())
        .unwrap_or(DEFAULT_MAX_SLOTS);
        let runtime_config = ShmRuntimeConfig::new(&shm_path, max_slots);
        let manifest = Arc::new(initial_point_manifest);
        let snapshot_path = std::env::var("SHM_SNAPSHOT_PATH")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|_| std::path::PathBuf::from("data/shm-snapshot.bin"));
        let snapshot_interval = std::env::var("SHM_SNAPSHOT_INTERVAL")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .map(Duration::from_secs)
            .unwrap_or_else(|| Duration::from_secs(300));
        let restore_on_start = std::env::var("SHM_RESTORE_ON_START")
            .map(|value| !value.eq_ignore_ascii_case("false"))
            .unwrap_or(true);
        debug!(
            "SHM config: path={}, max_slots={}, snapshot_path={}, snapshot_interval={:?}",
            shm_path.display(),
            max_slots,
            snapshot_path.display(),
            snapshot_interval
        );

        match cleanup_orphan_generation_files(&shm_path) {
            Ok(0) => {},
            Ok(n) => info!("removed {n} orphan SHM generation file(s) from previous run"),
            Err(e) => warn!("orphan SHM file cleanup failed (non-fatal): {e}"),
        }

        if !shm_path.parent().is_some_and(std::path::Path::exists) {
            return Err(IoError::ConfigError(format!(
                "authoritative SHM parent directory is unavailable: {}",
                shm_path.display()
            ))
            .into());
        }

        let point_watch = match SubscriptionBitmap::open_or_create(
            &automation_bitmap_path_from_shm(&shm_path),
        ) {
            Ok(automation_bitmap) => {
                let automation_socket = std::env::var("AETHER_AUTOMATION_POINT_WATCH_SOCKET")
                    .map(std::path::PathBuf::from)
                    .unwrap_or_else(|_| point_watch_socket_from_shm(&shm_path, "automation"));
                let mut targets = vec![(Arc::new(automation_bitmap), automation_socket)];
                for (consumer, variable) in [
                    ("alarm", "AETHER_ALARM_POINT_WATCH_SOCKET"),
                    ("api", "AETHER_API_POINT_WATCH_SOCKET"),
                ] {
                    match SubscriptionBitmap::open_or_create(&bitmap_path_for_consumer(
                        &shm_path, consumer,
                    )) {
                        Ok(bitmap) => {
                            let socket = std::env::var(variable)
                                .map(std::path::PathBuf::from)
                                .unwrap_or_else(|_| {
                                    point_watch_socket_from_shm(&shm_path, consumer)
                                });
                            targets.push((Arc::new(bitmap), socket));
                        },
                        Err(error) => warn!(
                            "{consumer} PointWatch target disabled (bitmap create failed): {error}"
                        ),
                    }
                }
                let (publisher, drain) =
                    PointWatchPublisher::new_with_fanout(targets, shutdown_token.clone());
                Some((publisher, drain))
            },
            Err(error) => {
                warn!("PointWatch disabled (bitmap create failed): {error}");
                None
            },
        };
        let observer = point_watch
            .as_ref()
            .map(|(publisher, _)| Arc::clone(publisher) as Arc<dyn AcquisitionCommitObserver>);
        let restore_path = restore_on_start
            .then_some(snapshot_path.as_path())
            .filter(|path| path.exists());
        let mut topology_publication = begin_topology_publication(&shm_path).map_err(|error| {
            IoError::config(format!(
                "acquire coordinated SHM publication lease: {error}"
            ))
        })?;
        let publication_epoch = topology_publication
            .next_publication_epoch(&health_path)
            .map_err(|error| {
                IoError::config(format!(
                    "allocate coordinated SHM publication epoch: {error}"
                ))
            })?;
        let handle = match ShmWriterHandle::create_published_with_observer_at_epoch(
            runtime_config.clone(),
            Arc::clone(&manifest),
            restore_path,
            observer.clone(),
            publication_epoch,
        ) {
            Ok(handle) => handle,
            Err(error) if restore_path.is_some() => {
                warn!("Snapshot restore failed, creating fresh: {error}");
                ShmWriterHandle::create_published_with_observer_at_epoch(
                    runtime_config,
                    manifest,
                    None,
                    observer,
                    publication_epoch,
                )
                .map_err(|error| {
                    IoError::config(format!(
                        "authoritative SHM writer initialization failed: {error}"
                    ))
                })?
            },
            Err(error) => {
                return Err(IoError::config(format!(
                    "authoritative SHM writer initialization failed: {error}"
                ))
                .into());
            },
        };
        let handle = Arc::new(handle);
        info!(
            "authoritative SHM generation ready: slots={}",
            handle
                .generation()
                .map_or(0, |generation| generation.slot_count())
        );

        let (snapshot_tx, mut snapshot_rx) = tokio::sync::watch::channel(false);
        let snapshot_handle = {
            let handle = Arc::clone(&handle);
            let snapshot_path = snapshot_path.clone();
            tokio::spawn(async move {
                let mut interval = tokio::time::interval(snapshot_interval);
                interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                // Tokio intervals tick immediately once. Consume that tick so
                // startup restore is not immediately overwritten by a
                // redundant snapshot.
                interval.tick().await;
                loop {
                    tokio::select! {
                        _ = interval.tick() => {
                            if let Some(generation) = handle.generation()
                                && let Err(error) = generation.save_snapshot(&snapshot_path)
                            {
                                warn!("periodic SHM snapshot failed: {error}");
                            }
                        },
                        changed = snapshot_rx.changed() => {
                            if changed.is_err() || *snapshot_rx.borrow() {
                                break;
                            }
                        },
                    }
                }
                if let Some(generation) = handle.generation() {
                    generation
                        .save_snapshot(&snapshot_path)
                        .map_err(|error| anyhow::anyhow!("final SHM snapshot failed: {error}"))?;
                }
                Ok::<(), anyhow::Error>(())
            })
        };
        let point_watch_drain_handle = point_watch.map(|(_, drain)| drain);
        (
            handle,
            Some(snapshot_handle),
            Some(snapshot_tx),
            point_watch_drain_handle,
            topology_publication,
            publication_epoch,
            health_path,
        )
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
                        if let Some(generation) = heartbeat_handle.generation() {
                            generation.acquisition_writer().update_heartbeat(timestamp_ms());
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
        let health_path = initial_health_path;
        let writer = Arc::new(ShmChannelHealthWriterHandle::empty(&health_path));
        match writer
            .rebuild_for_publication(Arc::new(initial_health_manifest), initial_publication_epoch)
        {
            Ok(()) => {
                info!("Channel health SHM ready: {}", health_path.display());
            },
            Err(error) => {
                return Err(IoError::config(format!(
                    "coordinated channel-health SHM initialization failed: {error}"
                ))
                .into());
            },
        }
        initial_topology_publication
            .commit(&health_path, initial_publication_epoch)
            .map_err(|error| {
                IoError::config(format!(
                    "coordinated point/health SHM commit failed: {error}"
                ))
            })?;
        info!(
            publication_epoch = initial_publication_epoch,
            "Committed initial point/health SHM topology"
        );
        writer
    };
    let channel_health_heartbeat_handle = {
        let writer = Arc::clone(&channel_health_writer);
        let heartbeat_shutdown = shutdown_token.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(1));
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tokio::select! {
                    _ = interval.tick() => {
                        if let Err(error) = writer.update_heartbeat(timestamp_ms()) {
                            warn!("channel-health SHM heartbeat failed: {error}");
                        }
                    },
                    _ = heartbeat_shutdown.cancelled() => break,
                }
            }
        })
    };

    // Create channel manager over the mandatory SHM writer.
    // Lock-free architecture - no RwLock wrapper needed
    let channel_manager = ChannelManager::with_shared_memory(
        routing_cache,
        sqlite_pool.clone(),
        Arc::clone(&shm_handle),
        Some(Arc::clone(&channel_health_writer)),
        Some(Arc::clone(&command_tx_cache)),
    )?;

    // Configure SHM listener for event-driven M2C dispatch.
    let channel_manager = channel_manager.with_shm_listener(shm_listener_shutdown_rx);

    let channel_manager = Arc::new(channel_manager);
    let topology_projector = Arc::new(aether_io::store::SqliteShmTopologyProjector::new(
        sqlite_pool.clone(),
        Arc::clone(&shm_handle),
        Arc::clone(&channel_health_writer),
    ));
    let channel_adapter = Arc::new(aether_io::SqliteChannelMutator::new_with_topology(
        sqlite_pool.clone(),
        Arc::clone(&channel_manager),
        Arc::clone(&topology_projector),
    ));

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

    let automatic_reconciliation = Arc::new(
        aether_io::automatic_reconciliation::AutomaticIoReconciler::new(
            sqlite_pool.clone(),
            Arc::clone(&channel_adapter),
            topology_projector,
            Arc::clone(&channel_adapter),
        ),
    );
    match automatic_reconciliation.reconcile_once().await {
        Ok(receipt) if receipt.converged() => {
            info!(
                attempted_channels = receipt.attempted_channels(),
                "Initial IO desired/applied reconciliation converged"
            );
        },
        Ok(receipt) => {
            warn!(
                topology_current = receipt.topology_current(),
                authority_stable = receipt.authority_stable(),
                attempted_channels = receipt.attempted_channels(),
                "Initial IO desired/applied reconciliation is degraded and fenced"
            );
        },
        Err(error) => {
            error!(
                error_kind = ?error.kind(),
                "Initial IO desired/applied reconciliation failed closed"
            );
        },
    }
    let automatic_reconciliation_interval = Duration::from_millis(
        std::env::var("AETHER_IO_RECONCILIATION_INTERVAL_MS")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(2_000),
    );
    let automatic_reconciliation_shutdown = shutdown_token.clone();
    let automatic_reconciliation_handle = tokio::spawn(async move {
        aether_io::automatic_reconciliation::run_automatic_io_reconciliation(
            automatic_reconciliation,
            automatic_reconciliation_interval,
            automatic_reconciliation_shutdown,
        )
        .await;
    });

    let watchdog_reconciler: Arc<dyn aether_ports::ChannelReconciler> = channel_adapter.clone();
    let (cleanup_handle, cleanup_token) = start_cleanup_task(
        Arc::clone(&channel_manager),
        configured_count,
        Some(watchdog_reconciler),
    );
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
    let channel_mutator: Arc<dyn aether_ports::ChannelMutator> = channel_adapter.clone();
    let channel_reconciler: Arc<dyn aether_ports::ChannelReconciler> = channel_adapter;
    let channel_audit: Arc<dyn aether_ports::AuditSink> = Arc::new(
        aether_store_local::SqliteAuditSink::initialize(sqlite_pool.clone())
            .await
            .map_err(|error| {
                IoError::ConfigError(format!("Channel-management audit unavailable: {error}"))
            })?,
    );
    let channel_management = Arc::new(aether_application::ChannelManagementApplication::new(
        channel_mutator,
        Arc::clone(&channel_audit),
        aether_application::SafetyPolicy,
    ));
    let channel_reconciliation =
        Arc::new(aether_application::ChannelReconciliationApplication::new(
            channel_reconciler,
            Arc::clone(&channel_audit),
            aether_application::SafetyPolicy,
        ));
    let point_topology = Arc::new(aether_io::point_topology::PointTopologyApplication::new(
        sqlite_pool.clone(),
        channel_audit,
    ));
    let access_authenticator = Arc::new(
        aether_auth_jwt::AccessTokenAuthenticator::from_env().map_err(|error| {
            IoError::ConfigError(format!("Channel-management authentication: {error}"))
        })?,
    );
    let app = create_api_routes_with_channel_applications(
        Arc::clone(&channel_manager),
        sqlite_pool,
        Arc::clone(&command_tx_cache),
        channel_management,
        channel_reconciliation,
        point_topology,
        access_authenticator,
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

    match tokio::time::timeout(Duration::from_secs(2), automatic_reconciliation_handle).await {
        Ok(Ok(())) => info!("Automatic IO reconciliation task stopped"),
        Ok(Err(error)) => error!("Automatic IO reconciliation task failed: {error}"),
        Err(_) => error!("Automatic IO reconciliation task shutdown timed out"),
    }

    match tokio::time::timeout(Duration::from_secs(2), shm_heartbeat_handle).await {
        Ok(Ok(())) => info!("SHM writer heartbeat task stopped"),
        Ok(Err(error)) => error!("SHM writer heartbeat task failed: {error}"),
        Err(_) => error!("SHM writer heartbeat task shutdown timed out"),
    }
    match tokio::time::timeout(Duration::from_secs(2), channel_health_heartbeat_handle).await {
        Ok(Ok(())) => info!("channel-health SHM heartbeat task stopped"),
        Ok(Err(error)) => error!("channel-health SHM heartbeat task failed: {error}"),
        Err(_) => error!("channel-health SHM heartbeat task shutdown timed out"),
    }

    // Wait for SHM listener task to complete (if it was started)
    if let Some(handle) = shm_listener_handle {
        let _ = handle.await;
        info!("ShmCommandListener shutdown complete");
    }

    // Wait for SnapshotManager to complete (saves final snapshot)
    if let Some(handle) = snapshot_manager_handle {
        match tokio::time::timeout(std::time::Duration::from_secs(10), handle).await {
            Ok(Ok(Ok(()))) => info!("SnapshotManager shutdown complete"),
            Ok(Ok(Err(error))) => error!("SnapshotManager task failed: {error}"),
            Ok(Err(error)) => error!("SnapshotManager join failed: {error}"),
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
