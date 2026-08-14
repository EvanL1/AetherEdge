//! `aether-api` — management API and WebSocket service.
//!
//! Unified remote application entry for AetherEdge clients:
//! - JWT auth (users, roles)
//! - WebSocket real-time data push with subscriptions
//! - POST /broadcast – push any JSON to all WebSocket clients
//! - GET /api/homepage – calculated points CRUD
//! - GET /api/network – read-only systemd-networkd view; remote writes disabled
//! - GET /api/config – admin-only configuration checks and export
//! - /api/{io,automation,history,uplink,alarm}/* – authenticated application gateway

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Query, State, WebSocketUpgrade},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{delete, get, post, put},
};
use dashmap::DashMap;
use md5::{Digest, Md5};
use serde::Deserialize;
use tokio_util::sync::CancellationToken;
use tower_http::cors::{Any, CorsLayer};
use tracing::info;
#[cfg(any(feature = "swagger-ui", all(test, feature = "openapi")))]
use utoipa::OpenApi;
#[cfg(feature = "swagger-ui")]
use utoipa_swagger_ui::{Config, SwaggerUi, Url};

mod auth;
mod config;
mod data_processing_runtime;
mod db;
mod live_values;
mod middleware_auth;
mod models;
#[cfg(feature = "swagger-ui")]
mod openapi_gateway;
mod read_models;
mod routes_auth;
mod routes_broadcast;
mod routes_config;
mod routes_data_processing;
mod routes_homepage;
mod routes_network;
mod service_gateway;
mod state;
#[cfg(test)]
mod test_support;
mod ws;

use crate::config::GatewayConfig;
use crate::live_values::{build_gateway_value_source, run_gateway_topology_refresh};
#[cfg(any(feature = "swagger-ui", all(test, feature = "openapi")))]
use crate::routes_data_processing::DataProcessingApiDoc;
use crate::state::AppState;
use crate::ws::WsHub;

const BOOTSTRAP_ADMIN_PASSWORD_ENV: &str = "AETHER_BOOTSTRAP_ADMIN_PASSWORD";
const MIN_BOOTSTRAP_ADMIN_PASSWORD_CHARS: usize = 16;
const MAX_WS_CLIENT_ID_BYTES: usize = 128;
const MAX_WS_DATA_TYPE_BYTES: usize = 64;
const HTTP_DRAIN_TIMEOUT: Duration = Duration::from_secs(5);

fn bootstrap_admin_login_digest(password: &str) -> String {
    format!("{:x}", Md5::digest(password.as_bytes()))
}

fn validate_bootstrap_admin_password(password: Option<&str>) -> anyhow::Result<&str> {
    let password = password.ok_or_else(|| {
        anyhow::anyhow!(
            "first startup requires {BOOTSTRAP_ADMIN_PASSWORD_ENV}; refusing to create an admin account with a public default password"
        )
    })?;
    let trimmed = password.trim();
    let normalized = trimmed.to_ascii_lowercase();
    if trimmed != password
        || trimmed.chars().count() < MIN_BOOTSTRAP_ADMIN_PASSWORD_CHARS
        || trimmed.chars().any(char::is_control)
        || matches!(
            normalized.as_str(),
            "admin123"
                | "change-me-in-production"
                | "changeme"
                | "password"
                | "0192023a7bbd73250516f069df18b500"
        )
    {
        anyhow::bail!(
            "{BOOTSTRAP_ADMIN_PASSWORD_ENV} must contain at least {MIN_BOOTSTRAP_ADMIN_PASSWORD_CHARS} characters, have no surrounding whitespace or control characters, and must not use a documented or common default"
        );
    }
    Ok(password)
}

/// Creates the initial administrator exactly once. Existing installations do
/// not need to retain the bootstrap secret in their environment.
async fn ensure_bootstrap_admin<F>(
    database: &sqlx::SqlitePool,
    bootstrap_password: F,
) -> anyhow::Result<bool>
where
    F: FnOnce() -> Option<String>,
{
    let user_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users")
        .fetch_one(database)
        .await?;
    if user_count != 0 {
        return Ok(false);
    }

    let bootstrap_password = bootstrap_password();
    let password = validate_bootstrap_admin_password(bootstrap_password.as_deref())?;
    let login_digest = bootstrap_admin_login_digest(password);
    let password_hash = auth::hash_password(&login_digest)?;
    db::create_user(database, "admin", &password_hash, 1).await?;
    Ok(true)
}

// ── OpenAPI / Swagger UI ──────────────────────────────────────────────────────
// ApiDoc / SecurityAddon are compiled only with the Swagger UI so shared
// admin annotations can remain opt-in through `common/openapi`.

