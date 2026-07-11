//! API Route Configuration
//!
//! Central route definition for all Model Service API endpoints

use axum::{
    Router,
    extract::DefaultBodyLimit,
    routing::{get, post},
};
use std::sync::Arc;

#[cfg(feature = "swagger-ui")]
use utoipa::OpenApi;

use crate::app_state::AppState;

// Import handlers from api module
use crate::api::cloud_sync::export_instances;
use crate::api::health_handlers::health_check;
use crate::api::product_handlers::{get_product_points, list_products};

use crate::api::instance_management_handlers::{
    create_instance, delete_instance, execute_instance_action, reload_instances_from_db,
    update_instance,
};
use crate::api::instance_query_handlers::{
    get_instance, get_instance_children, get_instance_data, get_instance_points, get_topology_tree,
    list_instances, list_instances_slim, search_instances,
};

// New global routing handlers (work with unified database)
use crate::api::global_routing_handlers::{
    delete_all_routing_handler, delete_channel_routing_handler,
    delete_instance_routing_handler as global_delete_instance_routing, get_all_routing_handler,
    get_routing_by_channel_handler,
};
// Refactored routing handlers (work with unified database)
use crate::api::routing_management_handlers::{
    create_instance_routing, delete_instance_routing, update_instance_routing,
    validate_instance_routing,
};
use crate::api::routing_query_handlers::get_instance_routing_handler;

use crate::api::single_point_handlers::{
    delete_action_routing, delete_measurement_routing, get_action_point, get_measurement_point,
    toggle_action_routing, toggle_measurement_routing, upsert_action_routing,
    upsert_measurement_routing,
};

use crate::api::property_handlers::{delete_property, upsert_property};

use common::admin_api::{get_log_level, list_log_files, set_log_level, view_log_file};

// OpenAPI documentation - only compiled when swagger-ui feature is enabled
#[cfg(feature = "swagger-ui")]
#[derive(OpenApi)]
#[openapi(
    paths(
        crate::api::instance_query_handlers::list_instances,
        crate::api::instance_query_handlers::list_instances_slim,
        crate::api::instance_query_handlers::search_instances,
        crate::api::instance_management_handlers::create_instance,
        crate::api::instance_query_handlers::get_instance,
        crate::api::instance_management_handlers::update_instance,
        crate::api::instance_management_handlers::delete_instance,
        crate::api::instance_query_handlers::get_instance_data,
        crate::api::instance_query_handlers::get_instance_points,
        crate::api::instance_management_handlers::execute_instance_action,
        // Instance-level routing handlers (refactored for unified database)
        crate::api::routing_query_handlers::get_instance_routing_handler,
        crate::api::routing_management_handlers::create_instance_routing,
        crate::api::routing_management_handlers::update_instance_routing,
        crate::api::routing_management_handlers::delete_instance_routing,
        crate::api::routing_management_handlers::validate_instance_routing,
        // Single point routing handlers
        crate::api::single_point_handlers::get_measurement_point,
        crate::api::single_point_handlers::upsert_measurement_routing,
        crate::api::single_point_handlers::delete_measurement_routing,
        crate::api::single_point_handlers::toggle_measurement_routing,
        crate::api::single_point_handlers::get_action_point,
        crate::api::single_point_handlers::upsert_action_routing,
        crate::api::single_point_handlers::delete_action_routing,
        crate::api::single_point_handlers::toggle_action_routing,
        // Single property handlers
        crate::api::property_handlers::upsert_property,
        crate::api::property_handlers::delete_property,
        // Global routing handlers (unified database)
        crate::api::global_routing_handlers::get_all_routing_handler,
        crate::api::global_routing_handlers::delete_all_routing_handler,
        crate::api::global_routing_handlers::get_routing_by_channel_handler,
        crate::api::global_routing_handlers::delete_instance_routing_handler,
        crate::api::global_routing_handlers::delete_channel_routing_handler,
        crate::api::product_handlers::list_products,
        crate::api::product_handlers::get_product_points,
        // Cloud sync endpoints
        crate::api::cloud_sync::export_instances,
        // Admin endpoints
        common::admin_api::set_log_level,
        common::admin_api::get_log_level
    ),
    components(
        schemas(
            crate::dto::CreateInstanceDto,
            crate::dto::UpdateInstanceDto,
            crate::dto::ActionRequest,
            crate::dto::RoutingRequest,
            crate::dto::SinglePointRoutingRequest,
            crate::dto::ToggleRoutingRequest,
            crate::dto::RoutingUpdate,
            crate::dto::RoutingType,
            crate::config::Product,
            crate::config::MeasurementPoint,
            crate::config::ActionPoint,
            crate::config::PropertyTemplate,
            // Admin schemas
            common::admin_api::SetLogLevelRequest,
            common::admin_api::LogLevelResponse
        )
    ),
    tags(
        (name = "aether-automation", description = "Automation and rule orchestration API"),
        (name = "products", description = "Product template management (read-only)"),
        (name = "admin", description = "Administration and service management")
    )
)]
pub struct AutomationApiDoc;

