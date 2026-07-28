//! Instance Query API Handlers
//!
//! Provides read-only endpoints for querying instance information and data.

#![allow(clippy::disallowed_methods)] // json! macro used in multiple functions

use axum::{
    extract::{Path, Query, State},
    response::Json,
};
use common::SuccessResponse;
use serde::Deserialize;
use serde_json::json;
use std::sync::Arc;

use crate::app_state::AppState;
use crate::dto::{DataTypeQuery, InstancePointsResponse};
use crate::error::AutomationError;

/// Pagination query parameters for listing instances
#[derive(Debug, Deserialize)]
pub struct PaginationQuery {
    /// Optional product filter
    pub product_name: Option<String>,
    #[serde(default = "default_page")]
    pub page: u32,
    #[serde(default = "default_page_size")]
    pub page_size: u32,
}

fn default_page() -> u32 {
    1
}

fn default_page_size() -> u32 {
    20
}

/// List instances with pagination (includes product-model summary per instance).
///
/// Optionally filter by `product_name` to narrow to a specific device type.
/// Each record contains `instance_id`, `instance_name`, `product_name`,
/// `parent_id`, and `properties` JSON. Does **not** include live measurement
/// values — for runtime data use `/api/instances/{id}/data`. Intended for the
/// instance-list view where a lightweight response is preferred.
#[utoipa::path(
    get,
    path = "/api/instances",
    params(
        ("product_name" = Option<String>, Query, description = "Optional product filter"),
        ("page" = Option<u32>, Query, description = "Page number (default: 1)"),
        ("page_size" = Option<u32>, Query, description = "Items per page (default: 20, max: 100)")
    ),
    responses(
        (status = 200, description = "List instances with pagination", body = serde_json::Value,
            example = json!({
                "total": 10,
                "page": 1,
                "page_size": 20,
                "list": [
                    {
                        "instance_id": 1,
                        "instance_name": "pump_01",
                        "product_name": "pump",
                        "properties": {
                            "max_flow_lpm": 500.0,
                            "manufacturer": "Example Corp"
                        }
                    },
                    {
                        "instance_id": 2,
                        "instance_name": "conveyor_01",
                        "product_name": "conveyor",
                        "properties": {
                            "max_speed_mps": 2.5,
                            "length_m": 12.0
                        }
                    }
                ]
            })
        )
    ),
    tag = "automation"
)]
pub async fn list_instances(
    State(state): State<Arc<AppState>>,
    Query(query): Query<PaginationQuery>,
) -> Result<Json<SuccessResponse<serde_json::Value>>, AutomationError> {
    let product_name = query.product_name.as_deref();
    let page = query.page.max(1); // Ensure page is at least 1
    let page_size = query.page_size.clamp(1, 100); // Limit to reasonable range

    let result = state
        .instance_manager
        .list_instances_paginated(product_name, page, page_size)
        .await;

    match result {
        Ok((total, instances)) => Ok(Json(SuccessResponse::new(json!({
            "total": total,
            "page": page,
            "page_size": page_size,
            "list": instances
        })))),
        Err(e) => Err(AutomationError::InternalError(format!(
            "Failed to list instances: {}",
            e
        ))),
    }
}