#[cfg(any(feature = "swagger-ui", all(test, feature = "openapi")))]
#[derive(OpenApi)]
#[openapi(
    paths(
        service_info,
        health_check,
        liveness_check,
        readiness_check,
        ws_handler,
        routes_auth::register,
        routes_auth::login,
        routes_auth::refresh_token,
        routes_auth::logout,
        routes_auth::get_me,
        routes_auth::update_me,
        routes_auth::change_password,
        routes_auth::get_roles,
        routes_auth::get_all_users,
        routes_auth::admin_get_user,
        routes_auth::admin_update_user,
        routes_auth::admin_delete_user,
        routes_auth::get_auth_stats,
        routes_auth::cleanup_tokens,
        routes_auth::validate_token,
        routes_broadcast::broadcast_message,
        routes_broadcast::broadcast_status,
        routes_homepage::list_points,
        routes_homepage::get_point,
        routes_homepage::update_point,
        routes_homepage::reset_points,
        routes_network::get_network_config,
        routes_network::update_network_config,
        routes_network::apply_network_config,
        routes_config::check_config,
        routes_config::export_config,
        common::admin_api::get_log_level,
        common::admin_api::set_log_level,
        common::admin_api::list_log_files,
        common::admin_api::view_log_file,
    ),
    components(schemas(
        models::UserCreate,
        models::UserLogin,
        models::UserUpdate,
        models::PasswordChange,
        models::RefreshTokenRequest,
        models::TokenResponse,
        models::GatewayDataResponse<models::TokenResponse>,
        models::GatewayDataResponse<models::RegistrationResult>,
        models::GatewayDataResponse<models::UserWithRole>,
        models::GatewayDataResponse<models::UserListData>,
        models::GatewayDataResponse<models::DeletedUserData>,
        models::GatewayDataResponse<models::AuthStatsData>,
        models::GatewayDataResponse<models::CalculatedPoint>,
        models::GatewayDataResponse<models::NetworkConfig>,
        models::GatewayMessageResponse,
        models::RegistrationResult,
        models::RoleListResponse,
        models::UserListData,
        models::DeletedUserData,
        models::AuthStatsData,
        models::HomepagePageData,
        models::HomepageResetData,
        models::GatewayDataResponse<models::HomepagePageData>,
        models::GatewayDataResponse<models::HomepageResetData>,
        models::GatewayDataResponse<serde_json::Value>,
        models::Role,
        models::RoleInfo,
        models::UserWithRole,
        models::CalculatedPoint,
        models::CalculatedPointUpdate,
        models::NetworkConfig,
        routes_config::ConfigArchive,
        common::admin_api::SetLogLevelRequest,
        common::admin_api::LogLevelResponse,
    )),
    tags(
        (name = "Auth", description = "Authentication and user management"),
        (name = "Homepage", description = "Operator dashboard point-definition CRUD"),
        (name = "Network", description = "Read-only network interface inspection; remote mutation is disabled"),
        (name = "Config", description = "System configuration export / import / upgrade"),
        (name = "WebSocket", description = "WebSocket broadcast and status"),
        (name = "Meta", description = "Service metadata and health"),
        (name = "admin", description = "Authenticated runtime administration"),
    ),
    modifiers(&SecurityAddon),
    info(
        title = "Aether API Gateway",
        version = env!("CARGO_PKG_VERSION"),
        description = "Authenticated remote-management API and WebSocket gateway. Protected operations require a Bearer JWT; use the service-local APIs only for intra-host communication. When compiled in, /docs and gateway-proxied /openapi/*.json are public and must only be exposed on a trusted commissioning network."
    )
)]
struct ApiDoc;

#[cfg(any(feature = "swagger-ui", all(test, feature = "openapi")))]
struct SecurityAddon;
#[cfg(any(feature = "swagger-ui", all(test, feature = "openapi")))]
impl utoipa::Modify for SecurityAddon {
    fn modify(&self, openapi: &mut utoipa::openapi::OpenApi) {
        if let Some(components) = openapi.components.as_mut() {
            components.add_security_scheme(
                "bearer_auth",
                utoipa::openapi::security::SecurityScheme::Http(
                    utoipa::openapi::security::HttpBuilder::new()
                        .scheme(utoipa::openapi::security::HttpAuthScheme::Bearer)
                        .bearer_format("JWT")
                        .build(),
                ),
            );
            components.add_security_scheme(
                "ws_query_token",
                utoipa::openapi::security::SecurityScheme::ApiKey(
                    utoipa::openapi::security::ApiKey::Query(
                        utoipa::openapi::security::ApiKeyValue::with_description(
                            "token",
                            "Access JWT fallback for browser WebSocket upgrades only",
                        ),
                    ),
                ),
            );
        }

        let bearer = || {
            vec![utoipa::openapi::security::SecurityRequirement::new(
                "bearer_auth",
                Vec::<String>::new(),
            )]
        };
        for (path, item) in &mut openapi.paths.paths {
            if !path.starts_with("/api/admin/") {
                continue;
            }
            if let Some(operation) = item.get.as_mut() {
                operation.security = Some(bearer());
            }
            if let Some(operation) = item.post.as_mut() {
                operation.security = Some(bearer());
            }
        }
    }
}

// ── WebSocket endpoint ────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct WsParams {
    client_id: Option<String>,
    data_type: Option<String>,
}

fn validate_ws_params(params: &WsParams) -> Result<(), &'static str> {
    if params
        .client_id
        .as_ref()
        .is_some_and(|client_id| client_id.len() > MAX_WS_CLIENT_ID_BYTES)
    {
        return Err("client_id exceeds the 128-byte limit");
    }
    if params
        .data_type
        .as_ref()
        .is_some_and(|data_type| data_type.len() > MAX_WS_DATA_TYPE_BYTES)
    {
        return Err("data_type exceeds the 64-byte limit");
    }
    Ok(())
}

fn invalid_ws_query_response(message: &str) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(serde_json::json!({
            "error": {
                "code": "INVALID_WS_QUERY",
                "message": message,
            }
        })),
    )
        .into_response()
}

async fn validate_ws_query_before_upgrade(
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    let params = match Query::<WsParams>::try_from_uri(request.uri()) {
        Ok(Query(params)) => params,
        Err(rejection) => return rejection.into_response(),
    };
    if let Err(message) = validate_ws_params(&params) {
        return invalid_ws_query_response(message);
    }
    next.run(request).await
}

#[utoipa::path(
    get,
    path = "/ws",
    params(
        ("client_id" = Option<String>, Query, description = "Optional client identifier"),
        ("data_type" = Option<String>, Query, description = "Subscription data category"),
        ("token" = Option<String>, Query, description = "Access JWT fallback for browser WebSocket upgrades; normal HTTP requests must use the Authorization header")
    ),
    responses(
        (status = 101, description = "WebSocket protocol upgrade"),
        (status = 400, description = "WebSocket query parameter exceeds its size limit"),
        (status = 401, description = "Missing or invalid access token")
    ),
    security(
        ("bearer_auth" = []),
        ("ws_query_token" = [])
    ),
    tag = "WebSocket"
)]
async fn ws_handler(
    ws: WebSocketUpgrade,
    Query(params): Query<WsParams>,
    State(state): State<Arc<AppState>>,
) -> Response {
    if let Err(message) = validate_ws_params(&params) {
        return invalid_ws_query_response(message);
    }
    let client_id = params
        .client_id
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let data_type = params.data_type.unwrap_or_else(|| "general".to_string());
    let hub = Arc::clone(&state.ws_hub);

    ws.max_message_size(64 * 1024)
        .max_frame_size(64 * 1024)
        .on_upgrade(move |socket| ws::handle_socket(socket, client_id, data_type, hub))
        .into_response()
}

