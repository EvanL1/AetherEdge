//! Instance Routing Query API Handlers
//!
//! This module provides API handlers for querying routing configurations.
//! It includes functions to retrieve routing information for instances, channels,
//! and the overall routing table.

use axum::{
    extract::{Path, State},
    response::Json,
};
use common::SuccessResponse;
use std::sync::Arc;

use crate::api::dto::{InstanceRoutingEntryDto, InstanceRoutingResponseDto};
use crate::app_state::AppState;
use crate::error::AutomationError;
use crate::routing_loader::RoutingScope;

/// Get all routing entries for an instance
///
/// Returns measurement and action routing configuration categorized by type.
#[utoipa::path(
    get,
    path = "/api/instances/{id}/routing",
    params(
        ("id" = u32, Path, description = "Instance ID")
    ),
    responses(
        (status = 200, description = "Instance routing categorized by type", body = common::SuccessResponse<crate::api::dto::InstanceRoutingResponseDto>,
            example = json!({
                "instance_id": 1,
                "measurement": [
                    {"channel": {"id": 1, "four_remote": "T", "point_id": 101}, "point_id": 101, "enabled": true},
                    {"channel": {"id": 1, "four_remote": "T", "point_id": 102}, "point_id": 102, "enabled": true}
                ],
                "action": [
                    {"channel": {"id": 1, "four_remote": "C", "point_id": 201}, "point_id": 201, "enabled": true}
                ]
            })
        ),
        (status = 500, description = "Database error")
    ),
    tag = "automation"
)]
pub async fn get_instance_routing_handler(
    State(state): State<Arc<AppState>>,
    Path(id): Path<u32>,
) -> Result<Json<SuccessResponse<InstanceRoutingResponseDto>>, AutomationError> {
    let snapshot = state
        .instance_manager
        .routing_snapshot(RoutingScope::Instance(id))
        .await
        .map_err(|error| {
            AutomationError::InternalError(format!("Failed to query instance routing: {error}"))
        })?;
    let revision = snapshot.revision();
    let (measurements, actions) = snapshot.into_parts();

    Ok(Json(SuccessResponse::new(InstanceRoutingResponseDto {
        instance_id: id,
        measurement: measurements
            .into_iter()
            .map(InstanceRoutingEntryDto::from)
            .collect(),
        action: actions
            .into_iter()
            .map(InstanceRoutingEntryDto::from)
            .collect(),
        logical_routing_revision: revision,
    })))
}
