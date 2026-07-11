use std::sync::Arc;

use axum::{
    Router,
    extract::{Query, State},
    http::StatusCode,
    response::Json,
    routing::{get, post},
};
use serde_json::{Value, json};
use tracing::{error, info};
use utoipa::OpenApi;
#[cfg(feature = "swagger-ui")]
use utoipa_swagger_ui::{Config, SwaggerUi};

use crate::backend_null::NullBackend;
use crate::backend_sqlite::SqliteHistoryBackend;
use crate::db_config;
use crate::models::{
    BatchQueryRequest, BatchQueryResponse, DataStats, HistoryRecord, LatestParams,
    QueryRangeParams, QueryResult, SeriesResult, ServiceConfig, StorageConfigRequest,
    StorageSettings, StorageTestRequest,
};
use crate::state::AppState;
use crate::storage::StorageBackend;

// ============================================================================
// Internal: lightweight connectivity probe (no schema init, no data write)
// ============================================================================

/// Probe connectivity for the given backend type without writing any data.
///
/// Add a new branch here when a new `StorageBackend` is implemented.
async fn probe_backend(req: &StorageTestRequest) -> anyhow::Result<()> {
    match req.backend.as_str() {
        "sqlite" => Ok(()),
        #[cfg(feature = "postgres-storage")]
        "postgres" | "timescaledb" => probe_pg(&req.pg_probe_dsn()).await,
        #[cfg(not(feature = "postgres-storage"))]
        "postgres" | "timescaledb" => anyhow::bail!(
            "PostgreSQL storage is optional; rebuild history with --features postgres-storage"
        ),
        "influxdb" => anyhow::bail!(
            "InfluxDB backend is not yet implemented; connectivity test not supported"
        ),
        other => anyhow::bail!(
            "Unknown backend type '{}'. Valid options: sqlite | postgres | timescaledb | influxdb",
            other
        ),
    }
}

/// Open a PostgreSQL connection pool, run `SELECT 1`, then close it.
#[cfg(feature = "postgres-storage")]
async fn probe_pg(url: &str) -> anyhow::Result<()> {
    use sqlx::Executor;
    use sqlx::postgres::PgPoolOptions;

    let pool = PgPoolOptions::new()
        .max_connections(1)
        .acquire_timeout(std::time::Duration::from_secs(5))
        .connect(url)
        .await
        .map_err(|e| anyhow::anyhow!("Connection failed: {}", e))?;

    pool.execute("SELECT 1")
        .await
        .map_err(|e| anyhow::anyhow!("Probe query failed: {}", e))?;
    pool.close().await;
    Ok(())
}

// ============================================================================
// Router
// ============================================================================

pub fn build_router(state: Arc<AppState>) -> Router {
    let api = Router::new()
        // Health
        .route("/", get(root))
        .route("/ping", get(ping))
        .route("/hisApi/health", get(health))
        // Data queries
        .route("/hisApi/data/query", get(query_range))
        .route("/hisApi/data/latest", get(query_latest))
        .route("/hisApi/data/range", get(data_range))
        .route("/hisApi/data/batch-query", post(batch_query))
        // Metadata
        .route("/hisApi/channels", get(list_channels))
        .route("/hisApi/metrics", get(metrics))
        // General service config (intervals, patterns, etc.)
        .route("/hisApi/config", get(get_config).put(update_config))
        // Storage backend config & control
        .route("/hisApi/storage", get(get_storage).put(update_storage))
        .route("/hisApi/storage/test", axum::routing::post(test_storage))
        .route("/hisApi/storage/reconnect", axum::routing::post(reconnect_storage))
        // Admin API (shared endpoints from common lib)
        .route("/api/admin/logs/level", get(common::admin_api::get_log_level).post(common::admin_api::set_log_level))
        .route("/api/admin/logs/files", get(common::admin_api::list_log_files))
        .route("/api/admin/logs/view", get(common::admin_api::view_log_file))
        .with_state(state);

    #[cfg(feature = "swagger-ui")]
    let api = api.merge(
        SwaggerUi::new("/docs")
            .url("/openapi.json", ApiDoc::openapi())
            .config(
                Config::default()
                    .default_model_rendering("model")
                    .default_models_expand_depth(1),
            ),
    );

    api
}