#[utoipa::path(
    get,
    path = "/",
    responses((status = 200, description = "Service name", body = String, content_type = "text/plain")),
    tag = "Meta"
)]
async fn service_info() -> &'static str {
    "Aether API Gateway"
}

#[utoipa::path(
    get,
    path = "/health",
    responses((status = 200, description = "Service is healthy", body = String, content_type = "text/plain")),
    tag = "Meta"
)]
async fn health_check() -> &'static str {
    "ok"
}

#[utoipa::path(
    get,
    path = "/live",
    responses((status = 200, description = "Process is alive", body = String, content_type = "text/plain")),
    tag = "Meta"
)]
async fn liveness_check() -> &'static str {
    "ok"
}

#[utoipa::path(
    get,
    path = "/ready",
    responses(
        (status = 200, description = "Gateway dependencies are ready"),
        (status = 503, description = "A required local dependency is unavailable")
    ),
    tag = "Meta"
)]
async fn readiness_check(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let database = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        sqlx::query("SELECT 1").execute(&state.db),
    )
    .await;
    match database {
        Ok(Ok(_)) => (
            axum::http::StatusCode::OK,
            Json(serde_json::json!({"status": "ready"})),
        ),
        Ok(Err(error)) => {
            tracing::warn!(%error, "API readiness database probe failed");
            (
                axum::http::StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({"status": "not_ready"})),
            )
        },
        Err(_) => {
            tracing::warn!("API readiness database probe timed out");
            (
                axum::http::StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({"status": "not_ready"})),
            )
        },
    }
}

// ── Router ────────────────────────────────────────────────────────────────────

fn commissioned_data_processing_router(state: &AppState) -> Option<Router<Arc<AppState>>> {
    state
        .data_processing
        .as_ref()
        .map(|_| routes_data_processing::router())
}

#[cfg(feature = "swagger-ui")]
fn gateway_openapi(state: &AppState) -> utoipa::openapi::OpenApi {
    if state.data_processing.is_some() {
        ApiDoc::openapi().nest("", DataProcessingApiDoc::openapi())
    } else {
        ApiDoc::openapi()
    }
}

#[cfg(feature = "swagger-ui")]
async fn gateway_openapi_document(
    State(state): State<Arc<AppState>>,
) -> axum::Json<utoipa::openapi::OpenApi> {
    axum::Json(gateway_openapi(&state))
}

fn build_router(state: Arc<AppState>) -> Router {
    let auth_routes = Router::new()
        .route("/register", post(routes_auth::register))
        .route("/login", post(routes_auth::login))
        .route("/refresh", post(routes_auth::refresh_token))
        .route("/logout", post(routes_auth::logout))
        .route("/me", get(routes_auth::get_me).put(routes_auth::update_me))
        .route("/me/password", put(routes_auth::change_password))
        .route("/roles", get(routes_auth::get_roles))
        .route("/users", get(routes_auth::get_all_users))
        .route("/users/{id}", get(routes_auth::admin_get_user))
        .route("/users/{id}", put(routes_auth::admin_update_user))
        .route("/users/{id}", delete(routes_auth::admin_delete_user))
        .route("/stats", get(routes_auth::get_auth_stats))
        .route("/cleanup-tokens", post(routes_auth::cleanup_tokens))
        .route("/validate", get(routes_auth::validate_token));

    let homepage_routes = Router::new()
        .route("/", get(routes_homepage::list_points))
        .route("/reset", post(routes_homepage::reset_points))
        .route("/{id}", get(routes_homepage::get_point))
        .route("/{id}", put(routes_homepage::update_point));

    let network_routes = Router::new()
        .route("/", get(routes_network::get_network_config))
        .route("/", put(routes_network::update_network_config))
        .route("/apply", post(routes_network::apply_network_config));

    let config_routes = Router::new()
        .route("/check", get(routes_config::check_config))
        .route("/export", get(routes_config::export_config));

    // Routes that require auth. Layered ONCE on the merged router so
    // adding a new sub-router (e.g. /reports) cannot accidentally skip
    // the JWT check the way per-route layering did before this fix.
    // Includes anything that mutates state, exposes admin operations,
    // or pushes data to other clients (broadcast). /auth is the only
    // public surface (register/login/refresh) and is mounted below
    // without the layer.
    let protected_api = Router::new()
        .route("/broadcast", post(routes_broadcast::broadcast_message))
        .route("/broadcast/status", get(routes_broadcast::broadcast_status))
        .nest("/homepage", homepage_routes)
        .nest("/network", network_routes)
        .nest("/config", config_routes)
        .merge(service_gateway::router());
    let protected_api = match commissioned_data_processing_router(&state) {
        Some(routes) => protected_api.nest("/data-processing", routes),
        None => protected_api,
    };
    let protected_api = protected_api.layer(axum::middleware::from_fn_with_state(
        Arc::clone(&state),
        middleware_auth::require_jwt,
    ));

    let api = Router::new()
        .merge(protected_api)
        .nest("/auth", auth_routes);

    // Narrow service-to-service surface. Its dedicated credential is checked
    // by the exact alarm handler and grants no authority over generic operator
    // broadcasts or any other management route.
    let internal_api = Router::new().route(
        "/alarm-events",
        post(routes_broadcast::receive_alarm_event).layer(DefaultBodyLimit::max(64 * 1024)),
    );

    // /api/admin/* — runtime log control. Must require auth: leaving these
    // open lets an attacker quietly escalate log verbosity or read log
    // files. Grouped into its own Router so the require_jwt layer covers
    // any future admin route added inside.
    let admin_routes = Router::new()
        .route(
            "/logs/level",
            get(common::admin_api::get_log_level).post(common::admin_api::set_log_level),
        )
        .route("/logs/files", get(common::admin_api::list_log_files))
        .route("/logs/view", get(common::admin_api::view_log_file))
        .layer(axum::middleware::from_fn_with_state(
            Arc::clone(&state),
            middleware_auth::require_jwt,
        ));

    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    let app = Router::new()
        .route("/", get(service_info))
        .route("/health", get(health_check))
        .route("/live", get(liveness_check))
        .route("/ready", get(readiness_check))
        .route(
            "/ws",
            get(ws_handler)
                .route_layer(axum::middleware::from_fn(validate_ws_query_before_upgrade))
                .route_layer(axum::middleware::from_fn_with_state(
                    Arc::clone(&state),
                    middleware_auth::require_jwt,
                )),
        )
        .nest("/api", api)
        .nest("/api/internal", internal_api)
        .nest("/api/admin", admin_routes);

    #[cfg(feature = "swagger-ui")]
    let app = {
        let app = app
            .route("/openapi/gateway.json", get(gateway_openapi_document))
            .route("/openapi/{service}", get(openapi_gateway::service_openapi));
        app.merge(
            SwaggerUi::new("/docs").config(
                Config::new([
                    Url::with_primary("Aether API Gateway", "/openapi/gateway.json", true),
                    Url::new("Aether I/O", "/openapi/io.json"),
                    Url::new("Aether Automation", "/openapi/automation.json"),
                    Url::new("Aether History", "/openapi/history.json"),
                    Url::new("Aether Uplink", "/openapi/uplink.json"),
                    Url::new("Aether Alarm", "/openapi/alarm.json"),
                ])
                .default_model_rendering("model")
                .default_models_expand_depth(1),
            ),
        )
    };

    app.with_state(state)
        .layer(DefaultBodyLimit::max(1024 * 1024))
        .layer(axum::middleware::from_fn(
            common::logging::http_request_logger,
        ))
        .layer(cors)
}

