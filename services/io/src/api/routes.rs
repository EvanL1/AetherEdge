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

use crate::api::command_cache::CommandTxCache;
use crate::core::channels::ChannelManager;

// Import handler modules
use crate::api::{
    handlers::health::*,
    handlers::{
        channel_handlers::*, channel_management_handlers::*, control_handlers::*,
        mapping_handlers::*, network_handlers::*, point_handlers::*, protocol_handlers::*,
        provision_handlers::*, template_handlers::*,
    },
};
use common::admin_api::{get_log_level, list_log_files, set_log_level, view_log_file};

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
    /// Command TX cache for O(1) hot path access
    /// Bypasses ChannelManager RwLock for Control/Adjustment writes
    pub command_tx_cache: Arc<CommandTxCache>,
    /// Explicit development-only gate for authoritative T/S simulation writes.
    pub allow_simulation_writes: bool,
    /// Internal runtime convergence facade shared with the governed HTTP boundary.
    pub channel_reconciliation: Option<Arc<ChannelReconciliationApplication>>,
}

impl Clone for AppState {
    fn clone(&self) -> Self {
        Self {
            channel_manager: self.channel_manager.clone(),
            sqlite_pool: self.sqlite_pool.clone(),
            command_tx_cache: self.command_tx_cache.clone(),
            allow_simulation_writes: self.allow_simulation_writes,
            channel_reconciliation: self.channel_reconciliation.clone(),
        }
    }
}

impl AppState {
    /// Create AppState with the channel manager and SQLite configuration pool.
    pub fn new(
        channel_manager: Arc<ChannelManager>,
        sqlite_pool: sqlx::SqlitePool,
        command_tx_cache: Arc<CommandTxCache>,
        allow_simulation_writes: bool,
        channel_reconciliation: Option<Arc<ChannelReconciliationApplication>>,
    ) -> Self {
        Self {
            channel_manager,
            sqlite_pool,
            command_tx_cache,
            allow_simulation_writes,
            channel_reconciliation,
        }
    }
}

pub type ProductionAppState = AppState;

