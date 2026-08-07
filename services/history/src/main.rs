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

use std::sync::Arc;

use tokio::sync::{Mutex, RwLock};
use tokio_util::sync::CancellationToken;
use tower_http::cors::{Any, CorsLayer};
use tracing::info;

mod api;
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

/// How long shutdown waits for the buffered points to reach storage.
const FINAL_FLUSH_TIMEOUT_SECS: u64 = 15;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // ── Env config ────────────────────────────────────────────────────────────
    let env = Arc::new(EnvConfig::default());

    // ── Logging ───────────────────────────────────────────────────────────────
    common::service_bootstrap::init_service(
        "aether-history",
        "Historical data service",
        env.api_port,
    )?;

    info!("aether-history starting");
    info!("SHM: {}", env.shm_path);
    info!("Channel health SHM: {}", env.channel_health_shm_path);
    info!("Embedded history: {}", env.history_db_path);

    // ── Shared SQLite – config table ──────────────────────────────────────────
    let sqlite = common::bootstrap_database::open_service_pool(&env.db_path).await?;

    db_config::create_config_table(&sqlite, &env.history_db_path).await?;
    let service_cfg = db_config::load_config(&sqlite).await?;
    let storage_cfg = db_config::load_storage(&sqlite).await?;
    let collector = collector::build_shm_history_collector(&sqlite, &env).await?;

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
        collector,
        storage: Arc::new(RwLock::new(initial_storage)),
        sqlite,
        env: Arc::clone(&env),
        config: Arc::new(RwLock::new(service_cfg)),
        storage_settings: Arc::new(RwLock::new(storage_cfg)),
        buffer: Arc::new(Mutex::new(Vec::new())),
    });

    // ── Background tasks ──────────────────────────────────────────────────────
    let shutdown = CancellationToken::new();
    let background = scheduler::spawn_all(Arc::clone(&state), shutdown.clone());

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

    let addr = common::bind_address(&env.api_host, env.api_port)?;

    common::shutdown::serve_with_shutdown(addr, app, shutdown).await?;

    // `serve_with_shutdown` only drains in-flight HTTP connections. Returning here
    // would drop the runtime while the flush task is still writing its final batch.
    background
        .join_flush(std::time::Duration::from_secs(FINAL_FLUSH_TIMEOUT_SECS))
        .await;

    Ok(())
}
