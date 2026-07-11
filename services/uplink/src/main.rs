//! `aether-uplink` — cloud data-forwarding service.
//!
//! Reads real-time data from SHM, persists outbound messages to a local
//! outbox, and forwards them to the cloud via MQTT (AWS IoT Core or any MQTT
//! 3.1.1 broker with optional TLS).
//!
//! Responsibilities:
//! - MQTT connection management with auto-reconnect and TLS
//! - Periodic property data upload from SHM
//! - System metrics upload (CPU, memory, disk, network)
//! - Single-point read / write commands from the cloud
//! - call-data / call-alarm total-recall commands
//! - Alarm broadcast (from alarm via HTTP → MQTT)
//! - HTTP API for MQTT config, status, certificate management
//!
//! Runtime settings (broker URL, intervals, logical series patterns) are persisted in
//! the shared SQLite `uplink_config` table.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use sqlx::sqlite::SqlitePoolOptions;
use tokio::sync::{Mutex, Notify, RwLock};
use tokio_util::sync::CancellationToken;
use tower_http::cors::{Any, CorsLayer};
use tracing::info;

mod config;
mod db_config;
mod device;
mod forwarder;
mod live_values;
mod models;
mod mqtt;
mod routes;
mod state;
mod system_monitor;
mod uplink;

use crate::config::EnvConfig;
use crate::state::AppState;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let env = Arc::new(EnvConfig::default());

    // ── Logging ───────────────────────────────────────────────────────────────
    let service_info = common::service_bootstrap::ServiceInfo::new(
        "aether-uplink",
        "Cloud data-forwarding service",
        env.api_port,
    );
    common::service_bootstrap::init_logging(&service_info, None)
        .map_err(|e| anyhow::anyhow!("Logging init failed: {}", e))?;
    common::logging::enable_sighup_log_reopen();
    common::service_bootstrap::print_startup_banner(&service_info);

    info!("aether-uplink starting");
    info!("SHM: {}", env.shm_path);

    // ── Shared SQLite ─────────────────────────────────────────────────────────
    if let Some(dir) = std::path::Path::new(&env.db_path).parent() {
        std::fs::create_dir_all(dir)?;
    }
    let sqlite = SqlitePoolOptions::new()
        .max_connections(5)
        .connect_with(common::bootstrap_database::sqlite_connect_options(
            &env.db_path,
        ))
        .await
        .map_err(|e| anyhow::anyhow!("SQLite connect failed: {} path={}", e, env.db_path))?;

    db_config::create_config_table(&sqlite).await?;
    let net_cfg = db_config::load_config(&sqlite).await?;

    // ── Device identity ───────────────────────────────────────────────────────
    let device = Arc::new(device::DeviceIdentity::resolve(
        &net_cfg.product_sn,
        &net_cfg.device_sn,
    ));
    let topics = Arc::new(device.topics());

    // ── Durable uplink outbox ─────────────────────────────────────────────────
    let file_outbox = Arc::new(
        aether_store_local::FileOutbox::open(&env.outbox_path, env.outbox_capacity)
            .map_err(|error| anyhow::anyhow!("Outbox open failed: {error}"))?,
    );
    let outbox: Arc<dyn aether_ports::DurableOutbox> = file_outbox.clone();

    // ── App State ─────────────────────────────────────────────────────────────
    let state = Arc::new(AppState {
        sqlite,
        outbox,
        env: Arc::clone(&env),
        config: Arc::new(RwLock::new(net_cfg)),
        device,
        topics,
        mqtt_client: Arc::new(Mutex::new(None)),
        mqtt_connected: Arc::new(AtomicBool::new(false)),
        reconnect_signal: Arc::new(Notify::new()),
        disconnect_requested: Arc::new(AtomicBool::new(false)),
        http_client: reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .build()?,
    });

    // ── Background tasks ──────────────────────────────────────────────────────
    let shutdown = CancellationToken::new();

    {
        let s = Arc::clone(&state);
        let sd = shutdown.clone();
        tokio::spawn(async move { mqtt::run_mqtt_loop(s, sd).await });
    }
    {
        let s = Arc::clone(&state);
        let sd = shutdown.clone();
        tokio::spawn(async move { forwarder::run_data_forwarder(s, sd).await });
    }
    {
        let s = Arc::clone(&state);
        let sd = shutdown.clone();
        tokio::spawn(async move { forwarder::run_system_monitor(s, sd).await });
    }
    {
        let s = Arc::clone(&state);
        let sd = shutdown.clone();
        tokio::spawn(async move { uplink::run_outbox_forwarder(s, sd).await });
    }
    {
        let sd = shutdown.clone();
        tokio::spawn(async move { uplink::run_outbox_maintenance(file_outbox, sd).await });
    }

    // ── HTTP server ───────────────────────────────────────────────────────────
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    let app = routes::build_router(Arc::clone(&state))
        .layer(axum::middleware::from_fn(
            common::logging::http_request_logger,
        ))
        .layer(axum::extract::DefaultBodyLimit::max(1024 * 1024))
        .layer(cors);

    let addr: std::net::SocketAddr = format!("{}:{}", env.api_host, env.api_port)
        .parse()
        .map_err(|e| anyhow::anyhow!("Invalid bind address: {}", e))?;

    info!("uplink listening on {}", addr);

    let listener = tokio::net::TcpListener::bind(addr).await?;

    axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            common::shutdown::wait_for_shutdown().await;
            info!("Shutdown signal received");
            shutdown.cancel();
        })
        .await?;

    common::logging::shutdown_logging_tasks().await;
    Ok(())
}
