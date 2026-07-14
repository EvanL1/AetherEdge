//! Single Point Routing API Handlers
//!
//! Provides RESTful API endpoints for managing routing of individual points.
//! Supports separate paths for measurement points and action points.

#![allow(clippy::disallowed_methods)] // json! macro used in multiple functions

use axum::{
    extract::{Path, State},
    http::{HeaderMap, HeaderValue, header::ETAG},
    response::{IntoResponse, Json, Response},
};
use common::SuccessResponse;
#[allow(unused_imports)]
use serde_json::json;
use std::sync::Arc;

use crate::app_state::AppState;
use crate::dto::{
    ActionRoutingConfirmationBody, ActionRoutingToggleBody, ActionRoutingUpsertBody,
    MeasurementRoutingDeleteRequest, MeasurementRoutingToggleRequest,
    MeasurementRoutingUpsertRequest,
};
use crate::error::AutomationError;

// ============================================================================
// Measurement Point Handlers
// ============================================================================

/// Get full details for a single measurement point (definition + routing + current value).
///
/// Returns the point definition from `instance.measurement_points`, the
/// associated C2M routing (which channel and channel-point it maps to), and
/// the latest measurement value from `inst:{id}:M`. Used by the point-detail
/// dialog on the frontend. Returns 404 if the instance or `point_id` does
/// not exist.
#[utoipa::path(
    get,
    path = "/api/instances/{id}/measurements/{point_id}",
    params(
        ("id" = u32, Path, description = "Instance ID"),
        ("point_id" = u32, Path, description = "Measurement point ID")
    ),
    responses(
        (status = 200, description = "Measurement point with routing", body = crate::dto::InstanceMeasurementPoint),
        (status = 404, description = "Instance or point not found"),
        (status = 500, description = "Database error")
    ),
    tag = "automation"
)]
pub async fn get_measurement_point(
    State(state): State<Arc<AppState>>,
    Path((id, point_id)): Path<(u32, u32)>,
) -> Result<Response, AutomationError> {
    let point = state
        .instance_manager
        .load_single_measurement_point(id, point_id)
        .await
        .map_err(|e| {
            AutomationError::InternalError(format!("Failed to load measurement point: {}", e))
        })?;
    let revision: i64 = sqlx::query_scalar(
        "SELECT revision FROM configuration_revisions WHERE scope = 'logical_routing'",
    )
    .fetch_one(state.instance_manager.pool())
    .await
    .map_err(|error| {
        AutomationError::InternalError(format!("Failed to load logical-routing revision: {error}"))
    })?;
    let mut response = Json(SuccessResponse::new(point)).into_response();
    let etag = HeaderValue::from_str(&format!("\"{revision}\""))
        .map_err(|error| AutomationError::InternalError(error.to_string()))?;
    response.headers_mut().insert(ETAG, etag);
    Ok(response)
}

/// Create or update the C2M routing for a single measurement point (UPSERT semantics).
///
/// Binds `instance.measurement_point` to a `channel.{T|S}.point` — after
/// this, new values arriving at that channel point become visible through the
/// instance's SHM projection. An existing routing is overwritten. The change
/// immediately triggers a routing-cache reload.
#[utoipa::path(
    put,
    path = "/api/instances/{id}/measurements/{point_id}/routing",
    params(
        ("id" = u32, Path, description = "Instance ID"),
        ("point_id" = u32, Path, description = "Measurement point ID")
    ),
    request_body = crate::dto::MeasurementRoutingUpsertRequest,
    responses(
        (status = 200, description = "Routing created/updated", body = serde_json::Value,
            example = json!({"message": "Routing updated for measurement point 101"})
        ),
        (status = 400, description = "Invalid routing configuration"),
        (status = 404, description = "Instance not found"),
        (status = 500, description = "Database error")
    ),
    tag = "automation"
)]
pub async fn upsert_measurement_routing(
    State(state): State<Arc<AppState>>,
    Path((id, point_id)): Path<(u32, u32)>,
    headers: HeaderMap,
    Json(request): Json<MeasurementRoutingUpsertRequest>,
) -> Result<Json<SuccessResponse<serde_json::Value>>, AutomationError> {
    let mutation =
        crate::api::measurement_routing_boundary::upsert_mutation(id, point_id, &request)?;
    let acceptance = crate::api::measurement_routing_boundary::apply(
        &state,
        &headers,
        request.confirmed,
        mutation,
    )
    .await?;
    Ok(Json(SuccessResponse::new(
        crate::api::measurement_routing_boundary::response_data(
            &acceptance,
            format!("Routing updated for measurement point {point_id}"),
        ),
    )))
}

