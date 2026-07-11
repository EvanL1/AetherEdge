//! `aether-api` — management API and WebSocket service.
//!
//! Unified entry for the AetherEMS front-end:
//! - JWT auth (users, roles)
//! - WebSocket real-time data push with subscriptions
//! - POST /broadcast – push any JSON to all WebSocket clients
//! - GET /api/v1/homepage – calculated points CRUD
//! - GET /api/v1/network – read-only systemd-networkd view; remote writes disabled
//! - GET /api/v1/config – admin-only config checks/export; remote mutation is disabled

use std::net::SocketAddr;
use std::sync::Arc;

use axum::{
    Router,
    extract::{DefaultBodyLimit, Query, State, WebSocketUpgrade},
    response::IntoResponse,
    routing::{delete, get, post, put},
};
use dashmap::DashMap;
use md5::{Digest, Md5};
use serde::Deserialize;
use tokio_util::sync::CancellationToken;
use tower_http::cors::{Any, CorsLayer};
use tracing::info;
use utoipa::OpenApi;
#[cfg(feature = "swagger-ui")]
use utoipa_swagger_ui::{Config, SwaggerUi};

mod auth;
mod config;
mod db;
mod live_values;
mod middleware_auth;
mod models;
mod routes_auth;
mod routes_broadcast;
mod routes_config;
mod routes_homepage;
mod routes_network;
mod state;
#[cfg(test)]
mod test_support;
mod ws;

use crate::config::GatewayConfig;
use crate::live_values::build_gateway_value_source;
use crate::state::AppState;
use crate::ws::WsHub;

const BOOTSTRAP_ADMIN_PASSWORD_ENV: &str = "AETHER_BOOTSTRAP_ADMIN_PASSWORD";
const MIN_BOOTSTRAP_ADMIN_PASSWORD_CHARS: usize = 16;

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
// ApiDoc / SecurityAddon are only consumed by the Swagger UI merge in
// `build_router`, which is feature-gated. Silence dead_code when the feature
// is off rather than cfg-gating the struct (avoids a cascade of unused
// imports from the `components(schemas(...))` list).

#[cfg_attr(not(feature = "swagger-ui"), allow(dead_code))]
#[derive(OpenApi)]
#[openapi(
    paths(
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
        routes_config::import_config,
        routes_config::restart_services,
        routes_config::start_upgrade,
        routes_config::abort_upgrade,
        routes_config::upgrade_status,
    ),
    components(schemas(
        models::UserCreate,
        models::UserLogin,
        models::UserUpdate,
        models::PasswordChange,
        models::RefreshTokenRequest,
        models::TokenResponse,
        models::Role,
        models::RoleInfo,
        models::UserWithRole,
        models::CalculatedPoint,
        models::CalculatedPointUpdate,
        models::NetworkConfig,
    )),
    tags(
        (name = "Auth", description = "Authentication and user management"),
        (name = "Homepage", description = "Homepage calculated point CRUD"),
        (name = "Network", description = "Read-only network interface inspection; remote mutation is disabled"),
        (name = "Config", description = "System configuration export / import / upgrade"),
        (name = "WebSocket", description = "WebSocket broadcast and status"),
    ),
    modifiers(&SecurityAddon),
    info(title = "AetherEMS API Gateway", version = "1.0.0", description = "AetherEMS unified API gateway")
)]
struct ApiDoc;

#[cfg_attr(not(feature = "swagger-ui"), allow(dead_code))]
struct SecurityAddon;
#[cfg_attr(not(feature = "swagger-ui"), allow(dead_code))]
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
        }
    }
}

// ── WebSocket endpoint ────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct WsParams {
    client_id: Option<String>,
    data_type: Option<String>,
}

async fn ws_handler(
    ws: WebSocketUpgrade,
    Query(params): Query<WsParams>,
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    let client_id = params
        .client_id
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let data_type = params.data_type.unwrap_or_else(|| "general".to_string());
    let hub = Arc::clone(&state.ws_hub);

    ws.on_upgrade(move |socket| ws::handle_socket(socket, client_id, data_type, hub))
}

