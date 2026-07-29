//! API Routes Registration Module
//!
//! This module handles route registration and global definitions for the Communication Service REST API.
//! All handler implementations are in separate handler modules.

use axum::{
    Router,
    extract::DefaultBodyLimit,
    routing::{get, post},
};
use chrono::{DateTime, Utc};
use std::sync::{Arc, OnceLock};
use utoipa::OpenApi;

use aether_application::{ChannelManagementApplication, ChannelReconciliationApplication};
use aether_auth_jwt::AccessTokenAuthenticator;

use crate::core::channels::ChannelManager;

use crate::api::{
    handlers::health::*,
    handlers::{
        channel_handlers::*, channel_management_handlers::*, mapping_handlers::*, point_handlers::*,
    },
};

/// Global service start time storage
static SERVICE_START_TIME: OnceLock<DateTime<Utc>> = OnceLock::new();

/// Set the service start time (should be called once at startup)
pub fn set_service_start_time(start_time: DateTime<Utc>) {
    let _ = SERVICE_START_TIME.set(start_time);
}

/// Get the service start time
pub fn get_service_start_time() -> DateTime<Utc> {
    *SERVICE_START_TIME.get().unwrap_or(&Utc::now())
}

/// Application state containing the channel manager
///
/// # Lock-free Architecture
/// - `channel_manager` is now `Arc<ChannelManager>` without RwLock
/// - ChannelManager internally uses `arc-swap` for O(1) lock-free access
/// - Read latency: ~5ns (was ~50μs with RwLock)
///
/// Live point reads use the same authoritative SHM layout as acquisition.
pub struct AppState {
    /// Channel manager with O(1) lock-free access
    pub channel_manager: Arc<ChannelManager>,
    pub sqlite_pool: sqlx::SqlitePool,
}

impl Clone for AppState {
    fn clone(&self) -> Self {
        Self {
            channel_manager: self.channel_manager.clone(),
            sqlite_pool: self.sqlite_pool.clone(),
        }
    }
}

impl AppState {
    /// Create AppState with the channel manager and SQLite configuration pool.
    pub fn new(channel_manager: Arc<ChannelManager>, sqlite_pool: sqlx::SqlitePool) -> Self {
        Self {
            channel_manager,
            sqlite_pool,
        }
    }
}

#[derive(OpenApi)]
#[openapi(
    paths(
        // Health
        crate::api::handlers::health::health_check,

        // Channel queries and status
        crate::api::handlers::channel_handlers::get_all_channels,
        crate::api::handlers::channel_handlers::get_channel_detail_handler,
        crate::api::handlers::channel_handlers::get_channel_status,

        // Point information
        crate::api::handlers::point_handlers::get_point_info_handler,
        crate::api::handlers::point_handlers::get_channel_points_handler,
        crate::api::handlers::point_handlers::get_unmapped_points_handler,
        crate::api::handlers::point_handlers::get_point_mapping_with_type_handler,

        // Channel management (CRUD)
        crate::api::handlers::channel_management_handlers::create_channel_handler,
        crate::api::handlers::channel_management_handlers::update_channel_handler,
        crate::api::handlers::channel_management_handlers::set_channel_enabled_handler,
        crate::api::handlers::channel_management_handlers::delete_channel_handler,
        crate::api::handlers::channel_management_handlers::reconcile_channels_handler,
        crate::api::handlers::channel_management_handlers::reconcile_channel_handler,

        // Mapping query
        crate::api::handlers::mapping_handlers::get_channel_mappings_handler,

    ),
    components(
        schemas(
            crate::dto::ChannelStatusResponse,
            crate::dto::ChannelStatusDto,
            crate::dto::ChannelDetail,
            crate::dto::ChannelRuntimeStatus,
            crate::dto::PointCounts,
            crate::dto::ChannelListQuery,
            crate::dto::PaginatedResponse<crate::dto::ChannelStatusResponse>,
            crate::dto::ChannelCreateRequest,
            crate::dto::ChannelConfigUpdateRequest,
            crate::dto::ChannelEnabledRequest,
            crate::dto::ChannelMutationOperation,
            crate::dto::ChannelRuntimeProjectionResult,
            crate::dto::ChannelCompletionAuditState,
            crate::dto::ChannelCompletionAudit,
            crate::dto::ChannelMutationResult,
            crate::dto::ChannelMutationResponse,
            crate::dto::ChannelReconciliationScopeResult,
            crate::dto::ChannelDesiredStateResult,
            crate::dto::ChannelReconciliationItemResult,
            crate::dto::ChannelReconciliationResult,
            crate::dto::ChannelReconciliationResponse,
            common::ErrorInfo,
            common::ErrorResponse,
            crate::dto::PointDefinition,
            crate::dto::GroupedPoints,
            crate::dto::GroupedMappings,
            crate::dto::PointMappingDetail
        )
    ),
    tags(
        (name = "io", description = "Device protocol and field I/O API")
    ),
    modifiers(&SecurityAddon),
    info(
        title = "Aether I/O Service API",
        version = env!("CARGO_PKG_VERSION"),
        description = "Internal loopback API for device protocols, channels, point mappings, and commissioning. Do not expose this service port remotely; use an authenticated ingress or an on-device commissioning workflow."
    )
)]
pub struct IoApiDoc;