// ============================================================================
// OpenAPI document (only consumed when swagger-ui feature is enabled)
// ============================================================================

#[cfg_attr(not(feature = "swagger-ui"), allow(dead_code))]
#[derive(OpenApi)]
#[openapi(
    paths(
        root,
        ping,
        health,
        query_range,
        query_latest,
        data_range,
        batch_query,
        list_channels,
        metrics,
        get_config,
        update_config,
        get_storage,
        update_storage,
        test_storage,
        reconnect_storage,
    ),
    components(schemas(
        HistoryRecord,
        QueryResult,
        DataStats,
        ServiceConfig,
        StorageConfigRequest,
        StorageTestRequest,
        BatchQueryRequest,
        BatchQueryResponse,
        SeriesResult,
    )),
    tags(
        (name = "Data",    description = "Historical data queries"),
        (name = "Meta",    description = "Metadata and runtime metrics"),
        (name = "Config",  description = "Service configuration"),
        (name = "Storage", description = "Storage backend configuration and control"),
        (name = "Health",  description = "Health checks"),
    ),
    info(
        title = "AetherEMS History Service",
        version = "1.0.0",
        description = "SHM historical sampling with embedded SQLite and optional PostgreSQL / TimescaleDB backends"
    )
)]
pub struct ApiDoc;

// ============================================================================
// Public helper – build and initialise a storage backend by type + URL.
// Used both in main.rs (startup restore) and the PUT /hisApi/storage handler.
// ============================================================================

pub async fn connect_storage_backend(
    backend: &str,
    url: &str,
) -> anyhow::Result<Arc<dyn StorageBackend>> {
    match backend {
        "sqlite" => {
            let storage = Arc::new(SqliteHistoryBackend::connect(url).await?);
            storage.init_schema().await?;
            Ok(storage)
        },
        #[cfg(feature = "postgres-storage")]
        "postgres" | "timescaledb" => connect_postgres_backend(backend, url).await,
        #[cfg(not(feature = "postgres-storage"))]
        "postgres" | "timescaledb" => anyhow::bail!(
            "PostgreSQL storage is optional; rebuild history with --features postgres-storage"
        ),
        "influxdb" => {
            let storage = Arc::new(crate::backend_influx::InfluxDbBackend);
            storage.init_schema().await?;
            Ok(storage)
        },
        other => anyhow::bail!("Unknown storage backend '{other}'"),
    }
}

#[cfg(feature = "postgres-storage")]
async fn connect_postgres_backend(
    backend: &str,
    url: &str,
) -> anyhow::Result<Arc<dyn StorageBackend>> {
    use sqlx::postgres::PgPoolOptions;

    // Extract the target database name from the DSN.
    let target_db = url::Url::parse(url)
        .ok()
        .map(|u| u.path().trim_start_matches('/').to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "history".to_string());

    // Step 1: Connect to the `postgres` maintenance database and auto-create
    // the target database if it does not already exist.
    let maintenance_url = replace_db_in_dsn(url, "postgres");
    let maint_pool = PgPoolOptions::new()
        .max_connections(2)
        .acquire_timeout(std::time::Duration::from_secs(10))
        .connect(&maintenance_url)
        .await
        .map_err(|e| anyhow::anyhow!("Cannot connect to database server: {}", e))?;

    let exists: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_database WHERE datname = $1)")
            .bind(&target_db)
            .fetch_one(&maint_pool)
            .await
            .unwrap_or(false);

    if !exists {
        // Database names cannot be parameterized in DDL; safe here because
        // the name comes from our own saved config, not raw user input.
        let create_sql = format!(r#"CREATE DATABASE "{}""#, target_db.replace('"', ""));
        sqlx::query(&create_sql)
            .execute(&maint_pool)
            .await
            .map_err(|e| {
                anyhow::anyhow!("Failed to auto-create database '{}': {}", target_db, e)
            })?;
        info!("Database '{}' created automatically", target_db);
    }
    maint_pool.close().await;

    // Step 2: Connect to the target database and initialise the schema.
    let pg_pool = PgPoolOptions::new()
        .max_connections(10)
        .connect(url)
        .await
        .map_err(|e| anyhow::anyhow!("Failed to connect to database '{}': {}", target_db, e))?;

    let storage: Arc<dyn StorageBackend> = match backend {
        "timescaledb" => {
            let b = Arc::new(crate::backend_tsdb::TimescaleDbBackend::new(pg_pool));
            b.init_schema().await?;
            b
        },
        _ => {
            let b = Arc::new(crate::backend_pg::PostgresBackend::new(pg_pool));
            b.init_schema().await?;
            b
        },
    };

    Ok(storage)
}