/// Delete the C2M routing for a single measurement point.
///
/// Removes the routing but **preserves the point definition** — the instance
/// product model is unchanged. After deletion no data flows into this
/// measurement point; the corresponding field in `inst:{id}:M` stops
/// updating and retains its last-known-good value.
#[utoipa::path(
    delete,
    path = "/api/instances/{id}/measurements/{point_id}/routing",
    params(
        ("id" = u32, Path, description = "Instance ID"),
        ("point_id" = u32, Path, description = "Measurement point ID")
    ),
    responses(
        (status = 200, description = "Routing deleted", body = serde_json::Value,
            example = json!({"message": "Routing deleted for measurement point 101", "rows_affected": 1})
        ),
        (status = 404, description = "Instance or routing not found"),
        (status = 500, description = "Database error")
    ),
    tag = "automation"
)]
pub async fn delete_measurement_routing(
    State(state): State<Arc<AppState>>,
    Path((id, point_id)): Path<(u32, u32)>,
    headers: HeaderMap,
    Json(request): Json<MeasurementRoutingDeleteRequest>,
) -> Result<Json<SuccessResponse<serde_json::Value>>, AutomationError> {
    let revision = crate::api::measurement_routing_boundary::revision(request.expected_revision)?;
    let mutation = aether_ports::MeasurementRoutingMutation::delete(
        aether_ports::MeasurementRouteKey::new(
            aether_domain::InstanceId::new(id),
            aether_domain::PointId::new(point_id),
        ),
        revision,
    );
    let acceptance = crate::api::measurement_routing_boundary::apply(
        &state,
        &headers,
        request.confirmed,
        mutation,
    )
    .await?;
    Ok(Json(SuccessResponse::new(
        crate::api::measurement_routing_boundary::response_data(
            &acceptance,
            format!("Routing deleted for measurement point {point_id}"),
        ),
    )))
}

/// Enable or disable the C2M routing for a single measurement point.
///
/// Lighter-weight than deletion — the routing definition is retained but
/// data flow is paused. When disabled, the field in `inst:{id}:M` stops
/// updating (last-known-good is preserved); enabling it resumes normal
/// sync. Commonly used to temporarily silence a faulty point's upstream
/// data.
#[utoipa::path(
    patch,
    path = "/api/instances/{id}/measurements/{point_id}/routing",
    params(
        ("id" = u32, Path, description = "Instance ID"),
        ("point_id" = u32, Path, description = "Measurement point ID")
    ),
    request_body = crate::dto::MeasurementRoutingToggleRequest,
    responses(
        (status = 200, description = "Routing enabled/disabled", body = serde_json::Value,
            example = json!({"message": "Routing enabled for measurement point 101", "rows_affected": 1})
        ),
        (status = 404, description = "Instance or routing not found"),
        (status = 500, description = "Database error")
    ),
    tag = "automation"
)]
pub async fn toggle_measurement_routing(
    State(state): State<Arc<AppState>>,
    Path((id, point_id)): Path<(u32, u32)>,
    headers: HeaderMap,
    Json(request): Json<MeasurementRoutingToggleRequest>,
) -> Result<Json<SuccessResponse<serde_json::Value>>, AutomationError> {
    let revision = crate::api::measurement_routing_boundary::revision(request.expected_revision)?;
    let mutation = aether_ports::MeasurementRoutingMutation::set_enabled(
        aether_ports::MeasurementRouteKey::new(
            aether_domain::InstanceId::new(id),
            aether_domain::PointId::new(point_id),
        ),
        request.enabled,
        revision,
    );
    let acceptance = crate::api::measurement_routing_boundary::apply(
        &state,
        &headers,
        request.confirmed,
        mutation,
    )
    .await?;
    let operation = if request.enabled {
        "enabled"
    } else {
        "disabled"
    };
    Ok(Json(SuccessResponse::new(
        crate::api::measurement_routing_boundary::response_data(
            &acceptance,
            format!("Routing {operation} for measurement point {point_id}"),
        ),
    )))
}

// ============================================================================
// Action Point Handlers
// ============================================================================

