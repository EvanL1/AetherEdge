//! Instance Routing Management API Handlers
//!
//! This module provides API handlers for managing routing configurations.
//! It includes functions to create, update, delete, and validate routing
//! configurations between channels and model instances.

#![allow(clippy::disallowed_methods)] // json! macro used in multiple functions

use axum::{
    extract::{Path, State},
    http::HeaderMap,
    response::Json,
};
use common::SuccessResponse;
use serde_json::json;
use std::sync::Arc;

use crate::app_state::AppState;
use crate::dto::{MeasurementRoutingDeleteRequest, PointType, RoutingRequest};
use crate::error::AutomationError;
use crate::routing_loader::{ActionRoutingRow, MeasurementRoutingRow};

/// Create a new routing for an instance
///
/// Creates a new channel-to-instance point routing. Validates that both
/// the channel and instance points exist before creating.
#[utoipa::path(
    post,
    path = "/api/instances/{id}/routing",
    params(
        ("id" = u32, Path, description = "Instance ID")
    ),
    request_body = crate::dto::RoutingRequest,
    responses(
        (status = 200, description = "Routing created", body = serde_json::Value,
            example = json!({
                "routing": {
                    "instance_id": 1,
                    "channel": {
                        "id": 1,
                        "four_remote": "T",
                        "point_id": 101
                    },
                    "point_id": 101
                }
            })
        ),
        (status = 400, description = "Validation error"),
        (status = 404, description = "Instance not found"),
        (status = 500, description = "Database error")
    ),
    tag = "automation"
)]
pub async fn create_instance_routing(
    State(state): State<Arc<AppState>>,
    Path(id): Path<u32>,
    headers: HeaderMap,
    Json(routing): Json<RoutingRequest>,
) -> Result<Json<SuccessResponse<serde_json::Value>>, AutomationError> {
    if routing.point_type == PointType::Action {
        return Err(AutomationError::InvalidRouting(
            "generic action-routing writes are retired; use PUT /api/instances/{id}/actions/{point_id}/routing with Bearer authentication and explicit confirmation"
                .to_string(),
        ));
    }
    let mutation = crate::api::measurement_routing_boundary::generic_mutation(id, &routing)?;
    let acceptance = crate::api::measurement_routing_boundary::apply(
        &state,
        &headers,
        routing.confirmed,
        mutation,
    )
    .await?;
    Ok(Json(SuccessResponse::new(
        crate::api::measurement_routing_boundary::response_data(
            &acceptance,
            format!("Routing updated for measurement point {}", routing.point_id),
        ),
    )))
}

/// Update routings for an instance (UPSERT)
///
/// Updates or creates routings for the specified points. Uses UPSERT semantics:
/// - Points in the request: created if not exists, updated if exists
/// - Points NOT in the request: remain unchanged (not deleted)
///
/// Uses a transaction to ensure atomic operation.
#[utoipa::path(
    put,
    path = "/api/instances/{id}/routing",
    params(
        ("id" = u32, Path, description = "Instance ID")
    ),
    request_body = [crate::dto::RoutingRequest],
    responses(
        (status = 200, description = "Routings updated", body = serde_json::Value,
            example = json!({"message": "Updated 5 routings"})
        ),
        (status = 400, description = "Validation errors"),
        (status = 500, description = "Transaction error")
    ),
    tag = "automation"
)]
pub async fn update_instance_routing(
    State(state): State<Arc<AppState>>,
    Path(id): Path<u32>,
    headers: HeaderMap,
    Json(routings): Json<Vec<RoutingRequest>>,
) -> Result<Json<SuccessResponse<serde_json::Value>>, AutomationError> {
    if routings
        .iter()
        .any(|routing| routing.point_type == PointType::Action)
    {
        return Err(AutomationError::InvalidRouting(
            "action-routing batch writes are disabled until a governed batch command is available; use the authenticated single-action routing endpoint"
                .to_string(),
        ));
    }
    let [routing] = routings.as_slice() else {
        return Err(AutomationError::InvalidRouting(
            "legacy measurement batch writes are retired; submit one revision-fenced route per request"
                .to_string(),
        ));
    };
    let mutation = crate::api::measurement_routing_boundary::generic_mutation(id, routing)?;
    let acceptance = crate::api::measurement_routing_boundary::apply(
        &state,
        &headers,
        routing.confirmed,
        mutation,
    )
    .await?;
    Ok(Json(SuccessResponse::new(
        crate::api::measurement_routing_boundary::response_data(&acceptance, "Updated 1 routing"),
    )))
}