#[derive(OpenApi)]
#[openapi(
    paths(
        // Health and service status
        crate::api::handlers::health::get_service_status,
        crate::api::handlers::health::health_check,

        // Protocol discovery
        crate::api::handlers::protocol_handlers::list_protocols,

        // Channel queries and status
        crate::api::handlers::channel_handlers::get_all_channels,
        crate::api::handlers::channel_handlers::list_channels,
        crate::api::handlers::channel_handlers::search_channels,
        crate::api::handlers::channel_handlers::get_channel_detail_handler,
        crate::api::handlers::channel_handlers::get_channel_status,
        crate::api::handlers::channel_handlers::list_all_points,

        // Control operations
        crate::api::handlers::control_handlers::control_channel,
        crate::api::handlers::control_handlers::write_channel_point,  // Unified write endpoint (supports single & batch)
        crate::api::handlers::control_handlers::set_channel_log_level,

        // Point information
        crate::api::handlers::point_handlers::get_point_info_handler,
        crate::api::handlers::point_handlers::get_channel_points_handler,
        crate::api::handlers::point_handlers::get_unmapped_points_handler,
        crate::api::handlers::point_handlers::get_point_mapping_with_type_handler,

        // Point CRUD operations (using parameterized inner handlers for OpenAPI docs)
        crate::api::handlers::point_handlers::create_telemetry_point_handler,
        crate::api::handlers::point_handlers::create_signal_point_handler,
        crate::api::handlers::point_handlers::create_control_point_handler,
        crate::api::handlers::point_handlers::create_adjustment_point_handler,
        crate::api::handlers::point_handlers::get_telemetry_point_config_handler,
        crate::api::handlers::point_handlers::get_signal_point_config_handler,
        crate::api::handlers::point_handlers::get_control_point_config_handler,
        crate::api::handlers::point_handlers::get_adjustment_point_config_handler,
        crate::api::handlers::point_handlers::update_telemetry_point_handler,
        crate::api::handlers::point_handlers::update_signal_point_handler,
        crate::api::handlers::point_handlers::update_control_point_handler,
        crate::api::handlers::point_handlers::update_adjustment_point_handler,
        crate::api::handlers::point_handlers::delete_telemetry_point_handler,
        crate::api::handlers::point_handlers::delete_signal_point_handler,
        crate::api::handlers::point_handlers::delete_control_point_handler,
        crate::api::handlers::point_handlers::delete_adjustment_point_handler,
        crate::api::handlers::point_handlers::batch_point_operations_handler,

        // Channel management (CRUD)
        crate::api::handlers::channel_management_handlers::create_channel_handler,
        crate::api::handlers::channel_management_handlers::update_channel_handler,
        crate::api::handlers::channel_management_handlers::set_channel_enabled_handler,
        crate::api::handlers::provision_handlers::provision_channel_handler,
        crate::api::handlers::channel_management_handlers::delete_channel_handler,
        crate::api::handlers::channel_management_handlers::reconcile_channels_handler,
        crate::api::handlers::channel_management_handlers::reconcile_channel_handler,
        crate::api::handlers::channel_management_handlers::reload_configuration_handler,
        crate::api::handlers::channel_management_handlers::reload_routing_handler,

        // Mapping management
        crate::api::handlers::mapping_handlers::get_channel_mappings_handler,
        crate::api::handlers::mapping_handlers::update_channel_mappings_handler,

        // Template management
        crate::api::handlers::template_handlers::list_templates,
        crate::api::handlers::template_handlers::get_template,
        crate::api::handlers::template_handlers::create_template,
        crate::api::handlers::template_handlers::create_template_from_channel,
        crate::api::handlers::template_handlers::update_template,
        crate::api::handlers::template_handlers::delete_template,
        crate::api::handlers::template_handlers::apply_template,

        // Admin endpoints
        common::admin_api::set_log_level,
        common::admin_api::get_log_level,
        common::admin_api::list_log_files,
        common::admin_api::view_log_file,

        // Network configuration endpoints
        crate::api::handlers::network_handlers::list_network_interfaces,
        crate::api::handlers::network_handlers::get_network_interface,
        crate::api::handlers::network_handlers::update_network_interface,
        crate::api::handlers::network_handlers::apply_network_changes
    ),
    components(
        schemas(
            crate::dto::ServiceStatus,
            crate::dto::ChannelStatusResponse,
            crate::dto::ChannelStatusDto,
            crate::dto::ChannelDetail,
            crate::dto::ChannelRuntimeStatus,
            crate::dto::PointCounts,
            crate::dto::ChannelListQuery,
            crate::dto::PaginatedResponse<crate::dto::ChannelStatusResponse>,
            crate::dto::ChannelOperation,
            crate::dto::ChannelOperationKind,
            crate::dto::ChannelControlOperationResult,
            crate::dto::ChannelControlResult,
            crate::dto::ChannelControlResponse,
            crate::dto::ControlRequest,
            crate::dto::AdjustmentRequest,
            crate::dto::ControlValueRequest,
            crate::dto::AdjustmentValueRequest,
            crate::dto::BatchControlRequest,
            crate::dto::BatchAdjustmentRequest,
            crate::dto::BatchCommandResult,
            crate::dto::BatchCommandError,
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
            crate::dto::RoutingReloadResult,
            crate::dto::PointDefinition,
            crate::dto::GroupedPoints,
            crate::dto::GroupedMappings,
            crate::dto::PointMappingDetail,
            crate::dto::PointMappingItem,
            crate::dto::MappingBatchUpdateRequest,
            crate::dto::MappingBatchUpdateResult,
            // Point CRUD DTOs
            crate::api::handlers::point_handlers::PointCrudResult,
            crate::api::handlers::point_handlers::PointUpdateRequest,
            // Batch Point CRUD DTOs
            crate::api::handlers::point_handlers::PointBatchRequest,
            crate::api::handlers::point_handlers::PointBatchResult,
            crate::api::handlers::point_handlers::PointBatchCreateItem,
            crate::api::handlers::point_handlers::PointBatchUpdateItem,
            crate::api::handlers::point_handlers::PointBatchDeleteItem,
            crate::api::handlers::point_handlers::OperationStats,
            crate::api::handlers::point_handlers::OperationStat,
            crate::api::handlers::point_handlers::PointBatchError,
            // Template schemas
            crate::dto::TemplateListItem,
            crate::dto::TemplateDetail,
            crate::dto::CreateTemplateReq,
            crate::dto::CreateTemplateFromChannelReq,
            crate::dto::UpdateTemplateReq,
            crate::dto::ApplyTemplateReq,
            crate::dto::TemplateListQuery,
            // Provision schemas
            crate::api::handlers::provision_handlers::ProvisionRequest,
            crate::api::handlers::provision_handlers::ProvisionResult,
            crate::api::handlers::provision_handlers::DiscoveredModelInfo,
            // Admin schemas
            common::admin_api::SetLogLevelRequest,
            common::admin_api::LogLevelResponse,
            // Network configuration schemas
            crate::api::handlers::network_handlers::NetworkInterfaceConfig,
            crate::api::handlers::network_handlers::NetworkInterfaceList,
            crate::api::handlers::network_handlers::NetworkConfigUpdateRequest,
            crate::api::handlers::network_handlers::NetworkConfigUpdateResult,
            crate::api::handlers::network_handlers::NetworkApplyResult
        )
    ),
    tags(
        (name = "io", description = "Device protocol and field I/O API"),
        (name = "templates", description = "Channel template management (snapshot & apply)"),
        (name = "admin", description = "Administration and service management"),
        (name = "network", description = "Network interface configuration")
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
        if let Some(operation) = openapi
            .paths
            .paths
            .get_mut("/api/channels/reload")
            .and_then(|path| path.post.as_mut())
        {
            operation.deprecated = Some(utoipa::openapi::Deprecated::True);
        }
    }
}