// ── Main ──────────────────────────────────────────────────────────────────────

fn bind_api_listener(addr: SocketAddr) -> anyhow::Result<tokio::net::TcpListener> {
    let socket = if addr.is_ipv4() {
        tokio::net::TcpSocket::new_v4()?
    } else {
        tokio::net::TcpSocket::new_v6()?
    };
    socket.set_reuseaddr(true)?;
    socket.bind(addr)?;
    Ok(socket.listen(1024)?)
}

async fn serve_bound_api(
    listener: tokio::net::TcpListener,
    app: Router,
    shutdown: CancellationToken,
) -> anyhow::Result<()> {
    let graceful_shutdown = shutdown.clone();
    let drain_signal = shutdown.clone();
    let mut server = std::pin::pin!(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(async move {
                tokio::select! {
                    _ = common::shutdown::wait_for_shutdown() => {
                        info!("Shutdown signal received");
                        graceful_shutdown.cancel();
                    },
                    _ = graceful_shutdown.cancelled() => {
                        tracing::warn!("Internal API shutdown requested");
                    },
                }
            })
            .await
    });

    let result = tokio::select! {
        result = &mut server => result.map_err(anyhow::Error::from),
        _ = drain_signal.cancelled() => {
            match tokio::time::timeout(HTTP_DRAIN_TIMEOUT, &mut server).await {
                Ok(result) => result.map_err(anyhow::Error::from),
                Err(_) => {
                    tracing::warn!(
                        timeout_ms = HTTP_DRAIN_TIMEOUT.as_millis(),
                        "API HTTP connections exceeded the graceful-drain deadline; closing them"
                    );
                    Ok(())
                },
            }
        },
    };

    result
}

async fn finish_api_runtime(
    server_result: anyhow::Result<()>,
    shutdown: CancellationToken,
    supervisor_task: tokio::task::JoinHandle<anyhow::Result<()>>,
) -> anyhow::Result<()> {
    shutdown.cancel();
    let supervisor_result = supervisor_task
        .await
        .map_err(|error| anyhow::anyhow!("API task supervisor join failed: {error}"))
        .and_then(|result| result);

    match (server_result, supervisor_result) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(server_error), Ok(())) => Err(server_error),
        (Ok(()), Err(supervisor_error)) => Err(supervisor_error),
        (Err(server_error), Err(supervisor_error)) => Err(anyhow::anyhow!(
            "API HTTP server failed: {server_error:#}; API task supervisor failed: {supervisor_error:#}"
        )),
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cfg = GatewayConfig::from_env()?;

    // ── Logging ───────────────────────────────────────────────────────────────
    common::service_bootstrap::init_service("aether-api", "API Gateway service", cfg.api_port)?;

    info!("aether-api starting on port {}", cfg.api_port);
    info!("SHM:   {}", cfg.shm_path);
    info!("Health SHM: {}", cfg.channel_health_shm_path);
    info!("PointWatch: {}", cfg.point_watch_socket);
    info!("DB:    {}", cfg.db_path);

    // ── SQLite ────────────────────────────────────────────────────────────────
    let db_pool = common::bootstrap_database::open_service_pool(&cfg.db_path).await?;

    db::create_tables(&db_pool).await?;
    db::init_roles(&db_pool).await?;
    db::init_calculated_points(&db_pool).await?;

    let live_values = build_gateway_value_source(&db_pool, &cfg).await?;
    let point_watch_capacity = aether_sqlite_topology::load_sqlite_shm_capacity(&db_pool)
        .await
        .map_err(|error| anyhow::anyhow!("load PointWatch capacity: {error}"))?;

    // Data Processing is composed only after explicit deployment opt-in. A
    // disabled deployment neither constructs source/processor clients nor
    // mounts the corresponding HTTP routes. Its read-only live state shares
    // the gateway's atomically refreshed topology generation.
    let data_processing = data_processing_runtime::build_data_processing_application(
        &db_pool,
        &cfg,
        Arc::clone(&live_values),
    )
    .await?;

    // ── Bootstrap admin user ──────────────────────────────────────────────────
    ensure_bootstrap_admin(&db_pool, || {
        std::env::var(BOOTSTRAP_ADMIN_PASSWORD_ENV).ok()
    })
    .await?;

    // ── App State ─────────────────────────────────────────────────────────────
    let ws_hub = WsHub::new(
        live_values.clone(),
        db_pool.clone(),
        cfg.shm_writer_stale_after_ms,
    );
    let service_client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(
            cfg.service_request_timeout_secs,
        ))
        .build()?;

    let state = Arc::new(AppState {
        db: db_pool,
        config: Arc::new(cfg),
        ws_hub: Arc::clone(&ws_hub),
        data_processing,
        refresh_tokens: DashMap::new(),
        service_client,
    });

    // Bind before starting any critical background task. A bad address or an
    // occupied port must fail startup without leaving detached workers behind.
    let app = build_router(Arc::clone(&state));
    let bind_addr = common::bind_address(&state.config.api_host, state.config.api_port)?;
    let listener = bind_api_listener(bind_addr)?;
    info!("Listening on {}", bind_addr);

    // ── Background tasks ──────────────────────────────────────────────────────
    let shutdown = CancellationToken::new();

    let topology_source = Arc::clone(&live_values);
    let topology_db = state.db.clone();
    let topology_config = state.config.as_ref().clone();
    let topology_shutdown = shutdown.clone();
    let mut supervisor =
        common::task_supervisor::CriticalTaskSupervisor::new(std::time::Duration::from_secs(5));
    supervisor.spawn("gateway-topology-refresh", async move {
        run_gateway_topology_refresh(
            topology_source,
            topology_db,
            topology_config,
            topology_shutdown,
        )
        .await;
    });

    let hb_hub = Arc::clone(&ws_hub);
    let hb_shutdown = shutdown.clone();
    supervisor.spawn("websocket-heartbeat", async move {
        ws::run_heartbeat(hb_hub, hb_shutdown).await;
    });

    let push_hub = Arc::clone(&ws_hub);
    let push_shutdown = shutdown.clone();
    let push_interval = state.config.data_fetch_interval_secs;
    let push_shm_path = state.config.shm_path.clone();
    let push_socket = state.config.point_watch_socket.clone();
    let push_debounce_ms = state.config.point_watch_debounce_ms;
    supervisor.spawn("websocket-data-push", async move {
        ws::run_data_push(
            push_hub,
            push_shutdown,
            push_interval,
            &push_shm_path,
            &push_socket,
            push_debounce_ms,
            point_watch_capacity,
        )
        .await;
    });
    let supervisor_shutdown = shutdown.clone();
    let supervisor_task = tokio::spawn(async move { supervisor.run(supervisor_shutdown).await });

    // ── HTTP server ───────────────────────────────────────────────────────────
    let server_result = serve_bound_api(listener, app, shutdown.clone()).await;
    let runtime_result = finish_api_runtime(server_result, shutdown, supervisor_task).await;
    common::logging::shutdown_logging_tasks().await;
    runtime_result?;

    info!("api stopped");
    Ok(())
}