/// Get full details for a single action point (definition + M2C routing + last written value).
///
/// The action-point counterpart of `/measurement-point/{id}`. Returns the
/// `action_point` definition, the associated M2C routing (which channel
/// C/A point it targets), and the most recently written command value from
/// `inst:{id}:A`.
#[utoipa::path(
    get,
    path = "/api/instances/{id}/actions/{point_id}",
    params(
        ("id" = u32, Path, description = "Instance ID"),
        ("point_id" = u32, Path, description = "Action point ID")
    ),
    responses(
        (status = 200, description = "Action point with routing", body = crate::dto::InstanceActionPoint),
        (status = 404, description = "Instance or point not found"),
        (status = 500, description = "Database error")
    ),
    tag = "automation"
)]
pub async fn get_action_point(
    State(state): State<Arc<AppState>>,
    Path((id, point_id)): Path<(u32, u32)>,
) -> Result<Response, AutomationError> {
    let point = state
        .instance_manager
        .load_single_action_point(id, point_id)
        .await
        .map_err(|e| {
            AutomationError::InternalError(format!("Failed to load action point: {}", e))
        })?;
    let revision: i64 = sqlx::query_scalar(
        "SELECT revision FROM configuration_revisions WHERE scope = 'logical_routing'",
    )
    .fetch_one(state.instance_manager.pool())
    .await
    .map_err(|error| {
        AutomationError::InternalError(format!("Failed to load logical-routing revision: {error}"))
    })?;
    let mut response = Json(SuccessResponse::new(point)).into_response();
    let etag = HeaderValue::from_str(&format!("\"{revision}\""))
        .map_err(|error| AutomationError::InternalError(error.to_string()))?;
    response.headers_mut().insert(ETAG, etag);
    Ok(response)
}

/// Create or update the M2C routing for a single action point (UPSERT semantics).
///
/// Binds `instance.action_point` to a `channel.{C|A}.point` — commands
/// issued via `POST /api/instances/{id}/action` or the rules engine then
/// travel through SHM + UDS to that channel and are dispatched to the
/// device. The routing cache is reloaded immediately; the next
/// `execute_action` call uses the new routing.
#[utoipa::path(
    put,
    path = "/api/instances/{id}/actions/{point_id}/routing",
    params(
        ("id" = u32, Path, description = "Instance ID"),
        ("point_id" = u32, Path, description = "Action point ID"),
        ("x-request-id" = Option<String>, Header, description = "Optional UUID audit correlation ID")
    ),
    request_body(
        content = crate::dto::ActionRoutingUpsertBody,
        description = "Governed action-route upsert or unbind; confirmed=true is required"
    ),
    responses(
        (status = 200, description = "Action-routing mutation accepted. If audit.status=incomplete or runtime.status=commands_revoked, retryable=false and the mutation must not be retried.", body = crate::dto::ActionRoutingMutationResponse,
            example = json!({
                "success": true,
                "data": {
                    "message": "Routing updated for action point 201",
                    "request_id": "018f0000-0000-7000-8000-000000000007",
                    "operation": "upsert",
                    "affected_routes": 1,
                    "resulting_revision": 8,
                    "audit": {"status": "recorded", "retryable": false},
                    "runtime": {"status": "published", "reconciliation_required": false},
                    "retryable": false
                }
            })
        ),
        (status = 400, description = "Invalid routing configuration"),
        (status = 403, description = "Missing/invalid Bearer credentials or actor lacks automation.routing.manage"),
        (status = 409, description = "The shared logical-routing revision is stale"),
        (status = 404, description = "Instance not found"),
        (status = 422, description = "Explicit confirmation is required"),
        (status = 503, description = "Mandatory attempted audit or pre-commit routing storage is unavailable")
    ),
    security(("bearer_auth" = [])),
    tag = "automation"
)]
pub async fn upsert_action_routing(
    State(state): State<Arc<AppState>>,
    Path((id, point_id)): Path<(u32, u32)>,
    headers: HeaderMap,
    Json(request): Json<ActionRoutingUpsertBody>,
) -> Result<Json<SuccessResponse<serde_json::Value>>, AutomationError> {
    let mutation =
        crate::api::action_routing_boundary::single_point_mutation(id, point_id, &request)?;
    let acceptance =
        crate::api::action_routing_boundary::apply(&state, &headers, request.confirmed, mutation)
            .await?;
    Ok(Json(SuccessResponse::new(
        crate::api::action_routing_boundary::response_data(
            &acceptance,
            format!("Routing updated for action point {point_id}"),
        ),
    )))
}