/// Create the API router over authoritative SHM and SQLite configuration.
pub fn create_api_routes(
    channel_manager: Arc<ChannelManager>,
    sqlite_pool: sqlx::SqlitePool,
    command_tx_cache: Arc<CommandTxCache>,
) -> Router {
    create_api_routes_with_boundary(
        channel_manager,
        sqlite_pool,
        command_tx_cache,
        simulation_writes_enabled(),
        None,
        ChannelManagementHttpBoundary::unavailable(),
        PointTopologyHttpBoundary::unavailable(),
    )
}

/// Create the production API router with the governed channel application
/// command explicitly composed by the service binary.
pub fn create_api_routes_with_channel_management(
    channel_manager: Arc<ChannelManager>,
    sqlite_pool: sqlx::SqlitePool,
    command_tx_cache: Arc<CommandTxCache>,
    channel_management: Arc<ChannelManagementApplication>,
    access_authenticator: Arc<AccessTokenAuthenticator>,
) -> Router {
    create_api_routes_with_boundary(
        channel_manager,
        sqlite_pool,
        command_tx_cache,
        simulation_writes_enabled(),
        None,
        ChannelManagementHttpBoundary::governed(channel_management, access_authenticator),
        PointTopologyHttpBoundary::unavailable(),
    )
}

/// Creates routes with only the governed point-topology command composed.
///
/// This narrow composition root is useful for embedded commissioning tests;
/// channel lifecycle mutations remain fail-closed.
pub fn create_api_routes_with_point_topology(
    channel_manager: Arc<ChannelManager>,
    sqlite_pool: sqlx::SqlitePool,
    command_tx_cache: Arc<CommandTxCache>,
    point_topology: Arc<crate::point_topology::PointTopologyApplication>,
    access_authenticator: Arc<AccessTokenAuthenticator>,
) -> Router {
    create_api_routes_with_boundary(
        channel_manager,
        sqlite_pool,
        command_tx_cache,
        simulation_writes_enabled(),
        None,
        ChannelManagementHttpBoundary::unavailable(),
        PointTopologyHttpBoundary::governed(point_topology, access_authenticator),
    )
}

/// Create the production API router with governed channel desired-state and
/// runtime-reconciliation application commands explicitly composed by the
/// service binary.
pub fn create_api_routes_with_channel_applications(
    channel_manager: Arc<ChannelManager>,
    sqlite_pool: sqlx::SqlitePool,
    command_tx_cache: Arc<CommandTxCache>,
    channel_management: Arc<ChannelManagementApplication>,
    channel_reconciliation: Arc<ChannelReconciliationApplication>,
    point_topology: Arc<crate::point_topology::PointTopologyApplication>,
    access_authenticator: Arc<AccessTokenAuthenticator>,
) -> Router {
    let state_reconciliation = Arc::clone(&channel_reconciliation);
    let point_authenticator = Arc::clone(&access_authenticator);
    create_api_routes_with_boundary(
        channel_manager,
        sqlite_pool,
        command_tx_cache,
        simulation_writes_enabled(),
        Some(state_reconciliation),
        ChannelManagementHttpBoundary::governed_with_reconciliation(
            channel_management,
            channel_reconciliation,
            access_authenticator,
        ),
        PointTopologyHttpBoundary::governed(point_topology, point_authenticator),
    )
}

