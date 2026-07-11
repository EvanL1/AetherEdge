//! Instance Routing Query API Handlers
//!
//! This module provides API handlers for querying routing configurations.
//! It includes functions to retrieve routing information for instances, channels,
//! and the overall routing table.

#![allow(clippy::disallowed_methods)] // json! macro used in multiple functions

use axum::{
    extract::{Path, State},
    response::Json,
};
use common::SuccessResponse;
use serde_json::json;
use std::sync::Arc;

use crate::app_state::AppState;
use crate::error::AutomationError;

/// Get all routing entries for an instance
///
/// Returns measurement and action routing configuration categorized by type.
#[utoipa::path(
    get,
    path = "/api/instances/{id}/routing",
    params(
        ("id" = u16, Path, description = "Instance ID")
    ),
    responses(
        (status = 200, description = "Instance routing categorized by type", body = serde_json::Value,
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
) -> Result<Json<SuccessResponse<serde_json::Value>>, AutomationError> {
    // Get both measurement and action routing
    let measurement_result = state.instance_manager.get_measurement_routing(id).await;

    let action_result = state.instance_manager.get_action_routing(id).await;

    // Check for database errors - fail fast instead of returning empty list
    let measurements = match measurement_result {
        Ok(data) => data,
        Err(e) => {
            return Err(AutomationError::InternalError(format!(
                "Database error querying measurement routing: {}",
                e
            )));
        },
    };

    let actions = match action_result {
        Ok(data) => data,
        Err(e) => {
            return Err(AutomationError::InternalError(format!(
                "Database error querying action routing: {}",
                e
            )));
        },
    };

    // Build categorized routing entries
    let mut measurement_entries = Vec::new();
    let mut action_entries = Vec::new();

    for m in measurements {
        measurement_entries.push(json!({
            "channel": {
                "id": m.channel_id,
                "four_remote": m.channel_type,
                "point_id": m.channel_point_id
            },
            "point_id": m.measurement_id,
            "enabled": m.enabled
        }));
    }

    for a in actions {
        action_entries.push(json!({
            "channel": {
                "id": a.channel_id,
                "four_remote": a.channel_type,
                "point_id": a.channel_point_id
            },
            "point_id": a.action_id,
            "enabled": a.enabled
        }));
    }

    Ok(Json(SuccessResponse::new(json!({
        "instance_id": id,
        "measurement": measurement_entries,
        "action": action_entries
    }))))
}