// ============================================================================
// Root / ping
// ============================================================================

/// history service banner.
///
/// Returns service name, version, and status. Use this to confirm the history
/// process is alive and the expected version is deployed.
/// Does not depend on any storage backend — returns 200 even if
/// TimescaleDB / InfluxDB is down.
#[utoipa::path(get, path = "/", tag = "Health",
    responses((status = 200, description = "Service name, version, and status")))]
async fn root() -> Json<Value> {
    Json(json!({
        "service": "aether-history",
        "version": env!("CARGO_PKG_VERSION"),
        "status": "running"
    }))
}

/// Minimal liveness probe — returns the string "pong".
///
/// Unlike `/`, the response body is a plain string with no JSON overhead,
/// making it suitable for high-frequency liveness probes and load balancer
/// health checks.
#[utoipa::path(get, path = "/ping", tag = "Health",
    responses((status = 200, description = "pong")))]
async fn ping() -> &'static str {
    "pong"
}

// ============================================================================
// Health
// ============================================================================

/// Storage backend connectivity health check.
///
/// Probes the active `StorageBackend` (Null / Postgres / Timescale / Influx)
/// with a real ping or query, not a cached status flag.
/// If the backend is unreachable, history still returns HTTP 200 but the
/// response `data` object will contain `connected: false` plus the error
/// reason. Use this to distinguish "history process is dead" from
/// "history is alive but the backend is unreachable".
#[utoipa::path(get, path = "/hisApi/health", tag = "Health",
    responses((status = 200, description = "Storage backend health status")))]
async fn health(State(state): State<Arc<AppState>>) -> Json<Value> {
    let backend = state.storage.read().await.clone();
    let storage_ok = backend.health_check().await;
    let buf_len = state.buffer.lock().await.len();
    let storage_settings = state.storage_settings.read().await;

    Json(json!({
        "success": storage_ok,
        "message": if storage_ok { "Storage backend healthy" } else { "Storage backend unavailable or not connected" },
        "data": {
            "backend":            storage_settings.backend,
            "active_backend":     backend.name(),
            "storage_enabled":    storage_settings.enabled,
            "storage_healthy":    storage_ok,
            "buffer_size":        buf_len,
        }
    }))
}

// ============================================================================
// Data queries
// ============================================================================

/// Query historical data within a time range.
///
/// Primary query endpoint. Identifies a point by
/// (`series_key`, `point_id`) logical identity and
/// slices the time window using `start_time` / `end_time`. An optional `step`
/// parameter enables downsampling aggregation. Returns a paginated list of
/// `[(timestamp, value), ...]` records. Downsampling is handled natively by
/// the backend (TimescaleDB continuous aggregate / InfluxDB group-by); history
/// itself does not resample. **Returns an empty set when no storage backend
/// is configured** — this is not an error condition.
#[utoipa::path(get, path = "/hisApi/data/query", tag = "Data",
    params(QueryRangeParams),
    responses(
        (status = 200, description = "Paginated historical records", body = QueryResult),
        (status = 500, description = "Query failed"),
    ))]
