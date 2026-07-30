//! Global Routing Management API Handlers
//!
//! This module provides API handlers for managing routing configurations at
//! the global level, including queries across all instances and channels.

use axum::{
    extract::{Path, State},
    response::Json,
};
use common::SuccessResponse;
use std::sync::Arc;

use crate::api::dto::{
    AllRoutingResponseDto, ChannelRoutingResponseDto, RoutingListEntryDto, RoutingTotalsDto,
};
use crate::app_state::AppState;
use crate::error::AutomationError;
use crate::routing_loader::RoutingScope;

/// Get all routing configurations (measurement and action)
///
/// Returns all routing entries in the system, categorized by type.
#[utoipa::path(
    get,
    path = "/api/routing",
    responses(
        (status = 200, description = "All routing configurations", body = common::SuccessResponse<crate::api::dto::AllRoutingResponseDto>,
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
) -> Result<Json<SuccessResponse<AllRoutingResponseDto>>, AutomationError> {
    let snapshot = state
        .instance_manager
        .routing_snapshot(RoutingScope::All)
        .await
        .map_err(|error| {
            AutomationError::InternalError(format!("Failed to query routing: {error}"))
        })?;
    let revision = snapshot.revision();
    let (measurements, actions) = snapshot.into_parts();
    let measurement_routing = measurements
        .into_iter()
        .map(|route| RoutingListEntryDto::measurement(route, true))
        .collect::<Vec<_>>();
    let action_routing = actions
        .into_iter()
        .map(|route| RoutingListEntryDto::action(route, true))
        .collect::<Vec<_>>();
    let total = RoutingTotalsDto {
        measurement: measurement_routing.len(),
        action: action_routing.len(),
    };

    Ok(Json(SuccessResponse::new(AllRoutingResponseDto {
        measurement_routing,
        action_routing,
        total,
        logical_routing_revision: revision,
    })))
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
        (status = 200, description = "Channel routing entries", body = common::SuccessResponse<crate::api::dto::ChannelRoutingResponseDto>,
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
) -> Result<Json<SuccessResponse<ChannelRoutingResponseDto>>, AutomationError> {
    let snapshot = state
        .instance_manager
        .routing_snapshot(RoutingScope::Channel(channel_id))
        .await
        .map_err(|error| {
            AutomationError::InternalError(format!("Failed to query channel routing: {error}"))
        })?;
    let revision = snapshot.revision();
    let (measurements, actions) = snapshot.into_parts();

    Ok(Json(SuccessResponse::new(ChannelRoutingResponseDto {
        channel_id,
        uplink: measurements
            .into_iter()
            .map(|route| RoutingListEntryDto::measurement(route, false))
            .collect(),
        downlink: actions
            .into_iter()
            .map(|route| RoutingListEntryDto::action(route, false))
            .collect(),
        logical_routing_revision: revision,
    })))
}
