//! Global Routing Management API Handlers
//!
//! This module provides API handlers for managing routing configurations at
//! the global level, including queries across all instances and channels.

#![allow(clippy::disallowed_methods)]

use axum::{
    extract::{Path, Query, State},
    http::HeaderMap,
    response::Json,
};
use common::SuccessResponse;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::sync::Arc;

use crate::app_state::AppState;
use crate::error::AutomationError;

#[derive(Debug, Deserialize)]
pub struct ConfirmQuery {
    pub confirm: Option<bool>,
}

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
        }
    });

    Ok(Json(SuccessResponse::new(result)))
}

/// Delete all routing configurations (DANGEROUS)
///
/// Removes all routing entries from the system. Requires confirmation parameter.
#[utoipa::path(
    delete,
    path = "/api/routing",
    params(
        ("confirm" = Option<bool>, Query, description = "Confirmation flag (must be true)"),
        ("x-request-id" = Option<String>, Header, description = "Optional UUID audit correlation ID")
    ),
    responses(
        (status = 200, description = "All routing deleted", body = Value),
        (status = 403, description = "Missing/invalid Bearer credentials or actor lacks automation.routing.manage"),
        (status = 422, description = "Explicit confirmation is required"),
        (status = 503, description = "Mandatory audit, routing storage, or cache publication is unavailable")
    ),
    security(("bearer_auth" = [])),
    tag = "automation"
)]
pub async fn delete_all_routing_handler(
    State(state): State<Arc<AppState>>,
    Query(params): Query<ConfirmQuery>,
    headers: HeaderMap,
) -> Result<Json<SuccessResponse<Value>>, AutomationError> {
    let acceptance = crate::api::action_routing_boundary::apply(
        &state,
        &headers,
        params.confirm.unwrap_or(false),
        aether_ports::ActionRoutingMutation::delete_all(),
    )
    .await?;

    let measurement_result = sqlx::query("DELETE FROM measurement_routing")
        .execute(&state.instance_manager.pool)
        .await
        .map_err(|e| {
            AutomationError::InternalError(format!("Failed to delete measurement routing: {}", e))
        })?;

    state
        .instance_manager
        .refresh_routing()
        .await
        .map_err(|e| {
            AutomationError::InternalError(format!(
                "Failed to refresh routing after delete_all: {}",
                e
            ))
        })?;

    let result = json!({
        "deleted": {
            "measurement": measurement_result.rows_affected(),
            "action": acceptance.affected_routes()
        },
        "request_id": acceptance.request_id(),
        "audit": crate::infra::application_control::completion_audit_response(
            acceptance.completion_audit()
        ),
        "retryable": acceptance.is_retryable()
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

/// Delete all routing for an instance
///
/// Removes all routing entries (measurement and action) for a specific instance.
#[utoipa::path(
    delete,
    path = "/api/routing/instances/{instance_name}",
    params(
        ("instance_name" = String, Path, description = "Instance name"),
        ("confirm" = Option<bool>, Query, description = "Confirmation flag (must be true)"),
        ("x-request-id" = Option<String>, Header, description = "Optional UUID audit correlation ID")
    ),
    responses(
        (status = 200, description = "Instance routing deleted", body = Value),
        (status = 403, description = "Missing/invalid Bearer credentials or actor lacks automation.routing.manage"),
        (status = 404, description = "Instance not found"),
        (status = 422, description = "Explicit confirmation is required"),
        (status = 503, description = "Mandatory audit, routing storage, or cache publication is unavailable")
    ),
    security(("bearer_auth" = [])),
    tag = "automation"
)]
pub async fn delete_instance_routing_handler(
    State(state): State<Arc<AppState>>,
    Path(instance_name): Path<String>,
    Query(params): Query<ConfirmQuery>,
    headers: HeaderMap,
) -> Result<Json<SuccessResponse<Value>>, AutomationError> {
    let instance_id = state
        .instance_manager
        .get_instance_id(&instance_name)
        .await
        .map_err(|_| AutomationError::InstanceNotFound(instance_name.clone()))?;
    let acceptance = crate::api::action_routing_boundary::apply(
        &state,
        &headers,
        params.confirm.unwrap_or(false),
        aether_ports::ActionRoutingMutation::delete_actions_for_instance(
            aether_domain::InstanceId::new(instance_id),
        ),
    )
    .await?;

    let measurement_result = sqlx::query("DELETE FROM measurement_routing WHERE instance_name = ?")
        .bind(&instance_name)
        .execute(&state.instance_manager.pool)
        .await
        .map_err(|e| {
            AutomationError::InternalError(format!("Failed to delete measurement routing: {}", e))
        })?;

    state
        .instance_manager
        .refresh_routing()
        .await
        .map_err(|e| {
            AutomationError::InternalError(format!(
                "Failed to refresh routing after delete instance routing: {}",
                e
            ))
        })?;

    let result = json!({
        "instance_name": instance_name,
        "deleted": {
            "measurement": measurement_result.rows_affected(),
            "action": acceptance.affected_routes()
        },
        "request_id": acceptance.request_id(),
        "audit": crate::infra::application_control::completion_audit_response(
            acceptance.completion_audit()
        ),
        "retryable": acceptance.is_retryable()
    });

    Ok(Json(SuccessResponse::new(result)))
}

/// Delete all routing for a channel
///
/// Removes all routing entries (uplink and downlink) for a specific channel.
#[utoipa::path(
    delete,
    path = "/api/routing/channels/{channel_id}",
    params(
        ("channel_id" = u32, Path, description = "Channel ID"),
        ("confirm" = Option<bool>, Query, description = "Confirmation flag (must be true)"),
        ("x-request-id" = Option<String>, Header, description = "Optional UUID audit correlation ID")
    ),
    responses(
        (status = 200, description = "Channel routing deleted", body = Value),
        (status = 403, description = "Missing/invalid Bearer credentials or actor lacks automation.routing.manage"),
        (status = 422, description = "Explicit confirmation is required"),
        (status = 503, description = "Mandatory audit, routing storage, or cache publication is unavailable")
    ),
    security(("bearer_auth" = [])),
    tag = "automation"
)]
pub async fn delete_channel_routing_handler(
    State(state): State<Arc<AppState>>,
    Path(channel_id): Path<u32>,
    Query(params): Query<ConfirmQuery>,
    headers: HeaderMap,
) -> Result<Json<SuccessResponse<Value>>, AutomationError> {
    let acceptance = crate::api::action_routing_boundary::apply(
        &state,
        &headers,
        params.confirm.unwrap_or(false),
        aether_ports::ActionRoutingMutation::delete_actions_for_channel(
            aether_domain::ChannelId::new(channel_id),
        ),
    )
    .await?;

    let uplink_result = sqlx::query("DELETE FROM measurement_routing WHERE channel_id = ?")
        .bind(channel_id)
        .execute(&state.instance_manager.pool)
        .await
        .map_err(|e| {
            AutomationError::InternalError(format!("Failed to delete uplink routing: {}", e))
        })?;

    state
        .instance_manager
        .refresh_routing()
        .await
        .map_err(|e| {
            AutomationError::InternalError(format!(
                "Failed to refresh routing after delete channel routing: {}",
                e
            ))
        })?;

    let result = json!({
        "channel_id": channel_id,
        "deleted": {
            "uplink": uplink_result.rows_affected(),
            "downlink": acceptance.affected_routes()
        },
        "request_id": acceptance.request_id(),
        "audit": crate::infra::application_control::completion_audit_response(
            acceptance.completion_audit()
        ),
        "retryable": acceptance.is_retryable()
    });

    Ok(Json(SuccessResponse::new(result)))
}