// ── Router ────────────────────────────────────────────────────────────────────

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
        .route("/export", get(routes_config::export_config))
        .route(
            "/import",
            post(routes_config::import_config).layer(DefaultBodyLimit::max(64 * 1024 * 1024)), // 64 MB for config ZIP
        )
        .route("/restart-services", post(routes_config::restart_services))
        .route(
            "/upgrade",
            post(routes_config::start_upgrade).layer(DefaultBodyLimit::max(1024 * 1024 * 1024)), // 1024 MB for firmware
        )
        .route("/upgrade/abort", post(routes_config::abort_upgrade))
        .route("/upgrade/status", get(routes_config::upgrade_status));

    // Routes that require auth. Layered ONCE on the merged router so
    // adding a new sub-router (e.g. /reports) cannot accidentally skip
    // the JWT check the way per-route layering did before this fix.
    // Includes anything that mutates state, exposes admin operations,
    // or pushes data to other clients (broadcast). /auth is the only
    // public surface (register/login/refresh) and is mounted below
    // without the layer.
    let protected_v1 = Router::new()
        .route("/broadcast", post(routes_broadcast::broadcast_message))
        .route("/broadcast/status", get(routes_broadcast::broadcast_status))
        .nest("/homepage", homepage_routes)
        .nest("/network", network_routes)
        .nest("/config", config_routes)
        .layer(axum::middleware::from_fn_with_state(
            Arc::clone(&state),
            middleware_auth::require_jwt,
        ));

    let api_v1 = Router::new().merge(protected_v1).nest("/auth", auth_routes);

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
        .route("/", get(|| async { "AetherEMS API Gateway" }))
        .route("/health", get(|| async { "ok" }))
        .route(
            "/ws",
            get(ws_handler).route_layer(axum::middleware::from_fn_with_state(
                Arc::clone(&state),
                middleware_auth::require_jwt,
            )),
        )
        .nest("/api/v1", api_v1)
        .nest("/api/admin", admin_routes)
        .with_state(state)
        .layer(DefaultBodyLimit::max(1024 * 1024))
        .layer(axum::middleware::from_fn(
            common::logging::http_request_logger,
        ))
        .layer(cors);

    #[cfg(feature = "swagger-ui")]
    let app = app.merge(
        SwaggerUi::new("/docs")
            .url("/openapi.json", ApiDoc::openapi())
            .config(
                Config::default()
                    .default_model_rendering("model")
                    .default_models_expand_depth(1),
            ),
    );

    app
}

// ── Main ──────────────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cfg = GatewayConfig::from_env()?;

    // ── Logging ───────────────────────────────────────────────────────────────
    let service_info = common::service_bootstrap::ServiceInfo::new(
        "aether-api",
        "API Gateway service",
        cfg.api_port,
    );
    common::service_bootstrap::init_logging(&service_info, None)
        .map_err(|e| anyhow::anyhow!("Failed to init logging: {}", e))?;
    common::logging::enable_sighup_log_reopen();
    common::service_bootstrap::print_startup_banner(&service_info);

    info!("aether-api starting on port {}", cfg.api_port);
    info!("SHM:   {}", cfg.shm_path);
    info!("Health SHM: {}", cfg.channel_health_shm_path);
    info!("PointWatch: {}", cfg.point_watch_socket);
    info!("DB:    {}", cfg.db_path);

    // Reconcile upgrade status: if a previous upgrade was interrupted by a
    // container restart, fix the stale "running" status in the status file.
    routes_config::reconcile_upgrade_status_on_startup();

    // ── SQLite ────────────────────────────────────────────────────────────────
    let db_dir = std::path::Path::new(&cfg.db_path)
        .parent()
        .unwrap_or(std::path::Path::new("."));
    std::fs::create_dir_all(db_dir)?;

    let db_pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(5)
        .connect_with(common::bootstrap_database::sqlite_connect_options(
            &cfg.db_path,
        ))
        .await
        .map_err(|e| anyhow::anyhow!("SQLite connect failed: {} path={}", e, cfg.db_path))?;

    db::create_tables(&db_pool).await?;
    db::init_roles(&db_pool).await?;
    db::init_calculated_points(&db_pool).await?;

    // ── Bootstrap admin user ──────────────────────────────────────────────────
    ensure_bootstrap_admin(&db_pool, || {
        std::env::var(BOOTSTRAP_ADMIN_PASSWORD_ENV).ok()
    })
    .await?;

    // ── App State ─────────────────────────────────────────────────────────────
    let live_values = build_gateway_value_source(&db_pool, &cfg).await?;
    let ws_hub = WsHub::new(live_values, db_pool.clone());

    let state = Arc::new(AppState {
        db: db_pool,
        config: Arc::new(cfg),
        ws_hub: Arc::clone(&ws_hub),
        refresh_tokens: DashMap::new(),
    });

    // ── Background tasks ──────────────────────────────────────────────────────
    let shutdown = CancellationToken::new();

    let hb_hub = Arc::clone(&ws_hub);
    let hb_shutdown = shutdown.clone();
    tokio::spawn(async move {
        ws::run_heartbeat(hb_hub, hb_shutdown).await;
    });

    let push_hub = Arc::clone(&ws_hub);
    let push_shutdown = shutdown.clone();
    let push_interval = state.config.data_fetch_interval_secs;
    let push_shm_path = state.config.shm_path.clone();
    let push_socket = state.config.point_watch_socket.clone();
    let push_debounce_ms = state.config.point_watch_debounce_ms;
    tokio::spawn(async move {
        ws::run_data_push(
            push_hub,
            push_shutdown,
            push_interval,
            &push_shm_path,
            &push_socket,
            push_debounce_ms,
        )
        .await;
    });

    // ── HTTP server ───────────────────────────────────────────────────────────
    let app = build_router(Arc::clone(&state));

    let bind_addr: SocketAddr = format!("{}:{}", state.config.api_host, state.config.api_port)
        .parse()
        .map_err(|e| anyhow::anyhow!("Invalid bind address: {}", e))?;

    let socket = tokio::net::TcpSocket::new_v4()?;
    socket.set_reuseaddr(true)?;
    socket.bind(bind_addr)?;
    let listener = socket.listen(1024)?;

    info!("Listening on {}", bind_addr);

    axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            common::shutdown::wait_for_shutdown().await;
            info!("Shutdown signal received");
            shutdown.cancel();
        })
        .await?;

    common::logging::shutdown_logging_tasks().await;
    info!("api stopped");
    Ok(())
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
