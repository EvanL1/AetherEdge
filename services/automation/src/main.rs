//! `aether-automation` — instance, rule, and action orchestration service.
//!
//! Owns commissioned instances, logical routing, and deterministic rules.

use std::{path::PathBuf, sync::Arc, time::Duration};

use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

// aether-automation imports
use aether_automation::infra::{
    application_control::RuleActionApplication, rule_live_state::ShmRuleLiveState,
    rule_queries::RuleQueries, rule_runtime::RuleRuntimeCoordinator,
};
use aether_automation::{
    AutomationError, Result,
    api::rule_routes::{RuleEngineState, create_rule_routes},
    bootstrap, routes,
};
use aether_calc::MemoryStateStore;
use aether_rules::{
    DEFAULT_RULE_EXECUTION_TIMEOUT, DEFAULT_TICK_MS, MAX_RULE_CONCURRENCY, PointWatchDispatcher,
    PointWatchHint, RuleScheduler, WatchEvent,
};
use aether_shm_bridge::{
    PointWatchEvent, PointWatchEventListener, PreparedPointWatchEventListener, SubscriptionBitmap,
    bitmap_path_for_consumer, default_shm_path, point_watch_socket_from_shm,
};
use aether_sqlite_topology::load_sqlite_shm_capacity;

#[cfg(feature = "openapi")]
async fn openapi_document() -> axum::Json<utoipa::openapi::OpenApi> {
    axum::Json(routes::openapi_document())
}

const MAX_RULE_TICK_MS: u64 = 60_000;
const MAX_RULE_TIMEOUT_MS: u64 = 120_000;

async fn optional_global_u64(pool: &sqlx::SqlitePool, key: &str) -> Result<Option<u64>> {
    let raw = sqlx::query_scalar::<_, String>(
        "SELECT value FROM service_config WHERE service_name = 'global' AND key = ?",
    )
    .bind(key)
    .fetch_optional(pool)
    .await
    .map_err(|error| AutomationError::DatabaseError(format!("failed to load {key}: {error}")))?;
    raw.map(|value| {
        value.parse::<u64>().map_err(|error| {
            AutomationError::InvalidConfig(format!("{key} must be an unsigned integer: {error}"))
        })
    })
    .transpose()
}

fn bounded_config(value: u64, key: &str, min: u64, max: u64) -> Result<u64> {
    if !(min..=max).contains(&value) {
        return Err(AutomationError::InvalidConfig(format!(
            "{key} must be in {min}..={max}"
        )));
    }
    Ok(value)
}

async fn run_command_notifier_probe(
    sink: Arc<aether_shm_bridge::ShmDeviceCommandSink>,
    shutdown: CancellationToken,
) {
    let mut interval = tokio::time::interval(Duration::from_secs(1));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            biased;
            _ = shutdown.cancelled() => break,
            _ = interval.tick() => {
                if let Err(error) = sink.probe_notifier().await {
                    debug!("Command notifier probe did not become ready: {error}");
                }
            },
        }
    }
}

async fn run_command_topology_rebuild(
    sink: Arc<aether_shm_bridge::ShmDeviceCommandSink>,
    pool: sqlx::SqlitePool,
    topology: Arc<aether_automation::infra::runtime_topology::AutomationTopologyHandle>,
    shutdown: CancellationToken,
) {
    let rebuild_notify = sink.rebuild_trigger();
    loop {
        tokio::select! {
            _ = rebuild_notify.notified() => {},
            _ = shutdown.cancelled() => break,
        }
        info!("SHM rebuild triggered — refreshing the complete automation topology...");
        const MAX_RETRIES: u32 = 10;
        const BASE_DELAY_MS: u64 = 1_000;
        const MAX_DELAY_MS: u64 = 15_000;
        let mut retry_count = 0_u32;
        let ok = loop {
            match topology.refresh(&pool).await {
                Ok(_) => {
                    info!("Complete automation topology restored successfully");
                    break true;
                },
                Err(error) if retry_count < MAX_RETRIES => {
                    let delay = (BASE_DELAY_MS * 2_u64.pow(retry_count)).min(MAX_DELAY_MS);
                    info!(
                        "Automation topology refresh retry {}/{} in {}ms: {}",
                        retry_count + 1,
                        MAX_RETRIES,
                        delay,
                        error
                    );
                    tokio::select! {
                        _ = tokio::time::sleep(Duration::from_millis(delay)) => {},
                        _ = shutdown.cancelled() => return,
                    }
                    retry_count += 1;
                },
                Err(error) => {
                    warn!(
                        "Automation topology refresh failed after {} retries: {}. A later unavailable command will start a new bounded cycle.",
                        MAX_RETRIES, error
                    );
                    break false;
                },
            }
        };
        if ok {
            info!("SHM auto-rebuild complete — M2C dispatch restored");
        }
    }
}