fn simulation_writes_enabled() -> bool {
    std::env::var("AETHER_ALLOW_SIMULATION_WRITES")
        .ok()
        .is_some_and(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
}

#[cfg(test)]
fn create_api_routes_with_simulation_writes(
    channel_manager: Arc<ChannelManager>,
    sqlite_pool: sqlx::SqlitePool,
    command_tx_cache: Arc<CommandTxCache>,
    allow_simulation_writes: bool,
) -> Router {
    create_api_routes_with_boundary(
        channel_manager,
        sqlite_pool,
        command_tx_cache,
        allow_simulation_writes,
        None,
        ChannelManagementHttpBoundary::unavailable(),
        PointTopologyHttpBoundary::unavailable(),
    )
}

fn create_api_routes_with_boundary(
    channel_manager: Arc<ChannelManager>,
    sqlite_pool: sqlx::SqlitePool,
    command_tx_cache: Arc<CommandTxCache>,
    allow_simulation_writes: bool,
    channel_reconciliation: Option<Arc<ChannelReconciliationApplication>>,
    channel_management: ChannelManagementHttpBoundary,
    point_topology: PointTopologyHttpBoundary,
) -> Router {
    let state = AppState::new(
        channel_manager,
        sqlite_pool,
        command_tx_cache,
        allow_simulation_writes,
        channel_reconciliation,
    );

    let router = Router::new()
        // Health check (top-level for monitoring systems)
        .route("/health", get(health_check))
        // Service management
        .route("/api/status", get(get_service_status))
        // Protocol discovery
        .route("/api/protocols", get(list_protocols))
        // Channel management (CRUD)
        .route("/api/channels", get(get_all_channels).post(create_channel_handler))
        .route("/api/channels/list", get(list_channels))
        .route("/api/channels/search", get(search_channels))
        .route("/api/points", get(list_all_points))
        .route("/api/channels/reconcile", post(reconcile_channels_handler))
        .route("/api/channels/{id}/reconcile", post(reconcile_channel_handler))
        .route("/api/channels/{id}", get(get_channel_detail_handler).put(update_channel_handler).delete(delete_channel_handler))
        .route("/api/channels/{id}/status", get(get_channel_status))
        .route("/api/channels/{id}/control", post(control_channel))
        .route("/api/channels/{id}/enabled", axum::routing::put(set_channel_enabled_handler))
        .route("/api/channels/{id}/logging", axum::routing::put(set_channel_log_level))
        .route("/api/channels/{id}/points", get(get_channel_points_handler))
        .route("/api/channels/{id}/unmapped-points", get(get_unmapped_points_handler))
        .route("/api/channels/{id}/mappings", get(get_channel_mappings_handler).put(update_channel_mappings_handler))
        .route("/api/channels/{channel_id}/{type}/points/{point_id}/mapping", get(get_point_mapping_with_type_handler))
        .route("/api/channels/reload", post(reload_configuration_handler))
        .route("/api/routing/reload", post(reload_routing_handler))
        // Point CRUD routes - type-specific for all operations
        .route("/api/channels/{channel_id}/T/points/{point_id}",
            get(get_telemetry_point_config_handler)
                .post(create_telemetry_point_handler)
                .put(update_telemetry_point_handler)
                .delete(delete_telemetry_point_handler))
        .route("/api/channels/{channel_id}/S/points/{point_id}",
            get(get_signal_point_config_handler)
                .post(create_signal_point_handler)
                .put(update_signal_point_handler)
                .delete(delete_signal_point_handler))
        .route("/api/channels/{channel_id}/C/points/{point_id}",
            get(get_control_point_config_handler)
                .post(create_control_point_handler)
                .put(update_control_point_handler)
                .delete(delete_control_point_handler))
        .route("/api/channels/{channel_id}/A/points/{point_id}",
            get(get_adjustment_point_config_handler)
                .post(create_adjustment_point_handler)
                .put(update_adjustment_point_handler)
                .delete(delete_adjustment_point_handler))
        // Batch point operations endpoint (create/update/delete in single request)
        .route("/api/channels/{channel_id}/points/batch", post(batch_point_operations_handler))
        .route("/api/channels/{channel_id}/provision", post(provision_channel_handler))
        // Unified write endpoint for all point types (T/S/C/A)
        .route("/api/channels/{channel_id}/write", post(write_channel_point))
        .route(
            "/api/channels/{channel_id}/{telemetry_type}/{point_id}",
            get(get_point_info_handler),
        )
        // Admin endpoints (log level + file access)
        .route(
            "/api/admin/logs/level",
            get(get_log_level).post(set_log_level),
        )
        .route("/api/admin/logs/files", get(list_log_files))
        .route("/api/admin/logs/view", get(view_log_file))
        // Template management endpoints
        .route("/api/templates", get(list_templates).post(create_template))
        .route("/api/templates/from-channel/{channel_id}", post(create_template_from_channel))
        .route("/api/templates/{id}", get(get_template).put(update_template).delete(delete_template))
        .route("/api/templates/{id}/apply/{channel_id}", post(apply_template))
        // Network configuration endpoints
        .route("/api/network/interfaces", get(list_network_interfaces))
        .route(
            "/api/network/interfaces/{name}",
            get(get_network_interface).put(update_network_interface),
        )
        .route("/api/network/apply", post(apply_network_changes))
        .layer(axum::Extension(channel_management))
        .layer(axum::Extension(point_topology));
    #[cfg(feature = "modbus")]
    let router = router.layer(axum::Extension(SunSpecDiscoveryBoundary::production()));
    router
        // CRITICAL: Apply middleware BEFORE .with_state() for it to work
        .layer(axum::middleware::from_fn(common::logging::http_request_logger))
        .layer(DefaultBodyLimit::max(1024 * 1024)) // 1 MB request body limit
        .with_state(state)
}

#[cfg(test)]
#[path = "routes_tests.rs"]
mod tests;