/// Get product-model details for a single instance.
///
/// Returns the full instance definition: base fields, properties, measurement
/// point list, and action point list. This is the **product-model** view (structure
/// definition) and contains no live values; for runtime data use
/// `/api/instances/{id}/data`. Returns 404 when `instance_id` does not exist.
#[utoipa::path(
    get,
    path = "/api/instances/{id}",
    params(
        ("id" = u32, Path, description = "Instance ID")
    ),
    responses(
        (status = 200, description = "Instance details", body = serde_json::Value,
            example = json!({
                "instance": {
                    "instance_id": 1,
                    "instance_name": "pump_01",
                    "product_name": "pump",
                    "properties": {
                        "max_flow_lpm": 500.0,
                        "manufacturer": "Example Corp",
                        "model": "P-500",
                        "process_zone": "line_a"
                    },
                    "created_at": "2025-10-15T10:30:00Z",
                    "updated_at": "2025-10-15T14:25:00Z"
                }
            })
        )
    ),
    tag = "automation"
)]
pub async fn get_instance(
    State(state): State<Arc<AppState>>,
    Path(id): Path<u32>,
) -> Result<Json<SuccessResponse<serde_json::Value>>, AutomationError> {
    match state.instance_manager.get_instance(id).await {
        Ok(instance) => Ok(Json(SuccessResponse::new(json!({
            "instance": instance
        })))),
        Err(e) => {
            if e.to_string().contains("not found") {
                Err(AutomationError::InstanceNotFound(id.to_string()))
            } else {
                Err(AutomationError::InternalError(format!(
                    "Failed to get instance: {}",
                    e
                )))
            }
        },
    }
}

/// Get real-time data for an instance
///
/// Returns current measurement and action values from SHM plus properties
/// from SQLite.
#[utoipa::path(
    get,
    path = "/api/instances/{id}/data",
    params(
        ("id" = u32, Path, description = "Instance ID"),
        ("type" = Option<String>, Query, description = "Optional data type filter (measurement/action)")
    ),
    responses(
        (status = 200, description = "Instance data", body = serde_json::Value,
            example = json!({
                "measurements": {
                    "101": "650.5",
                    "102": "12.3",
                    "103": "4500.0"
                },
                "actions": {
                    "201": "4500.0"
                },
                "properties": {
                    "max_flow_lpm": 500.0,
                    "manufacturer": "Example Corp"
                }
            })
        )
    ),
    tag = "automation"
)]
pub async fn get_instance_data(
    State(state): State<Arc<AppState>>,
    Path(id): Path<u32>,
    Query(query): Query<DataTypeQuery>,
) -> Result<Json<SuccessResponse<serde_json::Value>>, AutomationError> {
    match state
        .instance_manager
        .get_instance_data(id, query.data_type.as_deref())
        .await
    {
        Ok(data) => Ok(Json(SuccessResponse::new(data))),
        Err(e) => {
            let error_msg = e.to_string();
            if error_msg.contains("not found") {
                Err(AutomationError::InstanceNotFound(id.to_string()))
            } else {
                Err(AutomationError::InternalError(format!(
                    "Failed to get instance data: {}",
                    e
                )))
            }
        },
    }
}

/// Get point definitions with routing for an instance
///
/// Returns measurement, action, and property points. Measurements and actions carry
/// their routing configurations; properties carry the per-instance value (no routing).
#[utoipa::path(
    get,
    path = "/api/instances/{id}/points",
    params(
        ("id" = u32, Path, description = "Instance ID")
    ),
    responses(
        (status = 200, description = "Instance points with routing/values",
            body = InstancePointsResponse,
            example = json!({
                "instance_name": "pump_01",
                "measurements": [
                    {
                        "measurement_id": 1,
                        "name": "DC Voltage",
                        "unit": "V",
                        "description": "DC input voltage",
                        "routing": {
                            "channel_id": 1001,
                            "channel_type": "T",
                            "channel_point_id": 101,
                            "enabled": true
                        }
                    },
                    {
                        "measurement_id": 2,
                        "name": "DC Current",
                        "unit": "A",
                        "description": "DC input current"
                    }
                ],
                "actions": [
                    {
                        "action_id": 1,
                        "name": "Power Setpoint",
                        "unit": "kW",
                        "description": "Active power setpoint",
                        "routing": {
                            "channel_id": 1001,
                            "channel_type": "A",
                            "channel_point_id": 201,
                            "enabled": true
                        }
                    }
                ],
                "properties": [
                    {
                        "property_id": 1,
                        "name": "rated_power",
                        "unit": "kW",
                        "description": "Rated active power",
                        "value": 5000.0
                    },
                    {
                        "property_id": 2,
                        "name": "manufacturer",
                        "description": "Device manufacturer"
                    }
                ]
            })
        ),
        (status = 404, description = "Instance not found"),
        (status = 500, description = "Internal error")
    ),
    tag = "automation"
)]
pub async fn get_instance_points(
    State(state): State<Arc<AppState>>,
    Path(id): Path<u32>,
) -> Result<Json<SuccessResponse<InstancePointsResponse>>, AutomationError> {
    // Query instance_name for response (InstancePointsResponse still needs it for now)
    let instance = state.instance_manager.get_instance(id).await.map_err(|e| {
        if e.to_string().contains("not found") {
            AutomationError::InstanceNotFound(id.to_string())
        } else {
            AutomationError::InternalError(format!("Failed to get instance: {}", e))
        }
    })?;

    match state.instance_manager.load_instance_points(id).await {
        Ok((measurements, actions, properties)) => {
            let response = InstancePointsResponse {
                instance_name: instance.instance_name().to_string(),
                measurements,
                actions,
                properties,
            };
            Ok(Json(SuccessResponse::new(response)))
        },
        Err(e) => {
            let error_msg = e.to_string();
            if error_msg.contains("not found") {
                Err(AutomationError::InstanceNotFound(id.to_string()))
            } else {
                Err(AutomationError::InternalError(format!(
                    "Failed to get instance points: {}",
                    e
                )))
            }
        },
    }
}