#[cfg(test)]
mod runtime_lifecycle_tests {
    use std::sync::atomic::{AtomicBool, Ordering};

    use super::*;

    #[tokio::test]
    async fn listener_bind_failure_is_reported_synchronously() {
        let occupied =
            std::net::TcpListener::bind("127.0.0.1:0").expect("reserve an isolated API test port");
        let address = occupied.local_addr().expect("reserved port address");

        assert!(
            bind_api_listener(address).is_err(),
            "the API listener must report an occupied port before workers start"
        );
    }

    #[tokio::test]
    async fn server_failure_cancels_and_joins_background_tasks() {
        let shutdown = CancellationToken::new();
        let task_shutdown = shutdown.clone();
        let drained = Arc::new(AtomicBool::new(false));
        let drained_by_task = Arc::clone(&drained);
        let supervisor_task = tokio::spawn(async move {
            task_shutdown.cancelled().await;
            drained_by_task.store(true, Ordering::Release);
            Ok(())
        });

        let error = finish_api_runtime(
            Err(anyhow::anyhow!("synthetic HTTP server failure")),
            shutdown.clone(),
            supervisor_task,
        )
        .await
        .expect_err("server failure must remain visible");

        assert!(shutdown.is_cancelled());
        assert!(drained.load(Ordering::Acquire));
        assert!(error.to_string().contains("synthetic HTTP server failure"));
    }

    #[tokio::test]
    async fn concurrent_server_and_supervisor_failures_are_both_reported() {
        let shutdown = CancellationToken::new();
        let supervisor_task =
            tokio::spawn(async { Err(anyhow::anyhow!("synthetic supervisor failure")) });

        let error = finish_api_runtime(
            Err(anyhow::anyhow!("synthetic HTTP server failure")),
            shutdown,
            supervisor_task,
        )
        .await
        .expect_err("combined failure must remain visible");
        let message = error.to_string();
        assert!(message.contains("synthetic HTTP server failure"));
        assert!(message.contains("synthetic supervisor failure"));
    }
}

#[cfg(test)]
mod bootstrap_admin_tests {
    use std::sync::atomic::{AtomicBool, Ordering};

    use crate::test_support::app_state;

    use super::*;

    #[tokio::test]
    async fn first_start_rejects_missing_or_public_bootstrap_passwords() {
        let state = app_state().await;

        let missing = ensure_bootstrap_admin(&state.db, || None)
            .await
            .expect_err("first start must require an explicit bootstrap secret");
        assert!(
            missing
                .to_string()
                .contains("AETHER_BOOTSTRAP_ADMIN_PASSWORD")
        );

        for weak in [
            "admin123",
            "change-me-in-production",
            "                ",
            " leading-or-trailing-space ",
        ] {
            ensure_bootstrap_admin(&state.db, || Some(weak.to_owned()))
                .await
                .expect_err("documented or fixed bootstrap passwords must be rejected");
        }
    }

    #[tokio::test]
    async fn strong_bootstrap_password_creates_admin_once_without_default_fallback() {
        let state = app_state().await;
        let password = "correct-horse-battery-staple-2026";

        let created = ensure_bootstrap_admin(&state.db, || Some(password.to_owned()))
            .await
            .expect("create bootstrap admin");
        assert!(created);

        let admin = db::get_user_by_username(&state.db, "admin")
            .await
            .expect("query bootstrap admin")
            .expect("bootstrap admin exists");
        let login_digest = bootstrap_admin_login_digest(password);
        assert!(auth::verify_password(&login_digest, &admin.password_hash));
        assert_eq!(admin.role_id, 1);

        let created_again = ensure_bootstrap_admin(&state.db, || None)
            .await
            .expect("existing admin must not require the bootstrap secret again");
        assert!(!created_again);
    }

