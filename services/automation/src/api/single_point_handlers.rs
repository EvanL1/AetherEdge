//! Single Point Routing API Handlers
//!
//! Provides RESTful API endpoints for managing routing of individual points.
//! Supports separate paths for measurement points and action points.

#![allow(clippy::disallowed_methods)] // json! macro used in multiple functions

use axum::{
    extract::{Path, State},
    response::Json,
};
use common::SuccessResponse;
use serde_json::json;
use std::sync::Arc;

use crate::app_state::AppState;
use crate::dto::{SinglePointRoutingRequest, ToggleRoutingRequest};
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
        ("id" = u16, Path, description = "Instance ID"),
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
) -> Result<Json<SuccessResponse<crate::dto::InstanceMeasurementPoint>>, AutomationError> {
    match state
        .instance_manager
        .load_single_measurement_point(id, point_id)
        .await
    {
        Ok(point) => Ok(Json(SuccessResponse::new(point))),
        Err(e) => Err(AutomationError::InternalError(format!(
            "Failed to load measurement point: {}",
            e
        ))),
    }
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
        ("id" = u16, Path, description = "Instance ID"),
        ("point_id" = u32, Path, description = "Measurement point ID")
    ),
    request_body = crate::dto::SinglePointRoutingRequest,
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
    Json(request): Json<SinglePointRoutingRequest>,
) -> Result<Json<SuccessResponse<serde_json::Value>>, AutomationError> {
    // Upsert routing in database
    state
        .instance_manager
        .upsert_measurement_routing(id, point_id, request)
        .await
        .map_err(|e| AutomationError::InvalidData(format!("Failed to upsert routing: {}", e)))?;

    state
        .instance_manager
        .refresh_routing()
        .await
        .map_err(|e| {
            AutomationError::InternalError(format!(
                "Failed to refresh routing after upsert measurement routing: {}",
                e
            ))
        })?;

    Ok(Json(SuccessResponse::new(json!({
        "message": format!("Routing updated for measurement point {}", point_id)
    }))))
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
        ("id" = u16, Path, description = "Instance ID"),
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
) -> Result<Json<SuccessResponse<serde_json::Value>>, AutomationError> {
    // Delete routing from database
    let rows_affected = state
        .instance_manager
        .delete_measurement_routing(id, point_id)
        .await
        .map_err(|e| AutomationError::InternalError(format!("Failed to delete routing: {}", e)))?;

    if rows_affected == 0 {
        return Err(AutomationError::InternalError(format!(
            "No routing found for measurement point {} in instance {}",
            point_id, id
        )));
    }

    state
        .instance_manager
        .refresh_routing()
        .await
        .map_err(|e| {
            AutomationError::InternalError(format!(
                "Failed to refresh routing after delete measurement routing: {}",
                e
            ))
        })?;

    Ok(Json(SuccessResponse::new(json!({
        "message": format!("Routing deleted for measurement point {}", point_id),
        "rows_affected": rows_affected
    }))))
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
        ("id" = u16, Path, description = "Instance ID"),
        ("point_id" = u32, Path, description = "Measurement point ID")
    ),
    request_body = crate::dto::ToggleRoutingRequest,
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
    Json(request): Json<ToggleRoutingRequest>,
) -> Result<Json<SuccessResponse<serde_json::Value>>, AutomationError> {
    // Toggle routing in database
    let rows_affected = state
        .instance_manager
        .toggle_measurement_routing(id, point_id, request.enabled)
        .await
        .map_err(|e| AutomationError::InternalError(format!("Failed to toggle routing: {}", e)))?;

    if rows_affected == 0 {
        return Err(AutomationError::InternalError(format!(
            "No routing found for measurement point {} in instance {}",
            point_id, id
        )));
    }

    state
        .instance_manager
        .refresh_routing()
        .await
        .map_err(|e| {
            AutomationError::InternalError(format!(
                "Failed to refresh routing after toggle measurement routing: {}",
                e
            ))
        })?;

    let action = if request.enabled {
        "enabled"
    } else {
        "disabled"
    };
    Ok(Json(SuccessResponse::new(json!({
        "message": format!("Routing {} for measurement point {}", action, point_id),
        "rows_affected": rows_affected
    }))))
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
        ("id" = u16, Path, description = "Instance ID"),
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
) -> Result<Json<SuccessResponse<crate::dto::InstanceActionPoint>>, AutomationError> {
    match state
        .instance_manager
        .load_single_action_point(id, point_id)
        .await
    {
        Ok(point) => Ok(Json(SuccessResponse::new(point))),
        Err(e) => Err(AutomationError::InternalError(format!(
            "Failed to load action point: {}",
            e
        ))),
    }
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
        ("id" = u16, Path, description = "Instance ID"),
        ("point_id" = u32, Path, description = "Action point ID")
    ),
    request_body = crate::dto::SinglePointRoutingRequest,
    responses(
        (status = 200, description = "Routing created/updated", body = serde_json::Value,
            example = json!({"message": "Routing updated for action point 201"})
        ),
        (status = 400, description = "Invalid routing configuration"),
        (status = 404, description = "Instance not found"),
        (status = 500, description = "Database error")
    ),
    tag = "automation"
)]
pub async fn upsert_action_routing(
    State(state): State<Arc<AppState>>,
    Path((id, point_id)): Path<(u32, u32)>,
    Json(request): Json<SinglePointRoutingRequest>,
) -> Result<Json<SuccessResponse<serde_json::Value>>, AutomationError> {
    // Upsert routing in database
    state
        .instance_manager
        .upsert_action_routing(id, point_id, request)
        .await
        .map_err(|e| AutomationError::InvalidData(format!("Failed to upsert routing: {}", e)))?;

    state
        .instance_manager
        .refresh_routing()
        .await
        .map_err(|e| {
            AutomationError::InternalError(format!(
                "Failed to refresh routing after upsert action routing: {}",
                e
            ))
        })?;

    Ok(Json(SuccessResponse::new(json!({
        "message": format!("Routing updated for action point {}", point_id)
    }))))
}

