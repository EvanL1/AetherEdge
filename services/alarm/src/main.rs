//! `aether-alarm` — alarm monitoring service.
//!
//! Rewrites the Python alarm with the same REST API surface:
//! - Alert rules CRUD (`/alarmApi/rules`)
//! - Active alerts (`/alarmApi/alerts`)
//! - Alert event history (`/alarmApi/alert-events`)
//! - Background monitoring loop (reads SHM, triggers/recovers alerts)
//! - HTTP broadcasts to api (6005) and uplink (6006)

use std::sync::Arc;

use tokio_util::sync::CancellationToken;
use tracing::info;

mod alarm_rule_mutation;
mod api;
mod broadcast;
mod config;
mod db;
mod live_values;
mod models;
mod monitor;
mod notification;
mod routes;
mod state;

use crate::broadcast::HttpAlarmNotifier;
use crate::config::AlarmConfig;
use crate::live_values::{AlarmValueSource, build_shm_alarm_source, run_alarm_topology_refresh};
use crate::models::MonitorStatus;
use crate::state::AppState;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cfg = AlarmConfig::default();

    // ── Logging ──────────────────────────────────────────────────────────────
    common::service_bootstrap::init_service(
        "aether-alarm",
        "Alarm monitoring service",
        cfg.api_port,
    )?;

    info!("aether-alarm starting on port {}", cfg.api_port);
    info!("SHM:   {}", cfg.shm_path);
    info!("Health SHM: {}", cfg.channel_health_shm_path);
    info!("PointWatch: {}", cfg.point_watch_socket);
    info!("DB:    {}", cfg.db_path);

    // ── SQLite ────────────────────────────────────────────────────────────────
    let db_pool = common::bootstrap_database::open_service_pool(&cfg.db_path).await?;

    db::create_tables(&db_pool).await?;

    // ── Live state (lazy SHM reader; writer may start before or after us) ──────
    let live_values = build_shm_alarm_source(&db_pool, &cfg).await?;

    // ── HTTP client (for broadcasts) ──────────────────────────────────────────
    let http_client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()?;

    let notifier: Arc<dyn notification::AlarmNotifier> = Arc::new(HttpAlarmNotifier::new(
        http_client,
        cfg.api_url.clone(),
        cfg.uplink_url.clone(),
    ));

    // ── Governed alarm command boundary ──────────────────────────────────────
    let access_authenticator = Arc::new(
        aether_auth_jwt::AccessTokenAuthenticator::from_env()
            .map_err(|error| anyhow::anyhow!("Alarm command authentication: {error}"))?,
    );
    let audit: Arc<dyn aether_ports::AuditSink> = Arc::new(
        aether_store_local::SqliteAuditSink::initialize(db_pool.clone())
            .await
            .map_err(|error| anyhow::anyhow!("Alarm command audit: {error}"))?,
    );
    let alarm_store = Arc::new(alarm_rule_mutation::SqliteAlarmRuleMutator::new(
        db_pool.clone(),
        Arc::clone(&notifier),
    ));
    let mutator: Arc<dyn aether_ports::AlarmRuleMutator> = alarm_store.clone();
    let resolver: Arc<dyn aether_ports::AlertResolver> = alarm_store;
    let rule_application = Arc::new(aether_application::AlarmRuleApplication::new(
        mutator,
        Arc::clone(&audit),
        aether_application::SafetyPolicy,
    ));
    let alert_resolution_application =
        Arc::new(aether_application::AlertResolutionApplication::new(
            resolver,
            audit,
            aether_application::SafetyPolicy,
        ));

    let monitor_status = Arc::new(tokio::sync::RwLock::new(MonitorStatus {
        running: false,
        last_check_time: None,
        check_interval: cfg.data_fetch_interval,
    }));
    let state_live_values: Arc<dyn AlarmValueSource> = live_values.clone();

    let state = Arc::new(AppState {
        db: db_pool,
        live_values: state_live_values,
        config: Arc::new(cfg.clone()),
        notifier,
        monitor_status,
        rule_application,
        alert_resolution_application,
        access_authenticator,
    });

    // ── Background tasks ──────────────────────────────────────────────────────
    let shutdown = CancellationToken::new();

    let topology_source = Arc::clone(&live_values);
    let topology_pool = state.db.clone();
    let topology_config = cfg.clone();
    let topology_shutdown = shutdown.clone();
    tokio::spawn(async move {
        run_alarm_topology_refresh(
            topology_source,
            topology_pool,
            topology_config,
            topology_shutdown,
        )
        .await;
    });

    let monitor_state = Arc::clone(&state);
    let monitor_shutdown = shutdown.clone();
    tokio::spawn(async move {
        monitor::run_monitor(monitor_state, monitor_shutdown).await;
    });

    let count_state = Arc::clone(&state);
    let count_shutdown = shutdown.clone();
    tokio::spawn(async move {
        monitor::run_alarm_count_broadcaster(count_state, count_shutdown).await;
    });

    // ── HTTP server ───────────────────────────────────────────────────────────
    let app = routes::create_routes(Arc::clone(&state))
        .layer(axum::middleware::from_fn(
            common::logging::http_request_logger,
        ))
        .layer(axum::extract::DefaultBodyLimit::max(1024 * 1024));

    let addr = common::bind_address(&cfg.api_host, cfg.api_port)?;

    common::shutdown::serve_with_shutdown(addr, app, shutdown).await?;

    info!("alarm stopped");
    Ok(())
}