    #[tokio::test]
    async fn bootstrap_secret_is_never_consumed_after_any_user_exists() {
        let state = app_state().await;
        db::create_user(&state.db, "existing-viewer", "unused-test-hash", 3)
            .await
            .expect("seed an existing user");
        let provider_called = AtomicBool::new(false);

        let created = ensure_bootstrap_admin(&state.db, || {
            provider_called.store(true, Ordering::Relaxed);
            Some("this-secret-must-not-be-read".to_owned())
        })
        .await
        .expect("an initialized user database must skip bootstrap");

        assert!(!created);
        assert!(!provider_called.load(Ordering::Relaxed));
        assert!(
            db::get_user_by_username(&state.db, "admin")
                .await
                .expect("query admin after skipped bootstrap")
                .is_none()
        );
    }
}

#[cfg(test)]
mod service_gateway_route_tests {
    use axum::{
        body::{Body, to_bytes},
        http::{Request, StatusCode},
    };
    use tower::ServiceExt;

    use crate::test_support::{app_state, authorization_headers};

    use super::{
        MAX_WS_CLIENT_ID_BYTES, MAX_WS_DATA_TYPE_BYTES, WsParams, build_router, validate_ws_params,
    };

    #[tokio::test]
    async fn liveness_and_readiness_are_separate_public_probes() {
        let app = build_router(app_state().await);
        for path in ["/health", "/live", "/ready"] {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .uri(path)
                        .body(Body::empty())
                        .expect("valid probe request"),
                )
                .await
                .expect("probe response");
            assert_eq!(response.status(), StatusCode::OK, "probe {path}");
        }
    }

    #[tokio::test]
    async fn retired_api_paths_are_not_mounted() {
        let app = build_router(app_state().await);
        for (path, method) in [
            ("/api/v1/auth/login", "POST"),
            ("/api/v1/io/health", "POST"),
            ("/api/internal/v1/alarm-events", "POST"),
            ("/api/config/import", "POST"),
            ("/api/config/restart-services", "POST"),
            ("/api/config/upgrade", "POST"),
            ("/api/config/upgrade/abort", "POST"),
            ("/api/config/upgrade/status", "GET"),
        ] {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method(method)
                        .uri(path)
                        .body(Body::empty())
                        .expect("valid obsolete path request"),
                )
                .await
                .expect("obsolete path response");
            assert_eq!(response.status(), StatusCode::NOT_FOUND, "path {path}");
        }
    }

    #[test]
    fn websocket_query_limits_are_byte_exact() {
        let accepted = WsParams {
            client_id: Some("c".repeat(MAX_WS_CLIENT_ID_BYTES)),
            data_type: Some("d".repeat(MAX_WS_DATA_TYPE_BYTES)),
        };
        assert!(validate_ws_params(&accepted).is_ok());

        let client_id_too_long = WsParams {
            client_id: Some("c".repeat(MAX_WS_CLIENT_ID_BYTES + 1)),
            data_type: None,
        };
        assert!(validate_ws_params(&client_id_too_long).is_err());

        let data_type_too_long = WsParams {
            client_id: None,
            data_type: Some("d".repeat(MAX_WS_DATA_TYPE_BYTES + 1)),
        };
        assert!(validate_ws_params(&data_type_too_long).is_err());
    }

    #[tokio::test]
    async fn oversized_websocket_query_is_rejected_before_upgrade() {
        let app = build_router(app_state().await);
        for query in [
            format!("client_id={}", "c".repeat(MAX_WS_CLIENT_ID_BYTES + 1)),
            format!("data_type={}", "d".repeat(MAX_WS_DATA_TYPE_BYTES + 1)),
        ] {
            let mut request = Request::builder()
                .uri(format!("/ws?{query}"))
                .header("connection", "upgrade")
                .header("upgrade", "websocket")
                .header("sec-websocket-version", "13")
                .header("sec-websocket-key", "dGhlIHNhbXBsZSBub25jZQ==")
                .body(Body::empty())
                .expect("valid WebSocket upgrade request");
            request
                .headers_mut()
                .extend(authorization_headers("Viewer"));

            let response = app
                .clone()
                .oneshot(request)
                .await
                .expect("WebSocket query response");
            assert_eq!(response.status(), StatusCode::BAD_REQUEST, "query {query}");
        }
    }

    #[tokio::test]
    async fn internal_application_gateway_is_mounted_only_behind_jwt_authentication() {
        let state = app_state().await;
        let app = build_router(state);

        let unauthenticated = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/io/health")
                    .body(Body::empty())
                    .expect("valid unauthenticated request"),
            )
            .await
            .expect("gateway response");
        assert_eq!(unauthenticated.status(), StatusCode::UNAUTHORIZED);

        let mut authenticated_request = Request::builder()
            .uri("/api/io/health")
            .body(Body::empty())
            .expect("valid authenticated request");
        *authenticated_request.headers_mut() = authorization_headers("Engineer");
        let authenticated = app
            .oneshot(authenticated_request)
            .await
            .expect("gateway response");
        assert_eq!(authenticated.status(), StatusCode::BAD_GATEWAY);
        let body = to_bytes(authenticated.into_body(), 16 * 1024)
            .await
            .expect("read gateway response");
        assert!(String::from_utf8_lossy(&body).contains("UPSTREAM_UNAVAILABLE"));
    }

    #[tokio::test]
    async fn uplink_gateway_is_authenticated_read_only_health_only() {
        let app = build_router(app_state().await);

        let unauthenticated_health = Request::builder()
            .uri("/api/uplink/health")
            .body(Body::empty())
            .expect("valid health request");
        let unauthenticated = app
            .clone()
            .oneshot(unauthenticated_health)
            .await
            .expect("unauthenticated health response");
        assert_eq!(unauthenticated.status(), StatusCode::UNAUTHORIZED);

        let mut authenticated_health = Request::builder()
            .uri("/api/uplink/health")
            .body(Body::empty())
            .expect("valid health request");
        authenticated_health
            .headers_mut()
            .extend(authorization_headers("Viewer"));
        let health = app
            .clone()
            .oneshot(authenticated_health)
            .await
            .expect("authenticated health response");
        assert_eq!(health.status(), StatusCode::BAD_GATEWAY);

        let mut retired_mqtt = Request::builder()
            .uri("/api/uplink/mqtt/status")
            .body(Body::empty())
            .expect("retired MQTT request");
        retired_mqtt
            .headers_mut()
            .extend(authorization_headers("Viewer"));
        let retired = app
            .clone()
            .oneshot(retired_mqtt)
            .await
            .expect("retired MQTT response");
        assert_eq!(retired.status(), StatusCode::BAD_REQUEST);

        for path in [
            "/api/uplink/api/admin/logs/files",
            "/api/uplink/api/internal/alarm-events",
        ] {
            let mut request = Request::builder()
                .uri(path)
                .body(Body::empty())
                .expect("internal-only request");
            request.headers_mut().extend(authorization_headers("Admin"));
            let response = app
                .clone()
                .oneshot(request)
                .await
                .expect("internal-only response");
            assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{path}");
        }
    }

    #[tokio::test]
    async fn governed_mutations_use_one_canonical_request_id_end_to_end() {
        let app = build_router(app_state().await);

        let mut missing_device_command_id = Request::builder()
            .method("POST")
            .uri("/api/automation/api/instances/7/action")
            .header("x-aether-confirmed", "true")
            .body(Body::empty())
            .expect("valid device action request");
        missing_device_command_id
            .headers_mut()
            .extend(authorization_headers("Engineer"));
        let missing_device_command_id_response = app
            .clone()
            .oneshot(missing_device_command_id)
            .await
            .expect("missing device command ID response");
        assert_eq!(
            missing_device_command_id_response.status(),
            StatusCode::BAD_REQUEST
        );
        let missing_body = to_bytes(missing_device_command_id_response.into_body(), 16 * 1024)
            .await
            .expect("read missing request ID response");
        assert!(String::from_utf8_lossy(&missing_body).contains("MISSING_REQUEST_ID"));

        let mut generated_request = Request::builder()
            .method("POST")
            .uri("/api/io/api/channels/7/commands")
            .header("x-aether-confirmed", "true")
            .body(Body::empty())
            .expect("valid governed request");
        generated_request
            .headers_mut()
            .extend(authorization_headers("Engineer"));
        let generated_response = app
            .clone()
            .oneshot(generated_request)
            .await
            .expect("generated request ID response");
        assert_eq!(generated_response.status(), StatusCode::BAD_GATEWAY);
        let generated = generated_response
            .headers()
            .get("x-request-id")
            .and_then(|value| value.to_str().ok())
            .expect("generated request ID response header");
        let parsed = uuid::Uuid::parse_str(generated).expect("generated canonical UUID");
        assert_eq!(parsed.to_string(), generated);

        let mut invalid_request = Request::builder()
            .method("POST")
            .uri("/api/io/api/channels/7/commands")
            .header("x-aether-confirmed", "true")
            .header("x-request-id", "not-a-uuid")
            .body(Body::empty())
            .expect("valid HTTP request");
        invalid_request
            .headers_mut()
            .extend(authorization_headers("Engineer"));
        let invalid_response = app
            .clone()
            .oneshot(invalid_request)
            .await
            .expect("invalid request ID response");
        assert_eq!(invalid_response.status(), StatusCode::BAD_REQUEST);
        let body = to_bytes(invalid_response.into_body(), 16 * 1024)
            .await
            .expect("read invalid request ID response");
        assert!(String::from_utf8_lossy(&body).contains("INVALID_REQUEST_ID"));

        let command_id = "0190aee621397a878448806f1b843201";
        let mut outcome_request = Request::builder()
            .uri(format!("/api/io/api/commands/{command_id}/outcome"))
            .body(Body::empty())
            .expect("valid outcome query");
        outcome_request
            .headers_mut()
            .extend(authorization_headers("Viewer"));
        let outcome_response = app
            .oneshot(outcome_request)
            .await
            .expect("outcome query response");
        assert_eq!(outcome_response.status(), StatusCode::BAD_GATEWAY);
        assert!(!outcome_response.headers().contains_key("x-request-id"));
    }
}

