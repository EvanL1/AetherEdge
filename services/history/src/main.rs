//! `aether-history` — historical data service.
//!
//! Samples real-time data from SHM, persists it to embedded SQLite by default,
//! and exposes a REST API for historical queries. PostgreSQL/TimescaleDB are
//! optional storage adapters.
//!
//! Storage backend is configured at **runtime** via `PUT /hisApi/storage`.
//! Fresh installations start with the local SQLite backend enabled. Existing
//! runtime settings are restored on restart and may still explicitly disable
//! storage. The default profile requires only embedded SQLite and SHM.

use std::net::SocketAddr;
use std::sync::Arc;

use sqlx::sqlite::SqlitePoolOptions;
use tokio::sync::{Mutex, RwLock};
use tokio_util::sync::CancellationToken;
use tower_http::cors::{Any, CorsLayer};
use tracing::info;

mod backend_influx;
mod backend_null;
#[cfg(feature = "postgres-storage")]
mod backend_pg;
mod backend_sqlite;
#[cfg(feature = "postgres-storage")]
mod backend_tsdb;
mod collector;
mod config;
mod db_config;
mod models;
mod routes;
mod scheduler;
mod state;
mod storage;

use crate::backend_null::NullBackend;
use crate::config::EnvConfig;
use crate::state::AppState;
use crate::storage::StorageBackend;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // ── Env config ────────────────────────────────────────────────────────────
    let env = Arc::new(EnvConfig::default());

    // ── Logging ───────────────────────────────────────────────────────────────
    let service_info = common::service_bootstrap::ServiceInfo::new(
        "aether-history",
        "Historical data service",
        env.api_port,
    );
    common::service_bootstrap::init_logging(&service_info, None)
        .map_err(|e| anyhow::anyhow!("Failed to init logging: {}", e))?;
    common::logging::enable_sighup_log_reopen();
    common::service_bootstrap::print_startup_banner(&service_info);

    info!("aether-history starting");
    info!("SHM: {}", env.shm_path);
    info!("Embedded history: {}", env.history_db_path);

    // ── Shared SQLite – config table ──────────────────────────────────────────
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

    db_config::create_config_table(&sqlite, &env.history_db_path).await?;
    let service_cfg = db_config::load_config(&sqlite).await?;
    let storage_cfg = db_config::load_storage(&sqlite).await?;

    // ── Storage backend – lazy / runtime-configurable ─────────────────────────
    // Start with the null backend.  If the saved config has storage enabled,
    // attempt to reconnect immediately so a service restart preserves the setting.
    let initial_storage: Arc<dyn StorageBackend> = if storage_cfg.enabled
        && !storage_cfg.url.is_empty()
    {
        match routes::connect_storage_backend(&storage_cfg.backend, &storage_cfg.url).await {
            Ok(b) => {
                info!(
                    "Storage backend '{}' connected at startup",
                    storage_cfg.backend
                );
                b
            },
            Err(e) => {
                if storage_cfg.backend.eq_ignore_ascii_case("sqlite") {
                    return Err(anyhow::anyhow!(
                        "embedded SQLite history backend failed to initialize at {}: {}",
                        storage_cfg.url,
                        e
                    ));
                }
                tracing::warn!(
                    "Optional storage backend '{}' failed to connect at startup; keeping its configured intent visible while running degraded: {}",
                    storage_cfg.backend,
                    e
                );
                Arc::new(NullBackend)
            },
        }
    } else {
        info!("Storage disabled – configure via PUT /hisApi/storage");
        Arc::new(NullBackend)
    };

    // ── App State ─────────────────────────────────────────────────────────────
    let state = Arc::new(AppState {
        storage: Arc::new(RwLock::new(initial_storage)),
        sqlite,
        env: Arc::clone(&env),
        config: Arc::new(RwLock::new(service_cfg)),
        storage_settings: Arc::new(RwLock::new(storage_cfg)),
        buffer: Arc::new(Mutex::new(Vec::new())),
    });

    // ── Background tasks ──────────────────────────────────────────────────────
    let shutdown = CancellationToken::new();
    scheduler::spawn_all(Arc::clone(&state), shutdown.clone());

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

    let addr: SocketAddr = format!("{}:{}", env.api_host, env.api_port)
        .parse()
        .map_err(|e| anyhow::anyhow!("Invalid bind address: {}", e))?;

    info!("history listening on {}", addr);

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