/// Delete the M2C routing for a single action point.
///
/// After deletion the action can no longer be dispatched to the device:
/// `execute_action()` takes the "no-routing" branch — it writes to local
/// `inst:{id}:A` storage but `dispatch=None`. The point definition is
/// preserved.
#[utoipa::path(
    delete,
    path = "/api/instances/{id}/actions/{point_id}/routing",
    params(
        ("id" = u16, Path, description = "Instance ID"),
        ("point_id" = u32, Path, description = "Action point ID")
    ),
    responses(
        (status = 200, description = "Routing deleted", body = serde_json::Value,
            example = json!({"message": "Routing deleted for action point 201", "rows_affected": 1})
        ),
        (status = 404, description = "Instance or routing not found"),
        (status = 500, description = "Database error")
    ),
    tag = "automation"
)]
pub async fn delete_action_routing(
    State(state): State<Arc<AppState>>,
    Path((id, point_id)): Path<(u32, u32)>,
) -> Result<Json<SuccessResponse<serde_json::Value>>, AutomationError> {
    // Delete routing from database
    let rows_affected = state
        .instance_manager
        .delete_action_routing(id, point_id)
        .await
        .map_err(|e| AutomationError::InternalError(format!("Failed to delete routing: {}", e)))?;

    if rows_affected == 0 {
        return Err(AutomationError::InternalError(format!(
            "No routing found for action point {} in instance {}",
            point_id, id
        )));
    }

    state
        .instance_manager
        .refresh_routing()
        .await
        .map_err(|e| {
            AutomationError::InternalError(format!(
                "Failed to refresh routing after delete action routing: {}",
                e
            ))
        })?;

    Ok(Json(SuccessResponse::new(json!({
        "message": format!("Routing deleted for action point {}", point_id),
        "rows_affected": rows_affected
    }))))
}

/// Enable or disable the M2C routing for a single action point.
///
/// When disabled, `execute_action()` takes the no-routing branch: the
/// command is written to local `inst:{id}:A` but **not dispatched** to the
/// device. Use this to temporarily suppress a control point (e.g. prevent
/// accidental device triggers during debugging). Re-enabling restores
/// normal dispatch on the next action.
#[utoipa::path(
    patch,
    path = "/api/instances/{id}/actions/{point_id}/routing",
    params(
        ("id" = u16, Path, description = "Instance ID"),
        ("point_id" = u32, Path, description = "Action point ID")
    ),
    request_body = crate::dto::ToggleRoutingRequest,
    responses(
        (status = 200, description = "Routing enabled/disabled", body = serde_json::Value,
            example = json!({"message": "Routing enabled for action point 201", "rows_affected": 1})
        ),
        (status = 404, description = "Instance or routing not found"),
        (status = 500, description = "Database error")
    ),
    tag = "automation"
)]
pub async fn toggle_action_routing(
    State(state): State<Arc<AppState>>,
    Path((id, point_id)): Path<(u32, u32)>,
    Json(request): Json<ToggleRoutingRequest>,
) -> Result<Json<SuccessResponse<serde_json::Value>>, AutomationError> {
    // Toggle routing in database
    let rows_affected = state
        .instance_manager
        .toggle_action_routing(id, point_id, request.enabled)
        .await
        .map_err(|e| AutomationError::InternalError(format!("Failed to toggle routing: {}", e)))?;

    if rows_affected == 0 {
        return Err(AutomationError::InternalError(format!(
            "No routing found for action point {} in instance {}",
            point_id, id
        )));
    }

    state
        .instance_manager
        .refresh_routing()
        .await
        .map_err(|e| {
            AutomationError::InternalError(format!(
                "Failed to refresh routing after toggle action routing: {}",
                e
            ))
        })?;

    let action = if request.enabled {
        "enabled"
    } else {
        "disabled"
    };
    Ok(Json(SuccessResponse::new(json!({
        "message": format!("Routing {} for action point {}", action, point_id),
        "rows_affected": rows_affected
    }))))
}