/// Create all API routes for the Model Service
pub fn create_routes(state: Arc<AppState>) -> Router {
    Router::new()
        // Health check
        .route("/health", get(health_check))
        // Instance management API
        .route("/api/instances", get(list_instances).post(create_instance))
        .route("/api/instances/list", get(list_instances_slim))
        .route("/api/instances/search", get(search_instances))
        .route(
            "/api/instances/{id}",
            get(get_instance)
                .put(update_instance)
                .delete(delete_instance),
        )
        .route("/api/instances/{id}/data", get(get_instance_data))
        .route("/api/instances/{id}/points", get(get_instance_points))
        .route("/api/instances/{id}/action", post(execute_instance_action))
        .route("/api/instances/{id}/children", get(get_instance_children))
        // Topology tree endpoint
        .route("/api/topology", get(get_topology_tree))
        .route("/api/instances/reload", post(reload_instances_from_db))

        // Instance-level routing endpoints (refactored for unified database)
        .route(
            "/api/instances/{id}/routing",
            get(get_instance_routing_handler)
                .post(create_instance_routing)
                .put(update_instance_routing)
                .delete(delete_instance_routing),
        )
        .route(
            "/api/instances/{id}/routing/validate",
            post(validate_instance_routing),
        )
        // Single point routing endpoints
        .route(
            "/api/instances/{id}/measurements/{point_id}",
            get(get_measurement_point),
        )
        .route(
            "/api/instances/{id}/measurements/{point_id}/routing",
            axum::routing::put(upsert_measurement_routing)
                .delete(delete_measurement_routing)
                .patch(toggle_measurement_routing),
        )
        .route(
            "/api/instances/{id}/actions/{point_id}",
            get(get_action_point),
        )
        .route(
            "/api/instances/{id}/actions/{point_id}/routing",
            axum::routing::put(upsert_action_routing)
                .delete(delete_action_routing)
                .patch(toggle_action_routing),
        )

        // Single property value endpoints (instance_properties table)
        .route(
            "/api/instances/{id}/properties/{property_id}",
            axum::routing::put(upsert_property).delete(delete_property),
        )

        // Global routing management endpoints (new unified database APIs)
        .route("/api/routing", get(get_all_routing_handler).delete(delete_all_routing_handler))
        .route("/api/routing/by-channel/{channel_id}", get(get_routing_by_channel_handler))
        .route("/api/routing/instances/{id}", axum::routing::delete(global_delete_instance_routing))
        .route("/api/routing/channels/{channel_id}", axum::routing::delete(delete_channel_routing_handler))

        // Product management endpoints (read-only)
        .route("/api/products", get(list_products))
        .route("/api/products/{product_name}/points", get(get_product_points))
        // Cloud sync endpoints
        .route("/api/instances/export", get(export_instances))
        // Admin endpoints (log level + file access)
        .route(
            "/api/admin/logs/level",
            get(get_log_level).post(set_log_level),
        )
        .route("/api/admin/logs/files", get(list_log_files))
        .route("/api/admin/logs/view", get(view_log_file))
        // Apply HTTP request logging middleware
        .layer(axum::middleware::from_fn(common::logging::http_request_logger))
        .layer(DefaultBodyLimit::max(1024 * 1024)) // 1 MB request body limit
        .with_state(state)
}