struct SecurityAddon;

impl utoipa::Modify for SecurityAddon {
    fn modify(&self, openapi: &mut utoipa::openapi::OpenApi) {
        if let Some(components) = openapi.components.as_mut() {
            components.add_security_scheme(
                "bearer_auth",
                utoipa::openapi::security::SecurityScheme::Http(
                    utoipa::openapi::security::HttpBuilder::new()
                        .scheme(utoipa::openapi::security::HttpAuthScheme::Bearer)
                        .bearer_format("JWT")
                        .description(Some("Signed Aether access token"))
                        .build(),
                ),
            );
        }
    }
}

#[cfg(feature = "openapi")]
async fn openapi_document() -> axum::Json<utoipa::openapi::OpenApi> {
    axum::Json(IoApiDoc::openapi())
}

/// Create the production API router with governed channel desired-state and
/// runtime-reconciliation application commands explicitly composed by the
/// service binary.
pub fn create_api_routes_with_channel_applications(
    channel_manager: Arc<ChannelManager>,
    sqlite_pool: sqlx::SqlitePool,
    channel_management: Arc<ChannelManagementApplication>,
    channel_reconciliation: Arc<ChannelReconciliationApplication>,
    access_authenticator: Arc<AccessTokenAuthenticator>,
) -> Router {
    create_api_routes_with_boundary(
        channel_manager,
        sqlite_pool,
        ChannelManagementHttpBoundary::governed_with_reconciliation(
            channel_management,
            channel_reconciliation,
            access_authenticator,
        ),
    )
}

fn create_api_routes_with_boundary(
    channel_manager: Arc<ChannelManager>,
    sqlite_pool: sqlx::SqlitePool,
    channel_management: ChannelManagementHttpBoundary,
) -> Router {
    let state = AppState::new(channel_manager, sqlite_pool);

    let router = Router::new()
        // Health check (top-level for monitoring systems)
        .route("/health", get(health_check))
        // Channel management (CRUD)
        .route("/api/channels", get(get_all_channels).post(create_channel_handler))
        .route("/api/channels/reconcile", post(reconcile_channels_handler))
        .route("/api/channels/{id}/reconcile", post(reconcile_channel_handler))
        .route("/api/channels/{id}", get(get_channel_detail_handler).put(update_channel_handler).delete(delete_channel_handler))
        .route("/api/channels/{id}/status", get(get_channel_status))
        .route("/api/channels/{id}/enabled", axum::routing::put(set_channel_enabled_handler))
        .route("/api/channels/{id}/points", get(get_channel_points_handler))
        .route("/api/channels/{id}/unmapped-points", get(get_unmapped_points_handler))
        .route("/api/channels/{id}/mappings", get(get_channel_mappings_handler))
        .route("/api/channels/{channel_id}/{type}/points/{point_id}/mapping", get(get_point_mapping_with_type_handler))
        .route(
            "/api/channels/{channel_id}/{telemetry_type}/{point_id}",
            get(get_point_info_handler),
        )
        .layer(axum::Extension(channel_management));
    #[cfg(feature = "openapi")]
    let router = router.route("/openapi.json", get(openapi_document));
    router
        // CRITICAL: Apply middleware BEFORE .with_state() for it to work
        .layer(axum::middleware::from_fn(common::logging::http_request_logger))
        .layer(DefaultBodyLimit::max(1024 * 1024)) // 1 MB request body limit
        .with_state(state)
}

#[cfg(test)]
#[path = "routes_tests.rs"]
mod tests;