#[cfg(unix)]
async fn run_shm_inode_watch(
    sink: Arc<aether_shm_bridge::ShmDeviceCommandSink>,
    path: PathBuf,
    shutdown: CancellationToken,
) {
    use std::os::unix::fs::MetadataExt;

    const WATCH_INTERVAL: Duration = Duration::from_secs(5);
    let mut last_inode = std::fs::metadata(&path).ok().map(|metadata| metadata.ino());
    if let Some(inode) = last_inode {
        info!("SHM inode watcher: baseline inode={inode} for {path:?}");
    } else {
        info!("SHM inode watcher: canonical path {path:?} not yet present");
    }

    loop {
        tokio::select! {
            _ = tokio::time::sleep(WATCH_INTERVAL) => {},
            _ = shutdown.cancelled() => break,
        }
        let current_inode = match std::fs::metadata(&path) {
            Ok(metadata) => Some(metadata.ino()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => {
                warn!("SHM inode watcher: stat {path:?} failed: {error}");
                continue;
            },
        };
        match (last_inode, current_inode) {
            (Some(old), Some(new)) if old != new => {
                info!("SHM inode watcher: canonical inode changed {old} -> {new}");
                last_inode = Some(new);
                sink.invalidate_and_rebuild();
            },
            (None, Some(new)) => {
                info!("SHM inode watcher: canonical path appeared (inode={new})");
                last_inode = Some(new);
            },
            (Some(_), None) => {
                warn!("SHM inode watcher: canonical path {path:?} disappeared");
            },
            _ => {},
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    // Create service info
    let service_info = bootstrap::create_service_info();

    // Create application state with all initialized components
    let composition = bootstrap::compose_automation(&service_info).await?;
    let state = composition.state;
    let sqlite_pool = composition.sqlite_pool;
    let runtime_topology = composition.runtime_topology;

    // Validate the listener address before starting any background task so a
    // configuration error cannot bypass coordinated cancellation and drain.
    let addr = match bootstrap::internal_api_bind_address(&state.config.api) {
        Ok(addr) => addr,
        Err(error) => {
            common::logging::shutdown_logging_tasks().await;
            return Err(error);
        },
    };
    let listener = match common::shutdown::bind_http_listener(addr) {
        Ok(listener) => listener,
        Err(error) => {
            common::logging::shutdown_logging_tasks().await;
            return Err(AutomationError::InvalidConfig(format!(
                "failed to bind internal API address {addr} before runtime activation: {error}"
            )));
        },
    };

    // Initialize cancellation and supervision only after all fallible listener
    // configuration has been validated.
    let shutdown_token = CancellationToken::new();
    let mut supervisor =
        common::task_supervisor::CriticalTaskSupervisor::new(Duration::from_secs(5));
    debug!("Shutdown token initialized");

    // Create API routes using the routes module. The gateway is the only
    // process that serves Swagger UI; this loopback service publishes its spec.
    let app = routes::create_routes(Arc::clone(&state));

    // ============================================================================
    // Initialize Rule Engine (integrated on port 6002)
    // ============================================================================
    // Load tick_ms from global config (SQLite key-value table)
    let tick_ms = bounded_config(
        optional_global_u64(&sqlite_pool, "rules.tick_ms")
            .await?
            .unwrap_or(DEFAULT_TICK_MS),
        "rules.tick_ms",
        1,
        MAX_RULE_TICK_MS,
    )?;

    debug!("Rule scheduler tick_ms: {}", tick_ms);

    let shm_path = default_shm_path();
    debug!("SHM path: {}", shm_path.display());

    match runtime_topology.refresh(&sqlite_pool).await {
        Ok(_) => info!("Coherent point/health/routing topology configured"),
        Err(error) => warn!(
            "IO runtime topology is not ready; automation started in fail-closed degraded mode: {error}"
        ),
    }
    let topology_refresh_interval = Duration::from_millis(
        std::env::var("SHM_TOPOLOGY_REFRESH_INTERVAL_MS")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(1_000)
            .max(100),
    );

    // UDS notification is self-healing. Its receipt proves IO durably admitted
    // the CommandId and exact payload; it is not a physical-device outcome.
    let m2c_socket = std::env::var("AETHER_M2C_SOCKET")
        .unwrap_or_else(|_| aether_shm_bridge::DEFAULT_COMMAND_UDS_PATH.to_string());
    state
        .shm_dispatch
        .configure_notifier(&m2c_socket)
        .await
        .map_err(|error| {
            AutomationError::DispatchDegraded(format!(
                "failed to configure command UDS notifier: {error}"
            ))
        })?;
    info!("Durable command notifier configured for {m2c_socket}; reconnect is automatic");

    // Complete every fallible startup step before any long-running task is
    // spawned. Otherwise an audit-store error would bypass coordinated drain.
    let rule_audit = aether_store_local::SqliteAuditSink::initialize(sqlite_pool.clone())
        .await
        .map_err(|error| AutomationError::DatabaseError(error.to_string()))?;

    // Load max_concurrency from global config (SQLite key-value table)
    let max_concurrency = usize::try_from(bounded_config(
        optional_global_u64(&sqlite_pool, "rules.max_concurrency")
            .await?
            .unwrap_or(4),
        "rules.max_concurrency",
        1,
        MAX_RULE_CONCURRENCY as u64,
    )?)
    .map_err(|_| AutomationError::InvalidConfig("rules.max_concurrency is too large".into()))?;
    let rule_execution_timeout_ms = bounded_config(
        optional_global_u64(&sqlite_pool, "rules.execution_timeout_ms")
            .await?
            .unwrap_or(DEFAULT_RULE_EXECUTION_TIMEOUT.as_millis() as u64),
        "rules.execution_timeout_ms",
        1,
        MAX_RULE_TIMEOUT_MS,
    )?;

    // ── PointWatch bootstrap (automation side) ──────────────────────────────────────
    // PointWatch is an optional latency optimization and may fall back to
    // scheduler ticks while the offline-first SHM topology reconnects.
    //
    // 1. Open the SubscriptionBitmap created by io (automation writes bits,
    //    io reads them in the hot path).
    // 2. Create a PointWatchListener UDS server that receives PointWatchEvents
    //    from io's drain task.
    // 3. Create a PointWatchDispatcher (subscription index + WatchEvent forwarder).
    // 4. Wire the WatchEvent receiver into RuleScheduler via set_watch_receiver.
    // 5. After rules load, call rebuild_point_watch to populate the subscription index.
    //
    // Graceful degradation: any failure disables the event-driven path; automation
    // still works via the 100 ms tick fallback.
    // PointWatch bootstrap result: all four values are None if bitmap open
    // fails (graceful degradation).
    //
    // Returned values:
    //   pw_bitmap    — mmap'd subscription bitmap (automation sets bits, io reads)
    //   pw_dispatcher — subscription index; call rebuild_point_watch after load_rules
    //   pw_event_rx  — raw PointWatchEvent channel from PointWatchListener
    //   pw_watch_rx  — WatchEvent channel wired into RuleScheduler
    //
    // After load_rules: call rebuild_point_watch on pw_dispatcher, then spawn the
    // bridge task that reads pw_event_rx and calls dispatcher.dispatch() → pw_watch_rx.
    type PwInitResult = (
        Option<Arc<SubscriptionBitmap>>,
        Option<PointWatchDispatcher>,
        Option<PreparedPointWatchEventListener>,
        Option<tokio::sync::mpsc::Receiver<PointWatchEvent>>,
        Option<tokio::sync::mpsc::Receiver<WatchEvent>>,
    );
    let point_watch_capacity = match load_sqlite_shm_capacity(&sqlite_pool).await {
        Ok(capacity) => Some(capacity),
        Err(error) => {
            warn!("PointWatch disabled (SHM capacity is unavailable): {error}");
            None
        },
    };
    let (pw_bitmap, pw_dispatcher, pw_listener, pw_event_rx, pw_watch_rx): PwInitResult = {
        let bitmap_path = bitmap_path_for_consumer(&shm_path, "automation");
        let bitmap = point_watch_capacity.and_then(|capacity| {
            match SubscriptionBitmap::open_or_create(&bitmap_path, capacity) {
                Ok(bitmap) => Some(bitmap),
                Err(error) => {
                    warn!("PointWatch disabled (bitmap initialization failed): {error}");
                    None
                },
            }
        });
        match bitmap {
            Some(bitmap) => {
                let bitmap = Arc::new(bitmap);

                // event_rx: raw PointWatchEvents forwarded from the UDS socket.
                let point_watch_socket = std::env::var("AETHER_AUTOMATION_POINT_WATCH_SOCKET")
                    .map(PathBuf::from)
                    .unwrap_or_else(|_| point_watch_socket_from_shm(&shm_path, "automation"));
                let (listener, event_rx) =
                    PointWatchEventListener::new(&point_watch_socket, shutdown_token.clone());
                info!(
                    "PointWatchListener binding ({})",
                    point_watch_socket.display()
                );

                match listener.prepare() {
                    Ok(listener) => {
                        // Create dispatcher only after the socket is owned.
                        let (dispatcher, watch_rx) = PointWatchDispatcher::new();
                        (
                            Some(bitmap),
                            Some(dispatcher),
                            Some(listener),
                            Some(event_rx),
                            Some(watch_rx),
                        )
                    },
                    Err(error) => {
                        warn!(
                            "PointWatch disabled because its local listener could not be prepared: {error}"
                        );
                        (None, None, None, None, None)
                    },
                }
            },
            None => (None, None, None, None, None),
        }
    };

    // Create the rule scheduler with SHM as the live-state authority.
    // The shared ControlApplication routes all M2C commands through the typed
    // SHM + UDS command sink configured above.
    // Stateful calculation memory is intentionally process-local for now.
    let rule_log_root = PathBuf::from("logs/automation");
    let state_store = Arc::new(MemoryStateStore::new());
    let rule_live_state = Arc::new(ShmRuleLiveState::from_topology(Arc::clone(
        &runtime_topology,
    )));
    let rule_action_application = Arc::new(RuleActionApplication::new(Arc::clone(
        &state.control_application,
    )));
    let mut scheduler = RuleScheduler::with_state_store(
        rule_live_state,
        sqlite_pool.clone(),
        tick_ms,
        rule_log_root,
        state_store,
        // Both scheduled and manually-triggered rule actions enter the same
        // mandatory audit + CommandDispatcher path as external control.
        Some(rule_action_application),
    );
    scheduler
        .set_max_concurrency(max_concurrency)
        .map_err(AutomationError::from)?;
    scheduler
        .set_execution_timeout(Duration::from_millis(rule_execution_timeout_ms))
        .map_err(AutomationError::from)?;

    // Wire PointWatch event receiver into the scheduler (before Arc::new).
    // When present, RuleScheduler::start() selects on this channel alongside
    // the 100 ms tick for sub-millisecond OnChange rule dispatch.
    if let Some(watch_rx) = pw_watch_rx {
        scheduler.set_watch_receiver(watch_rx);
        info!("PointWatch watch_rx wired into RuleScheduler");
    }

    // Wrap dispatcher in Arc<Mutex<>> so the bridge task and the scheduler's
    // reload_rules path can share it. std::sync::Mutex (not tokio::sync) since
    // dispatch() and rebuild_from_rules() never .await inside the critical
    // section — async overhead would only add cost on the hot path.
    let pw_dispatcher_arc = pw_dispatcher.map(|d| Arc::new(std::sync::Mutex::new(d)));

    // Retain a reloadable view of the exact manifest atomically published with
    // each command writer generation. PointWatch never caches a stale layout
    // across IO's canonical-file swaps.
    let pw_manifest_source = state.shm_dispatch.manifest_source();

    // The rule library owns only the transport-neutral dispatcher index. This
    // service composition publishes returned points through the SHM bitmap.
    if let (Some(disp_arc), Some(_bitmap)) = (pw_dispatcher_arc.as_ref(), pw_bitmap.as_ref()) {
        scheduler.set_point_watch_rebuild_handle(Arc::clone(disp_arc));
        info!("PointWatch rebuild handle wired into RuleScheduler");
    }

    let scheduler = Arc::new(scheduler);
    state
        .install_rule_scheduler(Arc::clone(&scheduler))
        .map_err(|_| {
            AutomationError::InternalError("rule scheduler was installed more than once".into())
        })?;
    let rule_runtime = Arc::new(match pw_bitmap {
        Some(bitmap) => RuleRuntimeCoordinator::new(Arc::clone(&scheduler)).with_point_watch(
            Arc::clone(&runtime_topology),
            bitmap,
            pw_manifest_source,
        ),
        None => RuleRuntimeCoordinator::new(Arc::clone(&scheduler)),
    });

    info!(
        "Rule scheduler: tick_ms={}, max_concurrency={}",
        tick_ms, max_concurrency
    );

    // One coordinator owns every scheduler/PointWatch publication, including
    // startup, topology changes, and governed rule mutations.
    match rule_runtime.reload().await {
        Ok(refresh) => {
            info!("Rule Engine: loaded {} rules", refresh.rule_count());
            if let Some(error) = refresh.point_watch_failure() {
                warn!("PointWatch subscriptions remain gated: {error}");
            }
        },
        Err(error) => {
            shutdown_token.cancel();
            // Startup cannot advertise readiness with an empty/stale rule set.
            // Drain the already-composed recovery listeners before returning.
            let _ = supervisor.run(shutdown_token.clone()).await;
            common::logging::shutdown_logging_tasks().await;
            return Err(AutomationError::SchedulerError(format!(
                "initial rule load failed: {error}"
            )));
        },
    }

    // No fallible bootstrap step remains beyond this point. Only now activate
    // long-running tasks, so every startup error above exits without a detached
    // Tokio task or an enabled rule loop.
    {
        let sink = Arc::clone(&state.shm_dispatch);
        let task_shutdown = shutdown_token.clone();
        supervisor.spawn("automation-command-notifier-probe", async move {
            run_command_notifier_probe(sink, task_shutdown).await;
        });
    }
    {
        let sink = Arc::clone(&state.shm_dispatch);
        let pool = sqlite_pool.clone();
        let topology = Arc::clone(&runtime_topology);
        let task_shutdown = shutdown_token.clone();
        supervisor.spawn("automation-command-topology-rebuild", async move {
            run_command_topology_rebuild(sink, pool, topology, task_shutdown).await;
        });
    }
    {
        let sink = Arc::clone(&state.shm_dispatch);
        let path = shm_path.clone();
        let task_shutdown = shutdown_token.clone();
        supervisor.spawn("automation-shm-inode-watch", async move {
            run_shm_inode_watch(sink, path, task_shutdown).await;
        });
    }
    if let Some(listener) = pw_listener {
        supervisor.spawn_result("automation-point-watch-listener", async move {
            listener.run().await.map_err(anyhow::Error::from)
        });
    }

    // Refresh the full SQLite + point/health topology periodically. Transient
    // physical failures retain the current generation; invalid persisted
    // topology revokes commands until a complete candidate can be published.
    {
        let refresh_topology = Arc::clone(&runtime_topology);
        let refresh_pool = sqlite_pool.clone();
        let refresh_token = shutdown_token.clone();
        supervisor.spawn("automation-topology-refresh", async move {
            let mut interval = tokio::time::interval(topology_refresh_interval);
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            // Startup already attempted one refresh; avoid an immediate duplicate tick.
            interval.tick().await;
            loop {
                tokio::select! {
                    _ = interval.tick() => {
                        if let Err(error) = refresh_topology.refresh(&refresh_pool).await {
                            if error.is_retryable() {
                                debug!("Automation topology refresh deferred: {error}");
                            } else {
                                warn!("Automation topology refresh rejected: {error}");
                            }
                        }
                    },
                    _ = refresh_token.cancelled() => break,
                }
            }
        });
    }

    // Every successful topology replacement rebuilds PointWatch routing and
    // bitmap subscriptions from the newly published service generation.
    if let Some(mut changes) = rule_runtime.topology_changes() {
        let subscription_runtime = Arc::clone(&rule_runtime);
        let subscription_token = shutdown_token.clone();
        supervisor.spawn_result("automation-point-watch-reload", async move {
            loop {
                tokio::select! {
                    biased;
                    _ = subscription_token.cancelled() => return Ok(()),
                    changed = changes.changed() => {
                        if changed.is_err() {
                            return Err(anyhow::anyhow!("topology change stream closed"));
                        }
                        match subscription_runtime.reload().await {
                            Ok(refresh) => {
                                if let Some(error) = refresh.point_watch_failure() {
                                    warn!(
                                        "PointWatch subscriptions remain gated for topology sequence {}: {error}",
                                        refresh.topology_sequence().unwrap_or_default()
                                    );
                                } else if let Some(sequence) = refresh.topology_sequence() {
                                    info!(
                                        "PointWatch subscriptions refreshed for topology sequence {} ({} rules)",
                                        sequence,
                                        refresh.rule_count()
                                    );
                                } else {
                                    debug!(
                                        "Rules refreshed after a topology change ({} rules)",
                                        refresh.rule_count()
                                    );
                                }
                            },
                            Err(error) => return Err(anyhow::anyhow!(
                                "rules/PointWatch reload after topology change failed: {error}"
                            )),
                        }
                    },
                }
            }
        });
    }

    // Spawn the PointWatch bridge task if PointWatch is enabled. The
    // subscription index has already been built by reload_rules above; this
    // task validates each UDS wake-up hint, re-reads its pinned SHM slot, then
    // routes the authoritative sample through dispatcher.dispatch() → watch_rx.
    if let (Some(dispatcher_arc), Some(mut event_rx)) = (pw_dispatcher_arc, pw_event_rx) {
        // Spawn the bridge task: drains raw PointWatchEvents from the listener
        // and calls dispatcher.dispatch() which sends WatchEvents onto the
        // channel that RuleScheduler reads via set_watch_receiver.
        //
        // dispatcher_arc is shared with scheduler.reload_rules — the lock is
        // held briefly (just for the try_send hash lookup, no .await inside).
        let dispatcher_for_bridge = Arc::clone(&dispatcher_arc);
        let topology_for_bridge = Arc::clone(&runtime_topology);
        let runtime_for_bridge = Arc::clone(&rule_runtime);
        let shutdown_token_bridge = shutdown_token.clone();
        supervisor.spawn_result("automation-point-watch-bridge", async move {
            loop {
                tokio::select! {
                    biased;
                    _ = shutdown_token_bridge.cancelled() => return Ok(()),
                    ev = event_rx.recv() => {
                        match ev {
                            Some(e) => {
                                let Some(point_kind) = e.point_kind() else {
                                    continue;
                                };
                                let view = Arc::clone(&topology_for_bridge).pin_command().await;
                                if !runtime_for_bridge.accepts_point_watch(view.generation(), e)
                                {
                                    continue;
                                }
                                let sample = match view.generation().read_point_watch_sample(e) {
                                    Ok(Some(sample)) => sample,
                                    Ok(None) => continue,
                                    Err(error) => {
                                        warn!(
                                            channel_id = e.channel_id(),
                                            point_id = e.point_id(),
                                            slot = e.slot_index(),
                                            "PointWatch SHM re-read rejected: {error}"
                                        );
                                        continue;
                                    },
                                };
                                // Recover from mutex poison: prior panic in another
                                // thread doesn't invalidate the dispatcher state.
                                let d = dispatcher_for_bridge
                                    .lock()
                                    .unwrap_or_else(|p| p.into_inner());
                                d.dispatch(PointWatchHint::new(
                                    e.channel_id(),
                                    point_kind,
                                    e.point_id(),
                                    sample.value(),
                                    sample.raw(),
                                    sample.timestamp_ms(),
                                ));
                            }
                            None => return Err(anyhow::anyhow!(
                                "PointWatch listener channel closed"
                            )),
                        }
                    }
                }
            }
        });
        info!("PointWatch bridge task spawned");
    }

    // Create rule engine state and routes
    let rule_audit: Arc<dyn aether_ports::AuditSink> = Arc::new(rule_audit);
    let rule_application = Arc::new(aether_application::RuleExecutionApplication::new(
        scheduler.clone(),
        Arc::clone(&rule_audit),
        aether_application::SafetyPolicy,
    ));
    let rule_mutator: Arc<dyn aether_ports::AutomationRuleMutator> = Arc::new(
        aether_automation::infra::rule_mutation::SqliteRuleMutator::new(
            sqlite_pool.clone(),
            rule_runtime,
        ),
    );
    let rule_mutation_application = Arc::new(aether_application::RuleMutationApplication::new(
        rule_mutator,
        rule_audit,
        aether_application::SafetyPolicy,
    ));
    let rule_state = Arc::new(RuleEngineState::new(
        Arc::new(RuleQueries::new(sqlite_pool, Arc::clone(&scheduler))),
        rule_application,
        rule_mutation_application,
        Arc::clone(&state.control_authenticator),
    ));
    let rule_routes = create_rule_routes(rule_state);

    // Merge rule routes into the main app (both on port 6002)
    let app = app.merge(rule_routes);
    #[cfg(feature = "openapi")]
    let app = app.route("/openapi.json", axum::routing::get(openapi_document));

    // Start HTTP service (model API + rule engine - port 6002)
    info!("Automation service started on {}", addr);
    info!("");
    info!("Model API endpoints (port {}):", state.config.api.port);
    info!("  GET /health - Health check");
    info!("  GET/POST /api/instances - Instance management");
    info!("  GET /api/products - Product management");
    info!("  GET /api/instances/:id/data - Get instance data");
    info!("  POST /api/instances/:id/action - Accept action into local command plane");
    info!("");
    info!(
        "Rule Engine API endpoints (port {}):",
        state.config.api.port
    );
    info!("  GET/POST /api/rules - Rule management");
    info!("  GET/PUT/DELETE /api/rules/:id - Single rule operations");
    info!("  POST /api/rules/:id/execute - Execute rule manually");
    info!("  GET /api/scheduler/status - Scheduler status");
    info!("  POST /api/scheduler/reload - Reload rules");

    let scheduler_task = Arc::clone(&scheduler);
    let scheduler_stop = Arc::clone(&scheduler);
    let scheduler_shutdown = shutdown_token.clone();
    supervisor.spawn("automation-rule-scheduler", async move {
        let mut scheduler_run = std::pin::pin!(scheduler_task.start());
        tokio::select! {
            _ = &mut scheduler_run => {},
            _ = scheduler_shutdown.cancelled() => {
                scheduler_stop.stop();
                // Let an in-progress rule finish its receipt/state update. The
                // outer critical-task supervisor still enforces the service's
                // five-second drain deadline.
                scheduler_run.await;
            },
        }
    });
    // The listener is already reserved and every fallible rule bootstrap step
    // has completed, so logging maintenance can now join the runtime lifecycle.
    common::logging::enable_sighup_log_reopen();
    let supervisor_shutdown = shutdown_token.clone();
    let supervisor_task = tokio::spawn(async move { supervisor.run(supervisor_shutdown).await });
    info!("Rule scheduler started");

    let server_result =
        common::shutdown::serve_prebound_with_shutdown(listener, addr, app, shutdown_token.clone())
            .await;
    shutdown_token.cancel();
    scheduler.stop();
    let supervisor_result = supervisor_task.await.map_err(|error| {
        AutomationError::InternalError(format!("task supervisor join failed: {error}"))
    })?;
    let logger_scheduler = Arc::clone(&scheduler);
    let logger_shutdown =
        tokio::task::spawn_blocking(move || logger_scheduler.shutdown_rule_logging())
            .await
            .map_err(|error| {
                AutomationError::InternalError(format!("rule logger shutdown task failed: {error}"))
            })?
            .map_err(|error| {
                AutomationError::InternalError(format!(
                    "rule logger did not drain cleanly: {error}"
                ))
            });

    server_result.map_err(|error| AutomationError::InternalError(error.to_string()))?;
    supervisor_result?;
    logger_shutdown?;

    info!("Automation service shutdown complete");
    Ok(())
}