async fn query_range(
    State(state): State<Arc<AppState>>,
    Query(params): Query<QueryRangeParams>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let (default_page, max_page, max_days) = {
        let cfg = state.config.read().await;
        (
            cfg.default_page_size,
            cfg.max_page_size,
            cfg.max_time_range_days,
        )
    };

    let page = params.page.unwrap_or(1).max(1);
    let page_size = params
        .page_size
        .unwrap_or(default_page)
        .min(max_page)
        .max(1);

    let backend = state.storage.read().await.clone();
    match backend
        .query_range(&params, default_page, max_page, max_days)
        .await
    {
        Ok((data, total)) => {
            let has_more = (page * page_size) < total;
            Ok(Json(json!({
                "success": true,
                "message": format!("Found {} record(s)", data.len()),
                "data": data,
                "total": total,
                "page": page,
                "page_size": page_size,
                "has_more": has_more,
            })))
        },
        Err(e) => {
            error!("query_range error: {}", e);
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"success": false, "message": e.to_string()})),
            ))
        },
    }
}

/// Fetch the latest stored value for a given point.
///
/// Designed for "show the most recent value on page load" use-cases,
/// avoiding a full time-range query. Returns the latest `(timestamp, value)`
/// pair that history has persisted for the specified (`series_key`, `point_id`).
/// Note: "latest" means the most recent value in the historical store (subject
/// to the configured flush interval) — not the real-time SHM value.
/// For the live reading, use automation / api instead.
#[utoipa::path(get, path = "/hisApi/data/latest", tag = "Data",
    params(LatestParams),
    responses(
        (status = 200, description = "Most recent historical record for the point", body = HistoryRecord),
        (status = 404, description = "No data available yet"),
        (status = 500, description = "Query failed"),
    ))]
async fn query_latest(
    State(state): State<Arc<AppState>>,
    Query(params): Query<LatestParams>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let backend = state.storage.read().await.clone();
    match backend
        .query_latest(&params.series_key, &params.point_id)
        .await
    {
        Ok(Some(record)) => Ok(Json(json!({
            "success": true,
            "message": "Query successful",
            "data": record,
        }))),
        Ok(None) => Err((
            StatusCode::NOT_FOUND,
            Json(json!({"success": false, "message": "No data available"})),
        )),
        Err(e) => {
            error!("query_latest error: {}", e);
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"success": false, "message": e.to_string()})),
            ))
        },
    }
}

/// Batch range query – fetch multiple (`series_key`, `point_id`) series in one request.
///
/// Returns one result entry per requested series (in the same order), even if
/// a series has no data in the given range.  Each series contains at most
/// `limit_per_series` data points ordered by time ascending.
///
/// Limits:
/// - Maximum 20 series per request
/// - `limit_per_series` default 1000, max 5000
#[utoipa::path(
    post,
    path = "/hisApi/data/batch-query",
    tag = "Data",
    request_body = BatchQueryRequest,
    responses(
        (status = 200, description = "批量历史数据", body = BatchQueryResponse),
        (status = 400, description = "请求参数错误"),
        (status = 500, description = "查询失败"),
    )
)]
async fn batch_query(
    State(state): State<Arc<AppState>>,
    Json(req): Json<BatchQueryRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    const MAX_SERIES: usize = 20;
    const DEFAULT_LIMIT: i64 = 1000;
    const MAX_LIMIT: i64 = 5000;

    if req.series.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!({"success": false, "message": "series 列表不能为空"})),
        ));
    }
    if req.series.len() > MAX_SERIES {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!({
                "success": false,
                "message": format!("series 数量不能超过 {} 条，当前 {} 条", MAX_SERIES, req.series.len())
            })),
        ));
    }

    let start_time = crate::models::parse_time(&req.start_time).map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            Json(json!({"success": false, "message": format!("start_time 格式错误: {}", e)})),
        )
    })?;
    let end_time = crate::models::parse_time(&req.end_time).map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            Json(json!({"success": false, "message": format!("end_time 格式错误: {}", e)})),
        )
    })?;
    if end_time <= start_time {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!({"success": false, "message": "end_time 必须晚于 start_time"})),
        ));
    }

    let limit = req
        .limit_per_series
        .unwrap_or(DEFAULT_LIMIT)
        .clamp(1, MAX_LIMIT);

    let pairs: Vec<(String, String)> = req
        .series
        .iter()
        .map(|s| (s.series_key.clone(), s.point_id.clone()))
        .collect();

    let backend = state.storage.read().await.clone();
    match backend
        .query_batch(&pairs, start_time, end_time, limit)
        .await
    {
        Ok(series) => {
            let resp = BatchQueryResponse {
                start_time: req.start_time,
                end_time: req.end_time,
                series,
            };
            Ok(Json(json!({
                "success": true,
                "message": "OK",
                "data": resp,
            })))
        },
        Err(e) => {
            error!("batch_query error: {}", e);
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"success": false, "message": e.to_string()})),
            ))
        },
    }
}