/// Delete the M2C routing for a single action point.
///
/// After deletion, the governed command dispatcher fails closed before the
/// physical sink because no channel target can be resolved. No local value is
/// stored as a substitute for device dispatch. The point definition remains.
#[utoipa::path(
    delete,
    path = "/api/instances/{id}/actions/{point_id}/routing",
    params(
        ("id" = u32, Path, description = "Instance ID"),
        ("point_id" = u32, Path, description = "Action point ID"),
        ("x-request-id" = Option<String>, Header, description = "Optional UUID audit correlation ID")
    ),
    request_body(
        content = crate::dto::ActionRoutingConfirmationBody,
        description = "Governed action-route deletion; confirmed=true is required"
    ),
    responses(
        (status = 200, description = "Action-routing mutation accepted. If audit.status=incomplete or runtime.status=commands_revoked, retryable=false and the mutation must not be retried.", body = crate::dto::ActionRoutingMutationResponse,
            example = json!({
                "success": true,
                "data": {
                    "message": "Routing deleted for action point 201",
                    "request_id": "018f0000-0000-7000-8000-000000000007",
                    "operation": "delete",
                    "affected_routes": 1,
                    "resulting_revision": 8,
                    "audit": {"status": "recorded", "retryable": false},
                    "runtime": {"status": "published", "reconciliation_required": false},
                    "retryable": false
                }
            })
        ),
        (status = 403, description = "Missing/invalid Bearer credentials or actor lacks automation.routing.manage"),
        (status = 409, description = "The shared logical-routing revision is stale"),
        (status = 404, description = "Instance or routing not found"),
        (status = 422, description = "Explicit confirmation is required"),
        (status = 503, description = "Mandatory attempted audit or pre-commit routing storage is unavailable")
    ),
    security(("bearer_auth" = [])),
    tag = "automation"
)]
pub async fn delete_action_routing(
    State(state): State<Arc<AppState>>,
    Path((id, point_id)): Path<(u32, u32)>,
    headers: HeaderMap,
    Json(request): Json<ActionRoutingConfirmationBody>,
) -> Result<Json<SuccessResponse<serde_json::Value>>, AutomationError> {
    let expected = crate::api::measurement_routing_boundary::revision(request.expected_revision)?;
    let mutation = aether_ports::RevisionedActionRoutingMutation::delete(
        aether_ports::ActionRouteKey::new(
            aether_domain::InstanceId::new(id),
            aether_domain::PointId::new(point_id),
        ),
        expected,
    );
    let acceptance =
        crate::api::action_routing_boundary::apply(&state, &headers, request.confirmed, mutation)
            .await?;
    Ok(Json(SuccessResponse::new(
        crate::api::action_routing_boundary::response_data(
            &acceptance,
            format!("Routing deleted for action point {point_id}"),
        ),
    )))
}

/// Enable or disable the M2C routing for a single action point.
///
/// When disabled, the governed command dispatcher fails closed before the
/// physical sink; it does not create a local success value. Use this to
/// suppress a control point temporarily. Re-enabling restores routing for the
/// next accepted action.
#[utoipa::path(
    patch,
    path = "/api/instances/{id}/actions/{point_id}/routing",
    params(
        ("id" = u32, Path, description = "Instance ID"),
        ("point_id" = u32, Path, description = "Action point ID"),
        ("x-request-id" = Option<String>, Header, description = "Optional UUID audit correlation ID")
    ),
    request_body(
        content = crate::dto::ActionRoutingToggleBody,
        description = "Governed action-route enable/disable; confirmed=true is required"
    ),
    responses(
        (status = 200, description = "Action-routing mutation accepted. If audit.status=incomplete or runtime.status=commands_revoked, retryable=false and the mutation must not be retried.", body = crate::dto::ActionRoutingMutationResponse,
            example = json!({
                "success": true,
                "data": {
                    "message": "Routing enabled for action point 201",
                    "request_id": "018f0000-0000-7000-8000-000000000007",
                    "operation": "enable",
                    "affected_routes": 1,
                    "resulting_revision": 8,
                    "audit": {"status": "recorded", "retryable": false},
                    "runtime": {"status": "published", "reconciliation_required": false},
                    "retryable": false
                }
            })
        ),
        (status = 403, description = "Missing/invalid Bearer credentials or actor lacks automation.routing.manage"),
        (status = 409, description = "The shared logical-routing revision is stale"),
        (status = 404, description = "Instance or routing not found"),
        (status = 422, description = "Explicit confirmation is required"),
        (status = 503, description = "Mandatory attempted audit or pre-commit routing storage is unavailable")
    ),
    security(("bearer_auth" = [])),
    tag = "automation"
)]
pub async fn toggle_action_routing(
    State(state): State<Arc<AppState>>,
    Path((id, point_id)): Path<(u32, u32)>,
    headers: HeaderMap,
    Json(request): Json<ActionRoutingToggleBody>,
) -> Result<Json<SuccessResponse<serde_json::Value>>, AutomationError> {
    let expected = crate::api::measurement_routing_boundary::revision(request.expected_revision)?;
    let mutation = aether_ports::RevisionedActionRoutingMutation::set_enabled(
        aether_ports::ActionRouteKey::new(
            aether_domain::InstanceId::new(id),
            aether_domain::PointId::new(point_id),
        ),
        request.enabled,
        expected,
    );
    let acceptance =
        crate::api::action_routing_boundary::apply(&state, &headers, request.confirmed, mutation)
            .await?;
    let operation = if request.enabled {
        "enabled"
    } else {
        "disabled"
    };
    Ok(Json(SuccessResponse::new(
        crate::api::action_routing_boundary::response_data(
            &acceptance,
            format!("Routing {operation} for action point {point_id}"),
        ),
    )))
}