// ============================================================================
// Topology Query Handlers
// ============================================================================

/// Get direct child instances of a given parent.
///
/// One-level descent on the `parent_id` foreign key — does **not**
/// recurse. Returns each child's full instance row. For deep
/// hierarchies (Facility → ProcessLine → Pump → Motor) call this repeatedly
/// or use a separate tree-walk endpoint.
#[cfg_attr(feature = "openapi", utoipa::path(
    get,
    path = "/api/instances/{id}/children",
    params(("id" = u32, Path, description = "Parent instance ID")),
    responses(
        (status = 200, description = "Child instances", body = serde_json::Value,
            example = json!({
                "list": [
                    {"instance_id": 2, "instance_name": "line_01", "product_name": "ProcessLine", "parent_id": 1}
                ]
            })
        )
    ),
    tag = "automation"
))]
pub async fn get_instance_children(
    State(state): State<Arc<AppState>>,
    Path(id): Path<u32>,
) -> Result<Json<SuccessResponse<serde_json::Value>>, AutomationError> {
    match state.instance_manager.get_children(id).await {
        Ok(children) => Ok(Json(SuccessResponse::new(json!({
            "list": children
        })))),
        Err(e) => Err(AutomationError::InternalError(format!(
            "Failed to get children: {}",
            e
        ))),
    }
}

/// Get full topology tree (all instances with parent relationships)
///
/// Returns a flat list of topology nodes ordered for tree reconstruction:
/// root nodes first, then children in parent_id order.
///
#[cfg_attr(feature = "openapi", utoipa::path(
    get,
    path = "/api/topology",
    responses(
        (status = 200, description = "Full topology tree", body = serde_json::Value,
            example = json!({
                "tree": [
                    {"instance_id": 1, "instance_name": "facility_01", "product_name": "Facility"},
                    {"instance_id": 2, "instance_name": "line_01", "product_name": "ProcessLine", "parent_id": 1},
                    {"instance_id": 3, "instance_name": "pump_01", "product_name": "Pump", "parent_id": 2}
                ]
            })
        )
    ),
    tag = "automation"
))]
pub async fn get_topology_tree(
    State(state): State<Arc<AppState>>,
) -> Result<Json<SuccessResponse<serde_json::Value>>, AutomationError> {
    match state.instance_manager.get_topology_tree().await {
        Ok(tree) => Ok(Json(SuccessResponse::new(json!({
            "tree": tree
        })))),
        Err(e) => Err(AutomationError::InternalError(format!(
            "Failed to get topology tree: {}",
            e
        ))),
    }
}