#[utoipa::path(get, path = "/hisApi/data/range", tag = "Data",
    responses(
        (status = 200, description = "Data time range and aggregate statistics", body = DataStats),
        (status = 500, description = "Query failed"),
    ))]
async fn data_range(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let backend = state.storage.read().await.clone();
    match backend.get_stats().await {
        Ok(stats) => Ok(Json(json!({
            "success": true,
            "message": "OK",
            "data": {
                "earliest_timestamp": stats.earliest_timestamp,
                "latest_timestamp":   stats.latest_timestamp,
                "total_points":       stats.total_points,
                "channels":           stats.channels,
                "data_types":         stats.data_types,
            }
        }))),
        Err(e) => {
            error!("data_range error: {}", e);
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"success": false, "message": e.to_string()})),
            ))
        },
    }
}

// ============================================================================
// Metadata
// ============================================================================

/// List channels that have persisted historical data.
///
/// Returns `[channel_id, ...]`. **Only channels that have at least one
/// written record are included** — this may differ from the set of channels
/// currently configured in io, because newly added channels do not
/// appear until their first data point is flushed. Intended for populating
/// "select a channel" dropdowns in the frontend.
#[utoipa::path(get, path = "/hisApi/channels", tag = "Meta",
    responses(
        (status = 200, description = "List of channels with persisted data"),
        (status = 500, description = "Query failed"),
    ))]
async fn list_channels(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let backend = state.storage.read().await.clone();
    match backend.list_channels().await {
        Ok(channels) => {
            let count = channels.len();
            Ok(Json(json!({
                "success": true,
                "message": format!("Found {} channel(s)", count),
                "data": channels,
                "count": count,
            })))
        },
        Err(e) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"success": false, "message": e.to_string()})),
        )),
    }
}

/// Runtime metrics for the history process.
///
/// Returns cumulative statistics since startup: total points written,
/// NaN-skipped points, current write-buffer depth, and last flush
/// duration. A continuously growing buffer depth indicates the backend
/// write throughput is not keeping up with the collection rate.
#[utoipa::path(get, path = "/hisApi/metrics", tag = "Meta",
    responses((status = 200, description = "Runtime metrics (total points, channel count, buffer depth, etc.)")))]
async fn metrics(State(state): State<Arc<AppState>>) -> Json<Value> {
    let backend = state.storage.read().await.clone();
    let stats = backend.get_stats().await.unwrap_or_else(|_| DataStats {
        earliest_timestamp: None,
        latest_timestamp: None,
        total_points: 0,
        channels: vec![],
        data_types: vec![],
    });

    Json(json!({
        "success": true,
        "message": "OK",
        "data": {
            "total_points":  stats.total_points,
            "channel_count": stats.channels.len(),
            "backend":       backend.name(),
            "buffer_size":   state.buffer.lock().await.len(),
        }
    }))
}

// ============================================================================
// General service config CRUD (intervals, patterns, etc.)
// ============================================================================

/// Retrieve the current history runtime configuration.
///
/// Returns collection interval, write batch size, point filter patterns,
/// retention period, and related settings. Storage backend connection
/// parameters are **not** included here — manage those via `/hisApi/storage`.
#[utoipa::path(get, path = "/hisApi/config", tag = "Config",
    responses((status = 200, description = "Current service configuration", body = ServiceConfig)))]
