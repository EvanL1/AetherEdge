//! `aether-api` — authenticated remote application boundary.
//!
//! The gateway owns JWT users and roles and proxies only fixed application
//! service routes. Browser dashboards, WebSockets, host networking, archives,
//! upgrades, and process supervision are deployment concerns.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::{
    Router,
    extract::DefaultBodyLimit,
    routing::{delete, get, post, put},
};
use dashmap::DashMap;
use md5::{Digest, Md5};
use tower_http::cors::{Any, CorsLayer};
use tracing::info;
#[cfg(feature = "openapi")]
use utoipa::OpenApi;
#[cfg(feature = "swagger-ui")]
use utoipa_swagger_ui::{Config, SwaggerUi, Url};

mod auth;
mod config;
mod db;
mod middleware_auth;
mod models;
#[cfg(feature = "swagger-ui")]
mod openapi_gateway;
mod routes_auth;
mod service_gateway;
mod state;
#[cfg(test)]
mod test_support;

use crate::config::GatewayConfig;
use crate::state::AppState;

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
// ApiDoc / SecurityAddon are compiled only with the Swagger UI so shared
// admin annotations can remain opt-in through `common/openapi`.

#[cfg(feature = "openapi")]
#[derive(OpenApi)]
#[openapi(
    paths(
        service_info,
        health_check,
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
        models::GatewayMessageResponse,
        models::RegistrationResult,
        models::RoleListResponse,
        models::UserListData,
        models::DeletedUserData,
        models::AuthStatsData,
        models::UserUpdateSuccess,
        models::Role,
        models::RoleInfo,
        models::UserWithRole,
        common::admin_api::SetLogLevelRequest,
        common::admin_api::LogLevelResponse,
    )),
    tags(
        (name = "Auth", description = "Authentication and user management"),
        (name = "Meta", description = "Service metadata and health"),
        (name = "admin", description = "Authenticated runtime administration"),
    ),
    modifiers(&SecurityAddon),
    info(
        title = "Aether API Gateway",
        version = env!("CARGO_PKG_VERSION"),
        description = "Authenticated application gateway. Protected operations require a Bearer JWT; service-local APIs remain intra-host only. Optional Swagger routes must be exposed only on a trusted commissioning network."
    )
)]
struct ApiDoc;

#[cfg(feature = "openapi")]
struct SecurityAddon;
#[cfg(feature = "openapi")]
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

// ── Router ────────────────────────────────────────────────────────────────────

