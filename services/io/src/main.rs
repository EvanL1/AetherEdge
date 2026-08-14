//! `aether-io` — device protocol and field I/O service.
//!
//! A high-performance, async-first industrial communication service written in Rust.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::serve;
use clap::Parser;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

use aether_io::core::config::DEFAULT_PORT;
use common::service_bootstrap::ServiceInfo;
use errors::AetherResult;

// aether-io imports
use aether_io::{
    api::routes::{create_api_routes_with_channel_applications, set_service_start_time},
    core::{
        bootstrap::{self, Args},
        channels::ChannelManager,
        config::IoSqliteLoader,
    },
    error::IoError,
    runtime::{run_cleanup_task, shutdown_handler},
};
use aether_routing::load_routing_maps;
use aether_shm_bridge::{
    AcquisitionCommitObserver, PointWatchPublisher, ShmChannelHealthWriterHandle, ShmRuntimeConfig,
    ShmWriterHandle, SubscriptionBitmap, begin_topology_publication, bitmap_path_for_consumer,
    channel_health_path_from_shm, cleanup_orphan_generation_files, default_shm_path,
    point_watch_socket_from_shm, timestamp_ms,
};

async fn save_shm_snapshot(
    generation: Arc<aether_shm_bridge::ShmWriterGeneration>,
    path: std::path::PathBuf,
) -> anyhow::Result<()> {
    tokio::task::spawn_blocking(move || generation.save_snapshot(&path))
        .await
        .map_err(|error| anyhow::anyhow!("SHM snapshot worker failed: {error}"))?
        .map_err(|error| anyhow::anyhow!("SHM snapshot failed: {error}"))
}

async fn await_unit_task(name: &str, timeout: Duration, mut handle: tokio::task::JoinHandle<()>) {
    match tokio::time::timeout(timeout, &mut handle).await {
        Ok(Ok(())) => info!("{name} stopped"),
        Ok(Err(error)) => error!("{name} task failed: {error}"),
        Err(_) => {
            error!("{name} shutdown timed out; aborting task");
            handle.abort();
            let _ = handle.await;
        },
    }
}

/// Cancels every startup-owned plane if composition exits before the runtime
/// supervisor has taken responsibility for their handles.
struct StartupCancellationGuard {
    service: CancellationToken,
    shm: CancellationToken,
    listener_shutdown: tokio::sync::watch::Sender<bool>,
    snapshot_shutdown: Option<tokio::sync::watch::Sender<bool>>,
    armed: bool,
}

impl StartupCancellationGuard {
    fn new(
        service: CancellationToken,
        shm: CancellationToken,
        listener_shutdown: tokio::sync::watch::Sender<bool>,
    ) -> Self {
        Self {
            service,
            shm,
            listener_shutdown,
            snapshot_shutdown: None,
            armed: true,
        }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }

    fn supervise_snapshot(&mut self, shutdown: tokio::sync::watch::Sender<bool>) {
        self.snapshot_shutdown = Some(shutdown);
    }
}

impl Drop for StartupCancellationGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        self.service.cancel();
        self.shm.cancel();
        let _ = self.listener_shutdown.send(true);
        if let Some(shutdown) = &self.snapshot_shutdown {
            let _ = shutdown.send(true);
        }
    }
}

/// Resolve the SHM snapshot period from `SHM_SNAPSHOT_INTERVAL`.
///
/// `tokio::time::interval` panics on a zero period and the release profile sets
/// `panic = "abort"`, so an unclamped `0` would kill the IO service at startup.
fn snapshot_interval(configured_secs: Option<u64>) -> Duration {
    Duration::from_secs(configured_secs.unwrap_or(300).max(1))
}

fn internal_api_address(value: &str) -> Result<SocketAddr, IoError> {
    let address = value.parse().map_err(|error| {
        IoError::ConfigError(format!("Invalid bind address '{value}': {error}"))
    })?;
    common::require_loopback_bind_address(address).map_err(|error| {
        IoError::ConfigError(format!(
            "Invalid internal API bind address '{value}': {error}"
        ))
    })
}