async fn get_config(State(state): State<Arc<AppState>>) -> Json<Value> {
    let cfg = state.config.read().await.clone();
    Json(json!({ "success": true, "message": "OK", "data": cfg }))
}

/// Update the history runtime configuration (full replacement).
///
/// Persists the new configuration to SQLite and **applies it immediately**
/// without restarting history. Changes to collection interval, batch size,
/// and point patterns take effect at once. Storage backend connection
/// parameters cannot be changed here — use `PUT /hisApi/storage` instead.
#[utoipa::path(put, path = "/hisApi/config", tag = "Config",
    request_body = ServiceConfig,
    responses(
        (status = 200, description = "Configuration updated"),
        (status = 500, description = "Failed to persist configuration"),
    ))]
async fn update_config(
    State(state): State<Arc<AppState>>,
    Json(mut new_cfg): Json<ServiceConfig>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    new_cfg.normalize();
    if let Err(e) = db_config::save_config(&state.sqlite, &new_cfg).await {
        error!("Failed to save config: {}", e);
        return Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"success": false, "message": e.to_string()})),
        ));
    }

    *state.config.write().await = new_cfg;

    Ok(Json(
        json!({ "success": true, "message": "Config updated" }),
    ))
}

// ============================================================================
// Storage backend config & control
// ============================================================================

/// Retrieve the current storage backend configuration and connection status.
///
/// Returns the active backend kind (Null / Postgres / Timescale / Influx),
/// its connection parameters (host, port, database name, etc.; **password
/// field is masked**), and the live connection status (`connected: true/false`
/// plus last error detail if any). Use this to verify the historical write
/// path is healthy. To change the configuration use `PUT /hisApi/storage`;
/// to test connectivity use `POST /hisApi/storage/test`.
#[utoipa::path(get, path = "/hisApi/storage", tag = "Storage",
    responses((status = 200, description = "Current storage backend configuration and connection status")))]
async fn get_storage(State(state): State<Arc<AppState>>) -> Json<Value> {
    let ss = state.storage_settings.read().await.clone();
    let backend = state.storage.read().await.clone();
    let healthy = backend.health_check().await;

    // Preserve the legacy response shape for external backends. For embedded
    // SQLite, `database` contains the local file and no network endpoint exists.
    let (host, port, database, username) = if ss.backend == "sqlite" {
        ("local".to_string(), 0, ss.url.clone(), String::new())
    } else {
        parse_dsn_fields(&ss.url)
    };

    Json(json!({
        "success": true,
        "message": "OK",
        "data": {
            "enabled":        ss.enabled,
            "backend":        ss.backend,
            "host":           host,
            "port":           port,
            "database":       database,
            "username":       username,
            "active_backend": backend.name(),
            "connected":      healthy,
        }
    }))
}

/// Save storage backend connection parameters (**does not connect immediately**).
///
/// This endpoint only persists configuration; it does not attempt a database
/// connection and does not affect the currently active backend.
/// After saving, use the following endpoints:
/// - `POST /hisApi/storage/test` — verify connectivity
/// - `POST /hisApi/storage/reconnect` — apply the new config and connect
#[utoipa::path(put, path = "/hisApi/storage", tag = "Storage",
    request_body = StorageConfigRequest,
    responses(
        (status = 200, description = "Parameters saved"),
        (status = 400, description = "Invalid parameters (missing required fields)"),
        (status = 500, description = "Failed to persist parameters"),
    ))]
async fn update_storage(
    State(state): State<Arc<AppState>>,
    Json(req): Json<StorageConfigRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let missing_required = if req.backend == "sqlite" {
        req.database.is_empty()
    } else {
        req.host.is_empty() || req.database.is_empty() || req.username.is_empty()
    };
    if missing_required {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(
                json!({"success": false, "message": "database path is required for sqlite; host, database, and username are required for external backends"}),
            ),
        ));
    }

    let dsn = if req.backend == "sqlite" {
        req.database.clone()
    } else {
        req.to_dsn()
    };

    let new_ss = StorageSettings {
        enabled: req.enabled,
        backend: req.backend.clone(),
        url: dsn,
    };

    if let Err(e) = db_config::save_storage(&state.sqlite, &new_ss).await {
        error!("Failed to persist storage config: {}", e);
        return Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"success": false, "message": e.to_string()})),
        ));
    }

    // If disabling, immediately swap in NullBackend so writes stop.
    if !req.enabled {
        *state.storage.write().await = Arc::new(NullBackend);
        info!("Storage disabled, writes stopped");
    }

    *state.storage_settings.write().await = new_ss;

    Ok(Json(json!({
        "success": true,
        "message": "Parameters saved. Call POST /hisApi/storage/reconnect to connect"
    })))
}