#[cfg(all(test, feature = "openapi"))]
mod openapi_tests {
    use super::*;

    fn json(document: utoipa::openapi::OpenApi) -> serde_json::Value {
        serde_json::to_value(document).expect("serialize OpenAPI document")
    }

    #[test]
    fn gateway_openapi_matches_always_mounted_routes_and_security() {
        let specification = json(ApiDoc::openapi());

        assert_eq!(specification["info"]["title"], "Aether API Gateway");
        assert_eq!(specification["info"]["version"], env!("CARGO_PKG_VERSION"));
        assert!(
            !specification["info"]["title"]
                .as_str()
                .expect("title string")
                .contains(&["Aether", "EMS"].concat())
        );

        for (path, method) in [
            ("/", "get"),
            ("/health", "get"),
            ("/live", "get"),
            ("/ready", "get"),
            ("/ws", "get"),
            ("/api/auth/validate", "get"),
            ("/api/admin/logs/level", "get"),
            ("/api/admin/logs/level", "post"),
            ("/api/admin/logs/files", "get"),
            ("/api/admin/logs/view", "get"),
        ] {
            assert!(
                specification["paths"][path][method].is_object(),
                "missing {method} {path}"
            );
        }

        for (path, method) in [
            ("/ws", "get"),
            ("/api/auth/validate", "get"),
            ("/api/admin/logs/level", "get"),
            ("/api/admin/logs/level", "post"),
            ("/api/admin/logs/files", "get"),
            ("/api/admin/logs/view", "get"),
        ] {
            assert_eq!(
                specification["paths"][path][method]["security"][0]["bearer_auth"],
                serde_json::json!([]),
                "missing Bearer security on {method} {path}"
            );
        }
        assert_eq!(
            specification["paths"]["/ws"]["get"]["security"][1]["ws_query_token"],
            serde_json::json!([]),
            "WebSocket docs must expose the browser query-token fallback"
        );
        assert_eq!(
            specification["components"]["securitySchemes"]["ws_query_token"]["name"],
            "token"
        );

        assert!(
            specification["paths"]
                .get("/api/data-processing/tasks")
                .is_none(),
            "conditional routes must not appear in the base document"
        );
        assert_eq!(
            common::openapi_operation_count(&specification),
            35,
            "Router/OpenAPI operation drift"
        );
    }