#[cfg(feature = "swagger-ui")]
async fn gateway_openapi_document() -> axum::Json<utoipa::openapi::OpenApi> {
    axum::Json(ApiDoc::openapi())
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

    // Routes that require auth. Layered ONCE on the merged router so
    // adding a new sub-router (e.g. /reports) cannot accidentally skip
    // the JWT check the way per-route layering did before this fix.
    // `/auth` owns its documented public bootstrap/session routes. Every
    // application-service gateway route is protected here.
    let protected_v1 = Router::new().merge(service_gateway::router());
    let protected_v1 = protected_v1.layer(axum::middleware::from_fn_with_state(
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
        .route("/", get(service_info))
        .route("/health", get(health_check))
        .nest("/api/v1", api_v1)
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
    info!("DB:    {}", cfg.db_path);

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

    // ── Bootstrap admin user ──────────────────────────────────────────────────
    ensure_bootstrap_admin(&db_pool, || {
        std::env::var(BOOTSTRAP_ADMIN_PASSWORD_ENV).ok()
    })
    .await?;

    // ── App State ─────────────────────────────────────────────────────────────
    let service_client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(
            cfg.service_request_timeout_secs,
        ))
        .build()?;

    let state = Arc::new(AppState {
        db: db_pool,
        config: Arc::new(cfg),
        refresh_tokens: DashMap::new(),
        service_client,
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
        .with_graceful_shutdown(async {
            common::shutdown::wait_for_shutdown().await;
            info!("Shutdown signal received");
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

#[cfg(test)]
mod service_gateway_route_tests {
    use axum::{
        body::{Body, to_bytes},
        http::{Request, StatusCode},
    };
    use tower::ServiceExt;

    use crate::test_support::{app_state, authorization_headers};

    use super::build_router;

    #[tokio::test]
    async fn internal_application_gateway_is_mounted_only_behind_jwt_authentication() {
        let state = app_state().await;
        let app = build_router(state);

        let unauthenticated = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/io/health")
                    .body(Body::empty())
                    .expect("valid unauthenticated request"),
            )
            .await
            .expect("gateway response");
        assert_eq!(unauthenticated.status(), StatusCode::UNAUTHORIZED);

        let mut authenticated_request = Request::builder()
            .uri("/api/v1/io/health")
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
    async fn application_gateway_mutations_are_role_and_confirmation_guarded() {
        let app = build_router(app_state().await);

        let mut viewer_request = Request::builder()
            .method("POST")
            .uri("/api/v1/uplink/mqtt/disconnect")
            .header("x-aether-confirmed", "true")
            .body(Body::empty())
            .expect("valid viewer request");
        viewer_request
            .headers_mut()
            .extend(authorization_headers("Viewer"));
        let viewer_response = app
            .clone()
            .oneshot(viewer_request)
            .await
            .expect("viewer gateway response");
        assert_eq!(viewer_response.status(), StatusCode::FORBIDDEN);

        let mut unconfirmed_request = Request::builder()
            .method("POST")
            .uri("/api/v1/uplink/mqtt/disconnect")
            .body(Body::empty())
            .expect("valid unconfirmed request");
        unconfirmed_request
            .headers_mut()
            .extend(authorization_headers("Engineer"));
        let unconfirmed_response = app
            .oneshot(unconfirmed_request)
            .await
            .expect("unconfirmed gateway response");
        assert_eq!(
            unconfirmed_response.status(),
            StatusCode::PRECONDITION_REQUIRED
        );
    }
}

#[cfg(all(test, feature = "openapi"))]
mod openapi_tests {
    use super::*;

    fn document() -> serde_json::Value {
        serde_json::to_value(ApiDoc::openapi()).expect("serialize OpenAPI document")
    }

    #[test]
    fn gateway_openapi_matches_the_headless_application_boundary() {
        let specification = document();
        assert_eq!(specification["info"]["title"], "Aether API Gateway");

        for (path, method) in [
            ("/", "get"),
            ("/health", "get"),
            ("/api/v1/auth/validate", "get"),
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

        for retired in [
            "/ws",
            "/api/v1/homepage",
            "/api/v1/network",
            "/api/v1/config",
        ] {
            assert!(specification["paths"].get(retired).is_none());
        }
        assert!(
            specification["components"]["securitySchemes"]
                .get("ws_query_token")
                .is_none()
        );
    }

    #[test]
    fn protected_reads_and_admin_routes_require_bearer_authentication() {
        let specification = document();
        for (path, method) in [
            ("/api/v1/auth/validate", "get"),
            ("/api/v1/auth/users", "get"),
            ("/api/v1/auth/users/{id}", "get"),
            ("/api/admin/logs/level", "get"),
            ("/api/admin/logs/files", "get"),
            ("/api/admin/logs/view", "get"),
        ] {
            assert_eq!(
                specification["paths"][path][method]["security"][0]["bearer_auth"],
                serde_json::json!([]),
                "missing Bearer security on {method} {path}"
            );
        }
    }

    #[test]
    fn service_probes_and_auth_envelopes_match_the_wire() {
        let specification = document();
        assert!(
            specification["paths"]["/"]["get"]["responses"]["200"]["content"]["text/plain"]
                .is_object()
        );
        assert!(
            specification["paths"]["/health"]["get"]["responses"]["200"]["content"]["text/plain"]
                .is_object()
        );
        for (path, method) in [
            ("/api/v1/auth/login", "post"),
            ("/api/v1/auth/refresh", "post"),
            ("/api/v1/auth/me", "get"),
        ] {
            let schema = &specification["paths"][path][method]["responses"]["200"]["content"]["application/json"]
                ["schema"];
            assert!(schema.to_string().contains("GatewayDataResponse"));
        }
    }
}