/// Delete all measurement (C2M) routings for an instance.
///
/// Action routes are intentionally excluded: physical command topology must
/// use the governed action-routing application command.
#[utoipa::path(
    delete,
    path = "/api/instances/{id}/routing",
    params(
        ("id" = u32, Path, description = "Instance ID")
    ),
    responses(
        (status = 200, description = "Measurement routings deleted", body = serde_json::Value,
            example = json!({"message": "Deleted 3 measurement routings"})
        ),
        (status = 500, description = "Database error")
    ),
    tag = "automation"
)]
pub async fn delete_instance_routing(
    State(state): State<Arc<AppState>>,
    Path(id): Path<u32>,
    headers: HeaderMap,
    Json(request): Json<MeasurementRoutingDeleteRequest>,
) -> Result<Json<SuccessResponse<serde_json::Value>>, AutomationError> {
    let expected = crate::api::measurement_routing_boundary::revision(request.expected_revision)?;
    let acceptance = crate::api::measurement_routing_boundary::apply(
        &state,
        &headers,
        request.confirmed,
        aether_ports::MeasurementRoutingMutation::delete_for_instance(
            aether_domain::InstanceId::new(id),
            expected,
        ),
    )
    .await?;
    Ok(Json(SuccessResponse::new(
        crate::api::measurement_routing_boundary::response_data(
            &acceptance,
            format!(
                "Deleted {} measurement routings",
                acceptance.affected_routes()
            ),
        ),
    )))
}

/// Validate routing completeness and integrity for an instance.
///
/// Checks that every `measurement_point` maps to a real channel point, that
/// every `action_point` does the same, that the referenced channel is enabled,
/// that each `point_id` exists in the corresponding `{type}_points` table, and
/// that types are compatible (M must map to T/S; A must map to C/A). Returns
/// an `issues` list for use in the configuration health-check UI. Read-only —
/// no state is modified.
#[utoipa::path(
    post,
    path = "/api/instances/{id}/routing/validate",
    params(
        ("id" = u32, Path, description = "Instance ID")
    ),
    request_body = [crate::dto::RoutingRequest],
    responses(
        (status = 200, description = "Validation completed", body = serde_json::Value,
            example = json!({
                "instance_id": 1,
                "validations": [
                    {"channel": "1:T:101", "valid": true, "errors": []},
                    {"channel": "1:T:102", "valid": false, "errors": ["Point not found"]}
                ]
            })
        )
    ),
    tag = "automation"
)]
pub async fn validate_instance_routing(
    State(state): State<Arc<AppState>>,
    Path(id): Path<u32>,
    Json(routings): Json<Vec<RoutingRequest>>,
) -> Result<Json<SuccessResponse<serde_json::Value>>, AutomationError> {
    // Get instance name for validation
    let instance =
        state.instance_manager.get_instance(id).await.map_err(|e| {
            AutomationError::InternalError(format!("Failed to get instance: {}", e))
        })?;
    let instance_name = &instance.core.instance_name;

    let mut results = Vec::new();

    for routing in routings {
        // Save channel info for response
        let channel_info = format!(
            "{}:{}:{}",
            routing
                .channel_id
                .map_or("null".to_string(), |v| v.to_string()),
            routing
                .four_remote
                .as_ref()
                .map_or("null".to_string(), |fr| fr.to_string()),
            routing
                .channel_point_id
                .map_or("null".to_string(), |v| v.to_string())
        );

        // If all three channel fields are null, skip validation (unbound routing)
        if routing.channel_id.is_none()
            && routing.four_remote.is_none()
            && routing.channel_point_id.is_none()
        {
            results.push(json!({
                "channel": &channel_info,
                "valid": true,
                "errors": Vec::<String>::new()
            }));
            continue;
        }

        // Get routing type from request (explicit M/A specification)
        let routing_type = routing.point_type;

        // Validate based on routing type
        let validation_result = match routing_type {
            PointType::Measurement => {
                // Measurement routing (T/S → M)
                let routing_row = MeasurementRoutingRow {
                    channel_id: routing.channel_id,
                    channel_type: routing.four_remote,
                    channel_point_id: routing.channel_point_id,
                    measurement_id: routing.point_id,
                };
                state
                    .instance_manager
                    .validate_measurement_routing(&routing_row, instance_name)
                    .await
            },
            PointType::Action => {
                // Action routing (A → C/A)
                let routing_row = ActionRoutingRow {
                    action_id: routing.point_id,
                    channel_id: routing.channel_id,
                    channel_type: routing.four_remote,
                    channel_point_id: routing.channel_point_id,
                };
                state
                    .instance_manager
                    .validate_action_routing(&routing_row, instance_name)
                    .await
            },
        };

        match validation_result {
            Ok(validation) => {
                results.push(json!({
                    "channel": &channel_info,
                    "valid": validation.is_valid,
                    "errors": validation.errors
                }));
            },
            Err(e) => {
                results.push(json!({
                    "channel": &channel_info,
                    "valid": false,
                    "errors": vec![e.to_string()]
                }));
            },
        }
    }

    Ok(Json(SuccessResponse::new(json!({
        "instance_id": id,
        "validations": results
    }))))
}