    #[test]
    fn gateway_openapi_matches_wire_envelopes_and_content_types() {
        let specification = json(ApiDoc::openapi());

        for (path, method) in [
            ("/api/auth/login", "post"),
            ("/api/auth/refresh", "post"),
            ("/api/auth/me", "get"),
            ("/api/homepage/{id}", "get"),
            ("/api/homepage/{id}", "put"),
            ("/api/homepage", "get"),
            ("/api/homepage/reset", "post"),
            ("/api/network", "get"),
            ("/api/broadcast", "post"),
            ("/api/broadcast/status", "get"),
            ("/api/config/check", "get"),
        ] {
            let schema = &specification["paths"][path][method]["responses"]["200"]["content"]["application/json"]
                ["schema"];
            assert!(
                schema.to_string().contains("GatewayDataResponse"),
                "{method} {path} must document the gateway data envelope: {schema}"
            );
        }

        for (path, method) in [("/api/auth/me", "put"), ("/api/auth/users/{id}", "put")] {
            let schema = &specification["paths"][path][method]["responses"]["200"]["content"]["application/json"]
                ["schema"];
            let schema = schema.to_string();
            assert!(
                schema.contains("GatewayDataResponse") && schema.contains("UserWithRole"),
                "{method} {path} must document the canonical profile response"
            );
        }

        assert!(
            specification["paths"]["/api/auth/logout"]["post"]["security"].is_null(),
            "logout authenticates with the refresh token body, not Bearer auth"
        );
        assert!(
            specification["paths"]["/"]["get"]["responses"]["200"]["content"]["text/plain"]
                .is_object()
        );
        assert!(
            specification["paths"]["/health"]["get"]["responses"]["200"]["content"]["text/plain"]
                .is_object()
        );
        assert!(specification["paths"]["/api/config/export"]["get"]["responses"]["200"]
            ["content"]["application/zip"]
            .is_object());
        assert_eq!(
            specification["components"]["schemas"]["ConfigArchive"]["type"],
            "string"
        );
        assert_eq!(
            specification["components"]["schemas"]["ConfigArchive"]["format"],
            "binary"
        );
    }

    #[test]
    fn homepage_openapi_is_industry_neutral_and_documents_safe_empty_reset() {
        let specification = json(ApiDoc::openapi());
        let list_operation = specification["paths"]["/api/homepage"]["get"]
            .to_string()
            .to_lowercase();

        for energy_term in ["soc", "plant", "grid"] {
            assert!(
                !list_operation.contains(energy_term),
                "homepage OpenAPI must not publish the Energy Pack term {energy_term:?}"
            );
        }

        let reset_operation = &specification["paths"]["/api/homepage/reset"]["post"];
        assert!(
            reset_operation["description"]
                .as_str()
                .expect("reset description")
                .contains("safe empty state")
        );
        assert_eq!(
            reset_operation["responses"]["200"]["description"],
            "Homepage points cleared to the safe empty state"
        );
        let reset_properties =
            &specification["components"]["schemas"]["HomepageResetData"]["properties"];
        assert!(reset_properties["remaining_count"].is_object());
        assert!(reset_properties.get("imported_count").is_none());
    }

    #[test]
    fn gateway_openapi_documents_fail_closed_management_boundaries() {
        let specification = json(ApiDoc::openapi());

        let registration = &specification["paths"]["/api/auth/register"]["post"];
        assert!(registration["security"].is_null());
        assert!(registration["responses"]["403"].is_object());

        for (path, method) in [("/api/network", "put"), ("/api/network/apply", "post")] {
            let operation = &specification["paths"][path][method];
            assert_eq!(
                operation["security"][0]["bearer_auth"],
                serde_json::json!([]),
                "missing Bearer security on {method} {path}"
            );
            for status in ["401", "403", "501"] {
                assert!(
                    operation["responses"][status].is_object(),
                    "{method} {path} must document HTTP {status}"
                );
            }
        }
    }

    #[test]
    fn gateway_openapi_documents_every_admin_read_boundary() {
        let specification = json(ApiDoc::openapi());

        for (path, method) in [
            ("/api/config/check", "get"),
            ("/api/config/export", "get"),
            ("/api/auth/users", "get"),
            ("/api/auth/users/{id}", "get"),
            ("/api/auth/users/{id}", "put"),
            ("/api/auth/users/{id}", "delete"),
        ] {
            let operation = &specification["paths"][path][method];
            assert_eq!(
                operation["security"][0]["bearer_auth"],
                serde_json::json!([]),
                "missing Bearer security on {method} {path}"
            );
            for status in ["401", "403"] {
                assert!(
                    operation["responses"][status].is_object(),
                    "{method} {path} must document HTTP {status}"
                );
            }
        }
    }

    #[test]
    fn commissioned_data_processing_document_adds_only_conditional_routes() {
        let specification = json(ApiDoc::openapi().nest("", DataProcessingApiDoc::openapi()));

        for (path, method) in [
            ("/api/data-processing/tasks", "get"),
            ("/api/data-processing/processors/health", "get"),
            ("/api/data-processing/process", "post"),
        ] {
            assert!(
                specification["paths"][path][method].is_object(),
                "missing commissioned {method} {path}"
            );
            assert_eq!(
                specification["paths"][path][method]["security"][0]["bearer_auth"],
                serde_json::json!([]),
                "missing Bearer security on commissioned {method} {path}"
            );
        }
        let process = &specification["paths"]["/api/data-processing/process"]["post"];
        for status in [
            "400", "401", "403", "404", "413", "415", "422", "428", "500", "502", "503", "504",
        ] {
            assert!(
                process["responses"][status].is_object(),
                "data-processing command must document HTTP {status}"
            );
        }
        assert!(
            process["responses"]["404"]["description"]
                .as_str()
                .is_some_and(|description| description.contains("commissioned resource"))
        );
        assert_eq!(
            common::openapi_operation_count(&specification),
            38,
            "commissioned Router/OpenAPI drift"
        );
    }
}
