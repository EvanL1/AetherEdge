//! `aether-automation` — instance, rule, and action orchestration service.
//!
//! Model management service supporting measurement/action separation architecture.
//! Rule Engine API is integrated on the same port (6002).

use std::{net::SocketAddr, path::PathBuf, sync::Arc, time::Duration};

use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

// aether-automation imports
use aether_automation::infra::{
    application_control::RuleActionApplication, rule_live_state::ShmRuleLiveState,
    rule_runtime::RuleRuntimeCoordinator,
};
use aether_automation::{
    AutomationError, DEFAULT_TICK_MS, Result, RuleScheduler,
    api::rule_routes::{RuleEngineState, create_rule_routes},
    bootstrap, routes,
};
use aether_calc::MemoryStateStore;
use aether_rules::{PointWatchDispatcher, PointWatchHint, WatchEvent};
use aether_shm_bridge::{
    PointWatchEvent, PointWatchEventListener, SubscriptionBitmap, automation_bitmap_path_from_shm,
    default_shm_path, point_watch_socket_from_shm,
};

#[cfg(feature = "openapi")]
async fn openapi_document() -> axum::Json<utoipa::openapi::OpenApi> {
    axum::Json(routes::openapi_document())
}

#[tokio::main]
async fn main() -> Result<()> {
    // Create service info
    let service_info = bootstrap::create_service_info();

    // Initialize cancellation token for graceful shutdown
    let shutdown_token = CancellationToken::new();
    debug!("Shutdown token initialized");

    // Create application state with all initialized components
    let state = bootstrap::create_app_state(&service_info).await?;

    // Create API routes using the routes module. The gateway is the only
    // process that serves Swagger UI; this loopback service publishes its spec.
    let app = routes::create_routes(Arc::clone(&state));

    // ============================================================================
    // Initialize Rule Engine (integrated on port 6002)
    // ============================================================================
    let sqlite_pool = state.instance_manager.pool().clone();

    // Load tick_ms from global config (SQLite key-value table)
    let tick_ms: u64 = sqlx::query_scalar::<_, String>(
        "SELECT value FROM service_config WHERE service_name = 'global' AND key = 'rules.tick_ms'",
    )
    .fetch_optional(&sqlite_pool)
    .await
    .ok()
    .flatten()
    .and_then(|s| s.parse().ok())
    .unwrap_or(DEFAULT_TICK_MS);

    debug!("Rule scheduler tick_ms: {}", tick_ms);

    let shm_path = default_shm_path();
    debug!("SHM path: {}", shm_path.display());

    let runtime_topology = Arc::clone(state.instance_manager.runtime_topology());
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

    // UDS notification is self-healing. A command receipt is returned only
    // after the complete command event has been written to this transport;
    // the receipt is not a physical-device acknowledgement.
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
    info!("Typed command notifier configured for {m2c_socket}; reconnect is automatic");

    // A stale command writer requests a bounded refresh of the complete
    // point/health/routing generation. Partial IO publication keeps the prior
    // service generation and is retried.
    {
        let rebuild_notify = state.shm_dispatch.rebuild_trigger();
        let rebuild_pool = sqlite_pool.clone();
        let rebuild_topology = Arc::clone(&runtime_topology);
        let rebuild_token = shutdown_token.clone();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = rebuild_notify.notified() => {},
                    _ = rebuild_token.cancelled() => break,
                }
                info!("SHM rebuild triggered — refreshing the complete automation topology...");
                const MAX_RETRIES: u32 = 10;
                const BASE_DELAY_MS: u64 = 1000;
                const MAX_DELAY_MS: u64 = 15000;
                let mut retry_count = 0u32;
                let ok = loop {
                    match rebuild_topology.refresh(&rebuild_pool).await {
                        Ok(_) => {
                            info!("Complete automation topology restored successfully");
                            break true;
                        },
                        Err(e) if retry_count < MAX_RETRIES => {
                            let delay = (BASE_DELAY_MS * 2u64.pow(retry_count)).min(MAX_DELAY_MS);
                            info!(
                                "Automation topology refresh retry {}/{} in {}ms: {}",
                                retry_count + 1,
                                MAX_RETRIES,
                                delay,
                                e
                            );
                            tokio::time::sleep(Duration::from_millis(delay)).await;
                            retry_count += 1;
                        },
                        Err(e) => {
                            warn!(
                                "Automation topology refresh failed after {} retries: {}. \
                                 A later unavailable command will start a new bounded cycle.",
                                MAX_RETRIES, e
                            );
                            break false;
                        },
                    }
                };
                if ok {
                    info!("SHM auto-rebuild complete — M2C dispatch restored");
                }
            }
        });
    }

    // Spawn SHM canonical-path inode watcher.
    //
    // Step 3 of the SHM decoupling roadmap replaces in-place
    // reconfigure_existing with `ShmWriterHandle::rebuild_via_swap`: io
    // creates a new SHM file at a staging path, then POSIX-renames it
    // over the canonical path. automation's existing command mmap is still
    // mmap'd to the *previous* inode (now unlinked but live in memory),
    // so its `writer.generation()` reads stay constant. The command sink closes
    // that blind spot synchronously: every command holds the stable authority
    // sidecar's shared lease through SHM + UDS + receipt formation and checks
    // the mapped `(device, inode)` against the canonical path before and after
    // the transaction. IO holds the exclusive lease throughout publication.
    //
    // This low-frequency watcher is therefore a recovery accelerator, not a
    // correctness boundary. It periodically `stat(canonical_path)` and
    // compare the inode against a cached baseline. On change, fire the
    // existing `rebuild_trigger` Notify, which the auto-rebuild task
    // above already handles end-to-end (validated open on the new inode and
    // coherent writer/manifest publication).
    {
        use std::os::unix::fs::MetadataExt;
        const WATCH_INTERVAL: Duration = Duration::from_secs(5);

        let watch_dispatch = Arc::clone(&state.shm_dispatch);
        let watch_path = shm_path.clone();
        let watch_token = shutdown_token.clone();

        tokio::spawn(async move {
            // Baseline: capture initial inode (None if canonical does not
            // yet exist — io may not have created it yet). We only
            // fire on a *change* from a known-good value to avoid a
            // spurious rebuild during cold boot.
            let mut last_inode = std::fs::metadata(&watch_path).ok().map(|m| m.ino());
            if let Some(ino) = last_inode {
                info!(
                    "SHM inode watcher: baseline inode={} for {:?}",
                    ino, watch_path
                );
            } else {
                info!(
                    "SHM inode watcher: canonical path {:?} not yet present; will start tracking once it appears",
                    watch_path
                );
            }

            loop {
                tokio::select! {
                    _ = tokio::time::sleep(WATCH_INTERVAL) => {},
                    _ = watch_token.cancelled() => break,
                }

                let current_inode = match std::fs::metadata(&watch_path) {
                    Ok(m) => Some(m.ino()),
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
                    Err(e) => {
                        warn!(
                            "SHM inode watcher: stat {:?} failed (non-NotFound): {}",
                            watch_path, e
                        );
                        continue;
                    },
                };

                match (last_inode, current_inode) {
                    (Some(old), Some(new)) if old != new => {
                        info!(
                            "SHM inode watcher: canonical path inode changed {} → {} \
                             (io likely performed an atomic-swap reload); \
                             triggering writer rebuild",
                            old, new
                        );
                        last_inode = Some(new);
                        // The prior mmap still reports its old stable header
                        // generation after rename, so invalidate it explicitly
                        // before asking the rebuild loop to reopen the path.
                        watch_dispatch.invalidate_and_rebuild();
                    },
                    (None, Some(new)) => {
                        info!(
                            "SHM inode watcher: canonical path now exists (inode={}); \
                             tracking baseline",
                            new
                        );
                        last_inode = Some(new);
                    },
                    (Some(_), None) => {
                        warn!(
                            "SHM inode watcher: canonical path {:?} disappeared — \
                             io may be mid-restart; keeping prior baseline",
                            watch_path
                        );
                    },
                    _ => {}, // no change
                }
            }
        });
    }

    // Load max_concurrency from global config (SQLite key-value table)
    let max_concurrency: usize = sqlx::query_scalar::<_, String>(
        "SELECT value FROM service_config WHERE service_name = 'global' AND key = 'rules.max_concurrency'",
    )
    .fetch_optional(&sqlite_pool)
    .await
    .ok()
    .flatten()
    .and_then(|s| s.parse().ok())
    .unwrap_or(4);

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
        Option<tokio::sync::mpsc::Receiver<PointWatchEvent>>,
        Option<tokio::sync::mpsc::Receiver<WatchEvent>>,
    );
    let (pw_bitmap, pw_dispatcher, pw_event_rx, pw_watch_rx): PwInitResult = {
        let bitmap_path = automation_bitmap_path_from_shm(&shm_path);
        match SubscriptionBitmap::open(&bitmap_path) {
            Ok(bitmap) => {
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

                // Spawn the UDS listener run loop (accepts io connection).
                tokio::spawn(async move {
                    if let Err(error) = listener.run().await {
                        warn!("PointWatchListener exited with error: {error}");
                    }
                });
                // Create dispatcher (empty sub index until rebuild_point_watch).
                // watch_rx flows to RuleScheduler; dispatcher.dispatch() sends onto it.
                let (dispatcher, watch_rx) = PointWatchDispatcher::new();

                (
                    Some(bitmap),
                    Some(dispatcher),
                    Some(event_rx),
                    Some(watch_rx),
                )
            },
            Err(e) => {
                warn!(
                    "PointWatch disabled (bitmap open failed — is io running?): {}",
                    e
                );
                (None, None, None, None)
            },
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
    scheduler.set_max_concurrency(max_concurrency);

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
        Err(e) => warn!("Rule Engine: failed to load rules: {}", e),
    }

    // Refresh the full SQLite + point/health topology periodically. Transient
    // physical failures retain the current generation; invalid persisted
    // topology revokes commands until a complete candidate can be published.
    {
        let refresh_topology = Arc::clone(&runtime_topology);
        let refresh_pool = sqlite_pool.clone();
        let refresh_token = shutdown_token.clone();
        tokio::spawn(async move {
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
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    changed = changes.changed() => {
                        if changed.is_err() {
                            break;
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
                            Err(error) => warn!(
                                "PointWatch subscription refresh failed; scheduler tick fallback remains active: {error}"
                            ),
                        }
                    },
                    _ = subscription_token.cancelled() => break,
                }
            }
        });
    }

    // Spawn the PointWatch bridge task if PointWatch is enabled. The
    // subscription index has already been built by reload_rules above; this
    // task just routes raw UDS events → dispatcher.dispatch() → watch_rx.
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
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    ev = event_rx.recv() => {
                        match ev {
                            Some(e) => {
                                let view = Arc::clone(&topology_for_bridge).pin_command().await;
                                if !runtime_for_bridge.accepts_point_watch(view.generation(), e)
                                {
                                    continue;
                                }
                                // Recover from mutex poison: prior panic in another
                                // thread doesn't invalidate the dispatcher state.
                                let d = dispatcher_for_bridge
                                    .lock()
                                    .unwrap_or_else(|p| p.into_inner());
                                d.dispatch(PointWatchHint::new(
                                    e.channel_id(),
                                    e.point_id(),
                                    e.value(),
                                    e.raw(),
                                    e.timestamp_ms(),
                                ));
                            }
                            None => break, // listener channel closed
                        }
                    }
                    _ = shutdown_token_bridge.cancelled() => break,
                }
            }
            debug!("PointWatch bridge task stopped");
        });
        info!("PointWatch bridge task spawned");
    }

    // Create rule engine state and routes
    let rule_audit = aether_store_local::SqliteAuditSink::initialize(sqlite_pool.clone())
        .await
        .map_err(|error| AutomationError::DatabaseError(error.to_string()))?;
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
    let rule_state = Arc::new(
        RuleEngineState::new(sqlite_pool, Arc::clone(&scheduler))
            .with_execution_boundary(rule_application, Arc::clone(&state.control_authenticator))
            .with_mutation_boundary(
                rule_mutation_application,
                Arc::clone(&state.control_authenticator),
            ),
    );
    let rule_routes = create_rule_routes(rule_state);

    // Merge rule routes into the main app (both on port 6002)
    let app = app.merge(rule_routes);
    #[cfg(feature = "openapi")]
    let app = app.route("/openapi.json", axum::routing::get(openapi_document));

    // Start HTTP service (model API + rule engine - port 6002)
    let addr: SocketAddr = format!("{}:{}", state.config.api.host, state.config.api.port)
        .parse()
        .map_err(|error| {
            AutomationError::InvalidConfig(format!(
                "invalid internal API bind address {}:{}: {error}",
                state.config.api.host, state.config.api.port
            ))
        })?;

    // Create socket for unified API (port 6002)
    let socket = tokio::net::TcpSocket::new_v4()?;
    socket.set_reuseaddr(true)?;
    socket.bind(addr)?;
    let listener = socket.listen(1024)?;

    info!("Model Service (with Rule Engine) started on {}", addr);
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

    // Prepare graceful shutdown
    let cancel_token = shutdown_token.clone();
    let shutdown_signal = async move {
        cancel_token.cancelled().await;
        info!("Shutdown signal received, stopping service...");
    };

    // Spawn server task
    let server_task = async move {
        if let Err(e) = axum::serve(listener, app)
            .with_graceful_shutdown(shutdown_signal)
            .await
        {
            error!("Server error: {}", e);
        }
    };

    // Spawn server task
    let server_handle = tokio::spawn(server_task);
    info!("Server started (port {})", state.config.api.port);

    // Start rule scheduler in background
    let scheduler_handle = {
        let scheduler = Arc::clone(&scheduler);
        tokio::spawn(async move {
            scheduler.start().await;
        })
    };
    info!("Rule scheduler started");

    // Wait for shutdown signal (Ctrl+C or SIGTERM)
    common::shutdown::wait_for_shutdown().await;
    info!("Initiating graceful shutdown...");

    // Signal all tasks to shutdown
    shutdown_token.cancel();

    // Stop scheduler
    scheduler.stop();

    // Wait for tasks to complete with timeout
    let shutdown_timeout = tokio::time::Duration::from_secs(30);

    // Wait for server task
    match tokio::time::timeout(shutdown_timeout, server_handle).await {
        Ok(Ok(())) => info!("Server shut down gracefully"),
        Ok(Err(e)) => error!("Server task failed: {}", e),
        Err(_) => {
            error!("Server shutdown timed out");
        },
    }

    // Wait for scheduler to stop
    match tokio::time::timeout(shutdown_timeout, scheduler_handle).await {
        Ok(Ok(())) => info!("Scheduler shut down gracefully"),
        Ok(Err(e)) => error!("Scheduler task failed: {}", e),
        Err(_) => {
            error!("Scheduler shutdown timed out");
        },
    }

    info!("Model Service (with Rule Engine) shutdown complete");
    Ok(())
}
