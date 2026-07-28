//! Global Routing Management API Handlers
//!
//! This module provides API handlers for managing routing configurations at
//! the global level, including queries across all instances and channels.

#![allow(clippy::disallowed_methods)]

use axum::{
    extract::{Path, State},
    response::Json,
};
use common::SuccessResponse;
use serde::Serialize;
use serde_json::{Value, json};
use std::sync::Arc;

use crate::app_state::AppState;
use crate::error::AutomationError;

#[derive(Debug, Serialize)]
struct RoutingEntry {
    routing_id: i64,
    instance_id: u32,
    instance_name: String,
    channel_id: u32,
    channel_type: String,
    channel_point_id: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    measurement_point_id: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    action_point_id: Option<u32>,
    enabled: bool,
}

/// Get all routing configurations (measurement and action)
///
/// Returns all routing entries in the system, categorized by type.
#[utoipa::path(
    get,
    path = "/api/routing",
    responses(
        (status = 200, description = "All routing configurations", body = Value,
            example = json!({
                "measurement_routing": [],
                "action_routing": [],
                "total": {"measurement": 0, "action": 0}
            })
        ),
        (status = 500, description = "Database error")
    ),
    tag = "automation"
)]
pub async fn get_all_routing_handler(
    State(state): State<Arc<AppState>>,
) -> Result<Json<SuccessResponse<Value>>, AutomationError> {
    // Query measurement routing
    let measurement_routing = sqlx::query_as::<_, (i64, u32, String, u32, String, u32, u32, bool)>(
        r#"
        SELECT routing_id, instance_id, instance_name, channel_id, channel_type,
               channel_point_id, measurement_id AS measurement_point_id, enabled
        FROM measurement_routing
        ORDER BY instance_id, measurement_id
        "#,
    )
    .fetch_all(&state.instance_manager.pool)
    .await
    .map_err(|e| {
        AutomationError::InternalError(format!("Failed to query measurement routing: {}", e))
    })?;

    // Query action routing
    let action_routing = sqlx::query_as::<_, (i64, u32, String, u32, u32, String, u32, bool)>(
        r#"
        SELECT routing_id, instance_id, instance_name, action_id AS action_point_id, channel_id, channel_type,
               channel_point_id, enabled
        FROM action_routing
        ORDER BY instance_id, action_id
        "#,
    )
    .fetch_all(&state.instance_manager.pool)
    .await
    .map_err(|e| AutomationError::InternalError(format!("Failed to query action routing: {}", e)))?;
    let logical_routing_revision: i64 = sqlx::query_scalar(
        "SELECT revision FROM configuration_revisions WHERE scope = 'logical_routing'",
    )
    .fetch_one(&state.instance_manager.pool)
    .await
    .map_err(|error| {
        AutomationError::InternalError(format!("Failed to query logical-routing revision: {error}"))
    })?;

    let measurement_entries: Vec<RoutingEntry> = measurement_routing
        .into_iter()
        .map(
            |(
                routing_id,
                instance_id,
                instance_name,
                channel_id,
                channel_type,
                channel_point_id,
                measurement_point_id,
                enabled,
            )| {
                RoutingEntry {
                    routing_id,
                    instance_id,
                    instance_name,
                    channel_id,
                    channel_type,
                    channel_point_id,
                    measurement_point_id: Some(measurement_point_id),
                    action_point_id: None,
                    enabled,
                }
            },
        )
        .collect();

    let action_entries: Vec<RoutingEntry> = action_routing
        .into_iter()
        .map(
            |(
                routing_id,
                instance_id,
                instance_name,
                action_point_id,
                channel_id,
                channel_type,
                channel_point_id,
                enabled,
            )| {
                RoutingEntry {
                    routing_id,
                    instance_id,
                    instance_name,
                    channel_id,
                    channel_type,
                    channel_point_id,
                    measurement_point_id: None,
                    action_point_id: Some(action_point_id),
                    enabled,
                }
            },
        )
        .collect();

    let result = json!({
        "measurement_routing": measurement_entries,
        "action_routing": action_entries,
        "total": {
            "measurement": measurement_entries.len(),
            "action": action_entries.len()
        },
        "logical_routing_revision": logical_routing_revision
    });

    Ok(Json(SuccessResponse::new(result)))
}

/// Get routing by channel ID
///
/// Returns all routing entries (uplink and downlink) for a specific channel.
#[utoipa::path(
    get,
    path = "/api/routing/by-channel/{channel_id}",
    params(
        ("channel_id" = u32, Path, description = "Channel ID")
    ),
    responses(
        (status = 200, description = "Channel routing entries", body = Value,
            example = json!({
                "channel_id": 1001,
                "uplink": [],
                "downlink": []
            })
        ),
        (status = 500, description = "Database error")
    ),
    tag = "automation"
)]
pub async fn get_routing_by_channel_handler(
    State(state): State<Arc<AppState>>,
    Path(channel_id): Path<u32>,
) -> Result<Json<SuccessResponse<Value>>, AutomationError> {
    // Query uplink routing (C2M)
    let uplink = sqlx::query_as::<_, (i64, u16, String, String, u32, u32, bool)>(
        r#"
        SELECT routing_id, instance_id, instance_name, channel_type,
               channel_point_id, measurement_id AS measurement_point_id, enabled
        FROM measurement_routing
        WHERE channel_id = ?
        ORDER BY instance_id, measurement_id
        "#,
    )
    .bind(channel_id)
    .fetch_all(&state.instance_manager.pool)
    .await
    .map_err(|e| {
        AutomationError::InternalError(format!("Failed to query uplink routing: {}", e))
    })?;

    // Query downlink routing (M2C)
    let downlink = sqlx::query_as::<_, (i64, u16, String, u32, String, u32, bool)>(
        r#"
        SELECT routing_id, instance_id, instance_name, action_id AS action_point_id, channel_type,
               channel_point_id, enabled
        FROM action_routing
        WHERE channel_id = ?
        ORDER BY instance_id, action_id
        "#,
    )
    .bind(channel_id)
    .fetch_all(&state.instance_manager.pool)
    .await
    .map_err(|e| {
        AutomationError::InternalError(format!("Failed to query downlink routing: {}", e))
    })?;

    let result = json!({
        "channel_id": channel_id,
        "uplink": uplink.into_iter().map(|(routing_id, instance_id, instance_name, channel_type, channel_point_id, measurement_point_id, enabled)| {
            json!({
                "routing_id": routing_id,
                "instance_id": instance_id,
                "instance_name": instance_name,
                "channel_type": channel_type,
                "channel_point_id": channel_point_id,
                "measurement_point_id": measurement_point_id,
                "enabled": enabled
            })
        }).collect::<Vec<_>>(),
        "downlink": downlink.into_iter().map(|(routing_id, instance_id, instance_name, action_point_id, channel_type, channel_point_id, enabled)| {
            json!({
                "routing_id": routing_id,
                "instance_id": instance_id,
                "instance_name": instance_name,
                "action_point_id": action_point_id,
                "channel_type": channel_type,
                "channel_point_id": channel_point_id,
                "enabled": enabled
            })
        }).collect::<Vec<_>>()
    });

    Ok(Json(SuccessResponse::new(result)))
}