/// Reserve the API endpoint synchronously before any runtime task or device
/// projection is created.
///
/// Keeping this helper side-effect free beyond the socket itself makes an
/// occupied port a clean startup failure: dropping the returned error cannot
/// leave a detached Tokio task or a connected device runtime behind.
fn bind_api_listener(addr: SocketAddr) -> Result<tokio::net::TcpListener, IoError> {
    let socket = if addr.is_ipv4() {
        tokio::net::TcpSocket::new_v4()
    } else {
        tokio::net::TcpSocket::new_v6()
    }
    .map_err(|e| IoError::ConnectionError(format!("Failed to create socket: {e}")))?;
    socket
        .set_reuseaddr(true)
        .map_err(|e| IoError::ConnectionError(format!("Failed to set SO_REUSEADDR: {e}")))?;
    socket
        .bind(addr)
        .map_err(|e| IoError::ConnectionError(format!("Failed to bind to {addr}: {e}")))?;
    socket
        .listen(1024)
        .map_err(|e| IoError::ConnectionError(format!("Failed to listen: {e}")))
}

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

    // Resolve the authoritative database once and share one process-wide pool.
    let db_path = service_args.get_db_path("aether-io");
    info!(
        "Loading configuration from unified SQLite database: {}",
        db_path
    );
    let sqlite_options =
        common::bootstrap_database::sqlite_connect_options(&db_path).create_if_missing(false);
    let sqlite_pool = sqlx::sqlite::SqlitePoolOptions::new()
        .connect_with(sqlite_options)
        .await
        .map_err(|e| IoError::ConfigError(format!("Failed to create SQLite pool: {}", e)))?;
    let sqlite_loader = IoSqliteLoader::from_pool(sqlite_pool.clone());
    let (service_config, api_config) = sqlite_loader.load_service_config().await?;

    // Validation mode: validate and exit
    if args.validate {
        let channels = sqlite_loader.load_runtime_channels().await?;
        bootstrap::validate_configuration(&service_config, &channels)?;
        info!("Validation completed successfully");
        return Ok(());
    }

    // Reserve the externally visible process identity before SHM publishers,
    // background tasks, or initial reconciliation can produce side effects.
    // Every subsequent early return simply drops this unspawned listener.
    let bind_address =
        bootstrap::determine_bind_address(args.bind_address, &api_config.host, api_config.port);
    let addr = internal_api_address(&bind_address)?;
    let listener = bind_api_listener(addr)?;
    let command_uds_path = std::env::var("AETHER_M2C_SOCKET").ok();
    let prepared_command_listener =
        aether_io::core::channels::shm_listener::ShmCommandListener::prepare_path(
            command_uds_path.as_deref(),
        )
        .map_err(|error| {
            IoError::ConnectionError(format!("Failed to reserve M2C command socket: {error}"))
        })?;
    let (shm_listener_shutdown_tx, shm_listener_shutdown_rx) = tokio::sync::watch::channel(false);

    // Validate every mandatory command-plane dependency before SHM publishers,
    // device runtimes, or background tasks are created. A missing JWT secret or
    // unavailable durable audit sink must be a side-effect-free startup failure,
    // not a late return after initial reconciliation has already touched devices.
    let access_authenticator = Arc::new(
        aether_auth_jwt::AccessTokenAuthenticator::from_env().map_err(|error| {
            IoError::ConfigError(format!("Channel-management authentication: {error}"))
        })?,
    );
    let channel_audit: Arc<dyn aether_ports::AuditSink> = Arc::new(
        aether_store_local::SqliteAuditSink::initialize(sqlite_pool.clone())
            .await
            .map_err(|error| {
                IoError::ConfigError(format!("Channel-management audit unavailable: {error}"))
            })?,
    );
    let command_ledger = Arc::new(
        aether_io::core::channels::command_ledger::CommandLedger::initialize(sqlite_pool.clone())
            .await
            .map_err(|error| {
                IoError::ConfigError(format!("durable command ledger unavailable: {error}"))
            })?,
    );

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

    // Service coordination must stop before channels, while SHM heartbeats and
    // PointWatch remain alive through the final snapshot.
    let service_shutdown_token = CancellationToken::new();
    let shm_shutdown_token = CancellationToken::new();
    let mut startup_cancellation = StartupCancellationGuard::new(
        service_shutdown_token.clone(),
        shm_shutdown_token.clone(),
        shm_listener_shutdown_tx.clone(),
    );

    let initial_shm_topology = aether_sqlite_topology::load_sqlite_shm_topology(&sqlite_pool)
        .await
        .map_err(|error| {
            IoError::config(format!(
                "failed to load authoritative SHM topology from SQLite: {error}"
            ))
        })?;
    let max_slots = u32::try_from(initial_shm_topology.max_slots())
        .map_err(|_| IoError::config("shared_memory.max_slots exceeds the runtime u32 capacity"))?;
    let (initial_point_manifest, initial_health_manifest) = initial_shm_topology.into_manifests();
    // ============ Phase 2.5: publish the authoritative SHM generation ============
    let (
        shm_handle,
        prepared_point_watch_drains,
        initial_topology_publication,
        initial_publication_epoch,
        initial_health_path,
        snapshot_path,
        snapshot_interval,
    ) = {
        let shm_path = default_shm_path();
        let health_path = std::env::var("AETHER_CHANNEL_HEALTH_SHM_PATH")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|_| channel_health_path_from_shm(&shm_path));
        let runtime_config = ShmRuntimeConfig::new(&shm_path, max_slots);
        let manifest = Arc::new(initial_point_manifest);
        let snapshot_path = std::env::var("SHM_SNAPSHOT_PATH")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|_| std::path::PathBuf::from("data/shm-snapshot.bin"));
        let snapshot_interval = snapshot_interval(
            std::env::var("SHM_SNAPSHOT_INTERVAL")
                .ok()
                .and_then(|value| value.parse::<u64>().ok()),
        );
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

        let mut point_watch_targets = Vec::new();
        for (consumer, variable) in [
            ("automation", "AETHER_AUTOMATION_POINT_WATCH_SOCKET"),
            ("alarm", "AETHER_ALARM_POINT_WATCH_SOCKET"),
            ("api", "AETHER_API_POINT_WATCH_SOCKET"),
        ] {
            match SubscriptionBitmap::open_or_create(
                &bitmap_path_for_consumer(&shm_path, consumer),
                max_slots as usize,
            ) {
                Ok(bitmap) => {
                    let socket = std::env::var(variable)
                        .map(std::path::PathBuf::from)
                        .unwrap_or_else(|_| point_watch_socket_from_shm(&shm_path, consumer));
                    point_watch_targets.push((Arc::new(bitmap), socket));
                },
                Err(error) => warn!(
                    "{consumer} PointWatch target disabled (bitmap initialization failed): {error}"
                ),
            }
        }
        let point_watch = if point_watch_targets.is_empty() {
            warn!("PointWatch disabled because no consumer bitmap could be initialized");
            None
        } else {
            Some(PointWatchPublisher::prepare_with_fanout(
                point_watch_targets,
            ))
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
        let handle = match ShmWriterHandle::create(
            runtime_config.clone(),
            Arc::clone(&manifest),
            restore_path,
            observer.clone(),
            publication_epoch,
        ) {
            Ok(handle) => handle,
            Err(error) if restore_path.is_some() => {
                warn!("Snapshot restore failed, creating fresh: {error}");
                ShmWriterHandle::create(runtime_config, manifest, None, observer, publication_epoch)
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

        let prepared_point_watch_drains = point_watch.map(|(_, drains)| drains);
        (
            handle,
            prepared_point_watch_drains,
            topology_publication,
            publication_epoch,
            health_path,
            snapshot_path,
            snapshot_interval,
        )
    };

    let channel_health_writer = {
        let health_path = initial_health_path;
        let writer = Arc::new(ShmChannelHealthWriterHandle::empty(&health_path));
        match writer.rebuild(Arc::new(initial_health_manifest), initial_publication_epoch) {
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

    // Create channel manager over the mandatory SHM writer.
    // Lock-free architecture - no RwLock wrapper needed
    let channel_manager = ChannelManager::with_shared_memory(
        routing_cache,
        Arc::clone(&shm_handle),
        Some(Arc::clone(&channel_health_writer)),
    )?;

    // Configure SHM listener for event-driven M2C dispatch.
    let channel_manager = channel_manager.with_shm_listener_path(
        shm_listener_shutdown_rx,
        command_uds_path.as_deref(),
        command_ledger,
    );

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
    let command_listener = channel_manager
        .shm_listener()
        .cloned()
        .ok_or_else(|| IoError::state("SHM command listener is not configured"))?;

    info!("Starting {} service", service_config.name);

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

    // Every fallible authority and listener reservation has succeeded. Start
    // asynchronous SHM workers only now, after initial device projection can
    // no longer be followed by a normal startup error that detaches them.
    let point_watch_drain_handle =
        prepared_point_watch_drains.map(|prepared| prepared.spawn(shm_shutdown_token.clone()));
    let (snapshot_shutdown_tx, mut snapshot_shutdown_rx) = tokio::sync::watch::channel(false);
    startup_cancellation.supervise_snapshot(snapshot_shutdown_tx.clone());
    let snapshot_manager_handle = {
        let handle = Arc::clone(&shm_handle);
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(snapshot_interval);
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            // Tokio intervals tick immediately once. Consume that tick so
            // startup restore is not immediately overwritten by a redundant
            // snapshot.
            interval.tick().await;
            loop {
                tokio::select! {
                    _ = interval.tick() => {
                        if let Some(generation) = handle.generation()
                            && let Err(error) = save_shm_snapshot(generation, snapshot_path.clone()).await
                        {
                            warn!("periodic SHM snapshot failed: {error}");
                        }
                    },
                    changed = snapshot_shutdown_rx.changed() => {
                        if changed.is_err() || *snapshot_shutdown_rx.borrow() {
                            break;
                        }
                    },
                }
            }
            if let Some(generation) = handle.generation() {
                save_shm_snapshot(generation, snapshot_path).await?;
            }
            Ok::<(), anyhow::Error>(())
        })
    };

    // Writer liveness belongs to the SHM authority itself, not to any
    // commissioned channel. Keep it fresh even on the intentionally empty
    // default site so readers can distinguish a live writer from a valid but
    // abandoned mmap file.
    let shm_heartbeat_handle = {
        let heartbeat_handle = Arc::clone(&shm_handle);
        let heartbeat_shutdown = shm_shutdown_token.clone();
        let failure_shutdown = service_shutdown_token.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(1));
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tokio::select! {
                    _ = interval.tick() => {
                        if let Some(generation) = heartbeat_handle.generation() {
                            generation.acquisition_writer().update_heartbeat(timestamp_ms());
                        } else {
                            error!("authoritative SHM writer disappeared; stopping IO service");
                            failure_shutdown.cancel();
                            break;
                        }
                    },
                    _ = heartbeat_shutdown.cancelled() => break,
                }
            }
        })
    };
    let channel_health_heartbeat_handle = {
        let writer = Arc::clone(&channel_health_writer);
        let heartbeat_shutdown = shm_shutdown_token.clone();
        let failure_shutdown = service_shutdown_token.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(1));
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tokio::select! {
                    _ = interval.tick() => {
                        if let Err(error) = writer.update_heartbeat(timestamp_ms()) {
                            error!("channel-health SHM heartbeat failed: {error}");
                            failure_shutdown.cancel();
                            break;
                        }
                    },
                    _ = heartbeat_shutdown.cancelled() => break,
                }
            }
        })
    };

    // Every coordinator whose exit would invalidate service readiness is owned
    // by one supervisor. An unexpected exit cancels `service_shutdown_token`,
    // which enters the same ordered drain path as SIGINT/SIGTERM.
    let mut coordinator_supervisor =
        common::task_supervisor::CriticalTaskSupervisor::new(Duration::from_secs(5));

    // Reconciliation creates every authoritative runtime projection. Start the
    // command listener only after those channels have been registered.
    coordinator_supervisor.spawn("io-command-listener", async move {
        if let Err(error) = command_listener
            .run_prepared(prepared_command_listener)
            .await
        {
            error!("ShmCommandListener failed: {error}");
        }
    });
    info!("ShmCommandListener started for event-driven M2C dispatch (~1-2ms latency)");

    let automatic_reconciliation_interval = Duration::from_millis(
        std::env::var("AETHER_IO_RECONCILIATION_INTERVAL_MS")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(2_000),
    );
    let automatic_reconciliation_shutdown = service_shutdown_token.clone();
    coordinator_supervisor.spawn("io-automatic-reconciliation", async move {
        aether_io::automatic_reconciliation::run_periodic_io_reconciliation(
            automatic_reconciliation,
            automatic_reconciliation_interval,
            automatic_reconciliation_shutdown,
        )
        .await;
    });

    let watchdog_reconciler: Arc<dyn aether_ports::ChannelReconciler> = channel_adapter.clone();
    let cleanup_token = CancellationToken::new();
    let cleanup_shutdown = cleanup_token.clone();
    let cleanup_manager = Arc::clone(&channel_manager);
    coordinator_supervisor.spawn("io-cleanup-watchdog", async move {
        run_cleanup_task(cleanup_manager, Some(watchdog_reconciler), cleanup_shutdown).await;
    });
    // Start routing cache polling task (auto-detect routing changes from SQLite)
    let poll_pool = sqlite_pool.clone();
    let poll_cache = Arc::clone(&channel_manager.routing_cache);
    let poll_token = service_shutdown_token.clone();
    coordinator_supervisor.spawn("io-routing-poll", async move {
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
    let app = create_api_routes_with_channel_applications(
        Arc::clone(&channel_manager),
        sqlite_pool,
        channel_management,
        channel_reconciliation,
        point_topology,
        access_authenticator,
    );

    // The loopback service publishes /openapi.json; only aether-api owns the
    // externally reachable Swagger UI.
    // Note: HTTP request logging middleware is applied by the API composition root.

    info!("API server listening on http://{}", addr);
    info!("Health check: http://{}/health", addr);

    let server = serve(listener, app);
    let server_token = service_shutdown_token.clone();
    coordinator_supervisor.spawn("io-api-server", async move {
        let shutdown = async move { server_token.cancelled().await };
        if let Err(e) = server.with_graceful_shutdown(shutdown).await {
            error!("Server error: {}", e);
        }
    });
    let supervisor_shutdown = service_shutdown_token.clone();
    let supervisor_handle =
        tokio::spawn(async move { coordinator_supervisor.run(supervisor_shutdown).await });
    // All spawned work now has an owned handle and participates in the normal
    // ordered drain path. Do not let the startup fallback fire during routine
    // signal-driven shutdown.
    startup_cancellation.disarm();

    // An external signal and any unexpected critical-task exit converge on the
    // same drain path. For an external signal, establish the creation fence
    // before publishing task cancellation; an internal failure has already
    // cancelled the service token and is fenced immediately on observation.
    let shutdown_reason = tokio::select! {
        _ = common::shutdown::wait_for_shutdown() => "shutdown signal",
        _ = service_shutdown_token.cancelled() => "critical coordinator exit",
    };

    info!("Received {shutdown_reason}, quiescing command and reconciliation planes...");

    // Establish the lifecycle fence before notifying asynchronous tasks. This
    // waits for any in-flight channel creation to finish publication, then
    // prevents every subsequent reconciliation attempt from recreating it.
    if let Err(error) = channel_manager.begin_shutdown() {
        error!("Failed to establish channel shutdown fence: {error}");
    }

    // Stop new UDS commands, API requests, automatic/watchdog reconciliation,
    // and routing refreshes. Await them before enumerating channel IDs.
    // Publish normal shutdown to the supervisor first. If the listener sees
    // its watch signal and returns before this token is cancelled, the
    // supervisor would misclassify a planned exit as a critical failure.
    service_shutdown_token.cancel();
    let _ = shm_listener_shutdown_tx.send(true);
    cleanup_token.cancel();

    // The supervisor owns and observes every coordinator task. It drains them
    // before channel writers are stopped, or aborts and observes stragglers at
    // the common deadline. Preserve any failure until after the remaining
    // channels/snapshot/SHM cleanup has completed.
    let supervisor_result = match supervisor_handle.await {
        Ok(result) => result,
        Err(error) => Err(anyhow::anyhow!("IO task supervisor join failed: {error}")),
    };

    // With every creator and command producer quiesced, the channel set is
    // stable and can be drained without a late recreation race.
    let channel_shutdown_result = shutdown_handler(Arc::clone(&channel_manager)).await;

    // Capture the final authoritative state only after all channel writers stop.
    let _ = snapshot_shutdown_tx.send(true);
    info!("Signaled SnapshotManager to save final snapshot");

    // Wait for SnapshotManager to complete (saves final snapshot). Its file IO
    // runs in `spawn_blocking`; aborting the async wrapper cannot cancel an
    // already-running blocking job and would let it outlive SHM shutdown.
    // Preserve any error, finish the remaining cleanup, then return it.
    let snapshot_result = match snapshot_manager_handle.await {
        Ok(Ok(())) => {
            info!("SnapshotManager shutdown complete");
            Ok(())
        },
        Ok(Err(error)) => Err(error),
        Err(error) => Err(anyhow::anyhow!("SnapshotManager join failed: {error}")),
    };

    // SHM remains alive through the final snapshot and PointWatch delivery.
    // Only now stop authority heartbeats and let PointWatch drain its queue.
    shm_shutdown_token.cancel();
    await_unit_task(
        "SHM writer heartbeat",
        Duration::from_secs(2),
        shm_heartbeat_handle,
    )
    .await;
    await_unit_task(
        "channel-health SHM heartbeat",
        Duration::from_secs(2),
        channel_health_heartbeat_handle,
    )
    .await;

    // Wait for PointWatch drain task to flush remaining events and stop.
    if let Some(mut handle) = point_watch_drain_handle {
        match tokio::time::timeout(std::time::Duration::from_secs(2), &mut handle).await {
            Ok(_) => info!("PointWatch drain task stopped"),
            Err(_) => {
                warn!("PointWatch drain task shutdown timed out; aborting task");
                handle.abort();
                let _ = handle.await;
            },
        }
    }

    supervisor_result?;
    snapshot_result?;
    channel_shutdown_result?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn startup_failure_guard_stops_every_prepared_plane() {
        let service = CancellationToken::new();
        let shm = CancellationToken::new();
        let (listener_tx, mut listener_rx) = tokio::sync::watch::channel(false);
        let (snapshot_tx, mut snapshot_rx) = tokio::sync::watch::channel(false);
        let stopped = Arc::new(std::sync::atomic::AtomicUsize::new(0));

        let service_waiter = {
            let token = service.clone();
            let stopped = Arc::clone(&stopped);
            tokio::spawn(async move {
                token.cancelled().await;
                stopped.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            })
        };
        let shm_waiter = {
            let token = shm.clone();
            let stopped = Arc::clone(&stopped);
            tokio::spawn(async move {
                token.cancelled().await;
                stopped.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            })
        };
        let listener_waiter = {
            let stopped = Arc::clone(&stopped);
            tokio::spawn(async move {
                listener_rx.changed().await.expect("listener shutdown");
                assert!(*listener_rx.borrow());
                stopped.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            })
        };
        let snapshot_waiter = {
            let stopped = Arc::clone(&stopped);
            tokio::spawn(async move {
                snapshot_rx.changed().await.expect("snapshot shutdown");
                assert!(*snapshot_rx.borrow());
                stopped.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            })
        };

        {
            let mut guard = StartupCancellationGuard::new(service, shm, listener_tx);
            guard.supervise_snapshot(snapshot_tx);
            // Leaving this scope simulates any `?` during startup composition.
        }

        tokio::time::timeout(Duration::from_secs(1), async {
            let _ = tokio::join!(service_waiter, shm_waiter, listener_waiter, snapshot_waiter);
        })
        .await
        .expect("startup-owned tasks must observe cancellation promptly");
        assert_eq!(stopped.load(std::sync::atomic::Ordering::Relaxed), 4);
    }

    #[test]
    fn occupied_api_port_fails_before_any_async_task_can_start() {
        let occupied = std::net::TcpListener::bind("127.0.0.1:0").expect("occupy test port");
        let addr = occupied.local_addr().expect("occupied address");

        let error = bind_api_listener(addr).expect_err("occupied port must fail startup");

        assert!(error.to_string().contains("Failed to bind"));
        // `bind_api_listener` is synchronous and creates no Tokio tasks. Main
        // invokes it before PointWatch, snapshot, heartbeat, channel, listener,
        // reconciliation, routing, or HTTP task construction.
    }

    #[test]
    fn internal_api_address_accepts_ipv4_and_ipv6_loopback() {
        assert_eq!(
            internal_api_address("127.0.0.1:6001")
                .expect("IPv4 loopback")
                .to_string(),
            "127.0.0.1:6001"
        );
        assert_eq!(
            internal_api_address("127.99.1.2:6001")
                .expect("IPv4 loopback range")
                .to_string(),
            "127.99.1.2:6001"
        );
        assert_eq!(
            internal_api_address("[::1]:6001")
                .expect("IPv6 loopback")
                .to_string(),
            "[::1]:6001"
        );
    }

    #[test]
    fn internal_api_address_rejects_unspecified_listeners() {
        for exposed in ["0.0.0.0:6001", "[::]:6001"] {
            assert!(
                internal_api_address(exposed).is_err(),
                "{exposed} must not expose aether-io"
            );
        }
    }

    #[tokio::test]
    async fn api_listener_accepts_an_ipv6_bind_address_when_loopback_is_available() {
        if std::net::TcpListener::bind("[::1]:0").is_err() {
            // Some CI kernels disable IPv6 entirely. In that environment there
            // is no valid IPv6 listener contract to exercise.
            return;
        }

        let addr: SocketAddr = "[::1]:0".parse().expect("IPv6 loopback address");
        let listener = bind_api_listener(addr).expect("bind IPv6 API listener");

        assert!(listener.local_addr().expect("listener address").is_ipv6());
    }
}

#[cfg(test)]
mod snapshot_interval_tests {
    use super::snapshot_interval;
    use std::time::Duration;

    #[test]
    fn a_zero_configured_interval_is_clamped_instead_of_panicking() {
        // `SHM_SNAPSHOT_INTERVAL=0` reached `tokio::time::interval`, which panics —
        // fatal under the workspace release profile's `panic = "abort"`.
        assert_eq!(snapshot_interval(Some(0)), Duration::from_secs(1));
    }

    #[test]
    fn a_configured_interval_is_preserved() {
        assert_eq!(snapshot_interval(Some(30)), Duration::from_secs(30));
    }

    #[test]
    fn an_absent_setting_uses_the_default() {
        assert_eq!(snapshot_interval(None), Duration::from_secs(300));
    }
}