/// Test database connectivity using the supplied parameters.
///
/// Probes by connecting to the built-in `postgres` maintenance database (which
/// exists on every PostgreSQL / TimescaleDB server), so **the target business
/// database does not need to exist** for the test to pass.
/// Does not modify any runtime state or write any data.
#[utoipa::path(post, path = "/hisApi/storage/test", tag = "Storage",
    request_body = StorageTestRequest,
    responses(
        (status = 200, description = "Connection test successful"),
        (status = 500, description = "Connection failed; error detail in response body"),
    ))]
async fn test_storage(
    Json(req): Json<StorageTestRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let addr = req.addr();

    match probe_backend(&req).await {
        Ok(()) => Ok(Json(json!({
            "success": true,
            "message": format!("Successfully connected to {}", addr)
        }))),
        Err(e) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"success": false, "message": e.to_string()})),
        )),
    }
}

/// Connect (or reconnect) to the storage backend using the saved parameters.
///
/// On success, historical data collection begins immediately. If `enabled`
/// is currently `false`, this call has no effect — set `enabled = true` via
/// `PUT /hisApi/storage` first.
#[utoipa::path(post, path = "/hisApi/storage/reconnect", tag = "Storage",
    responses(
        (status = 200, description = "Reconnected successfully"),
        (status = 400, description = "Storage not configured or not enabled"),
        (status = 500, description = "Reconnection failed"),
    ))]
async fn reconnect_storage(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let (enabled, backend_type, dsn) = {
        let ss = state.storage_settings.read().await;
        (ss.enabled, ss.backend.clone(), ss.url.clone())
    };

    if !enabled {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(
                json!({"success": false, "message": "Storage is not enabled. Set enabled=true via PUT /hisApi/storage first"}),
            ),
        ));
    }

    if dsn.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(
                json!({"success": false, "message": "Storage parameters not configured. Call PUT /hisApi/storage first"}),
            ),
        ));
    }

    match connect_storage_backend(&backend_type, &dsn).await {
        Ok(b) => {
            info!("Storage backend '{}' reconnected", backend_type);
            *state.storage.write().await = b;
            Ok(Json(json!({
                "success": true,
                "message": format!("Connected to '{}' backend. Historical data collection started", backend_type)
            })))
        },
        Err(e) => {
            error!("Storage reconnect failed: {}", e);
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"success": false, "message": e.to_string()})),
            ))
        },
    }
}

// ============================================================================
// Helpers
// ============================================================================

/// Parse a PostgreSQL DSN back into (host, port, database, username) for the
/// GET /hisApi/storage response.  Returns empty strings on parse failure.
fn parse_dsn_fields(dsn: &str) -> (String, u16, String, String) {
    if let Ok(url) = url::Url::parse(dsn) {
        let host = url.host_str().unwrap_or("").to_string();
        let port = url.port().unwrap_or(5432);
        let database = url.path().trim_start_matches('/').to_string();
        let username = url.username().to_string();
        return (host, port, database, username);
    }
    (String::new(), 5432, String::new(), String::new())
}

/// Replace the database segment in a PostgreSQL DSN with `new_db`.
/// Used by the test endpoint to probe against the always-present `postgres` DB.
#[cfg(feature = "postgres-storage")]
fn replace_db_in_dsn(dsn: &str, new_db: &str) -> String {
    if let Ok(mut url) = url::Url::parse(dsn) {
        url.set_path(&format!("/{}", new_db));
        return url.to_string();
    }
    dsn.to_string()
}
