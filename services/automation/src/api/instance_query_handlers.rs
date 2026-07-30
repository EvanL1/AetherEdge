//! Instance Query API Handlers
//!
//! Provides read-only endpoints for querying instance information and data.

#![allow(clippy::disallowed_methods)] // json! macro used in multiple functions

use axum::{
    extract::{Path, Query, State, rejection::QueryRejection},
    response::Json,
};
use common::SuccessResponse;
use serde::Deserialize;
use serde_json::json;
use std::collections::BTreeSet;
use std::sync::Arc;

use crate::api::dto::{
    DataTypeQuery, InstanceDataResponseDto, InstanceDetailResponseDto, InstanceListResponseDto,
    InstancePickerItemDto, InstancePickerResponseDto, InstancePointsResponse,
    InstanceSearchResponseDto, InstanceSummaryDto,
};
use crate::app_state::AppState;
use crate::error::AutomationError;

/// Pagination query parameters for listing instances
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
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
        (status = 200, description = "List instances with pagination", body = common::SuccessResponse<InstanceListResponseDto>,
            example = json!({
                "success": true,
                "data": {
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
                        }
                    ]
                }
            })
        ),
        (status = 400, description = "Malformed query"),
        (status = 422, description = "Invalid pagination or product filter")
    ),
    tag = "automation"
)]
pub async fn list_instances(
    State(state): State<Arc<AppState>>,
    query: Result<Query<PaginationQuery>, QueryRejection>,
) -> Result<Json<SuccessResponse<InstanceListResponseDto>>, AutomationError> {
    let Query(query) = query
        .map_err(|_| AutomationError::InvalidData("malformed instance list query".to_string()))?;
    if query.page == 0 {
        return Err(AutomationError::InvalidData(
            "page must be at least 1".to_string(),
        ));
    }
    if !(1..=100).contains(&query.page_size) {
        return Err(AutomationError::InvalidData(
            "page_size must be between 1 and 100".to_string(),
        ));
    }
    if let Some(product_name) = query.product_name.as_deref()
        && (product_name.is_empty() || product_name.chars().count() > 128)
    {
        return Err(AutomationError::InvalidData(
            "product_name must contain between 1 and 128 characters".to_string(),
        ));
    }

    let product_name = query.product_name.as_deref();
    let (total, instances) = state
        .instance_manager
        .list_instances_paginated(product_name, query.page, query.page_size)
        .await
        .map_err(|error| {
            AutomationError::InternalError(format!("Failed to list instances: {error}"))
        })?;
    Ok(Json(SuccessResponse::new(InstanceListResponseDto {
        total,
        page: query.page,
        page_size: query.page_size,
        list: instances
            .into_iter()
            .map(InstanceSummaryDto::from)
            .collect(),
    })))
}

const DEFAULT_SEARCH_LIMIT: u32 = 100;
const MAX_SEARCH_LIMIT: u32 = 200;
const MAX_SEARCH_IDS: usize = 256;
const MAX_SEARCH_KEYWORD_CHARS: usize = 128;

const fn default_search_limit() -> u32 {
    DEFAULT_SEARCH_LIMIT
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InstanceSearchQuery {
    #[serde(default)]
    keyword: String,
    ids: Option<String>,
    #[serde(default = "default_search_limit")]
    limit: u32,
}

fn parse_search_ids(raw: Option<&str>) -> Result<Vec<u32>, AutomationError> {
    let Some(raw) = raw else {
        return Ok(Vec::new());
    };
    if raw.is_empty() {
        return Err(AutomationError::InvalidData(
            "ids must contain at least one instance ID".to_string(),
        ));
    }

    let mut ids = BTreeSet::new();
    for value in raw.split(',') {
        if value.is_empty() || value.trim() != value {
            return Err(AutomationError::InvalidData(
                "ids must be comma-separated unsigned integers".to_string(),
            ));
        }
        let instance_id = value.parse::<u32>().map_err(|_| {
            AutomationError::InvalidData(
                "ids must be comma-separated unsigned integers".to_string(),
            )
        })?;
        ids.insert(instance_id);
        if ids.len() > MAX_SEARCH_IDS {
            return Err(AutomationError::InvalidData(format!(
                "ids supports at most {MAX_SEARCH_IDS} unique values"
            )));
        }
    }
    Ok(ids.into_iter().collect())
}

/// Search commissioned instances with a bounded typed query.
#[utoipa::path(
    get,
    path = "/api/instances/search",
    params(
        ("keyword" = Option<String>, Query, description = "Optional case-insensitive name substring"),
        ("ids" = Option<String>, Query, description = "Optional comma-separated instance IDs"),
        ("limit" = Option<u32>, Query, description = "Maximum results (default 100, max 200)")
    ),
    responses(
        (status = 200, description = "Matching instances", body = common::SuccessResponse<InstanceSearchResponseDto>,
            example = json!({
                "success": true,
                "data": {
                    "count": 1,
                    "limit": 100,
                    "list": [{
                        "instance_id": 1,
                        "instance_name": "pump_01",
                        "product_name": "pump",
                        "parent_id": null,
                        "properties": {}
                    }]
                }
            })
        ),
        (status = 400, description = "Malformed query"),
        (status = 422, description = "Invalid IDs, keyword, or limit")
    ),
    tag = "automation"
)]
pub async fn search_instances(
    State(state): State<Arc<AppState>>,
    query: Result<Query<InstanceSearchQuery>, QueryRejection>,
) -> Result<Json<SuccessResponse<InstanceSearchResponseDto>>, AutomationError> {
    let Query(query) = query
        .map_err(|_| AutomationError::InvalidData("malformed instance search query".to_string()))?;
    if query.keyword.chars().count() > MAX_SEARCH_KEYWORD_CHARS {
        return Err(AutomationError::InvalidData(format!(
            "keyword supports at most {MAX_SEARCH_KEYWORD_CHARS} characters"
        )));
    }
    if !(1..=MAX_SEARCH_LIMIT).contains(&query.limit) {
        return Err(AutomationError::InvalidData(format!(
            "limit must be between 1 and {MAX_SEARCH_LIMIT}"
        )));
    }
    let ids = parse_search_ids(query.ids.as_deref())?;
    let instances = state
        .instance_manager
        .find_instances(&query.keyword, &ids, query.limit)
        .await
        .map_err(|error| {
            AutomationError::InternalError(format!("Failed to search instances: {error}"))
        })?;
    let list: Vec<_> = instances
        .into_iter()
        .map(InstanceSummaryDto::from)
        .collect();
    Ok(Json(SuccessResponse::new(InstanceSearchResponseDto {
        count: list.len(),
        limit: query.limit,
        list,
    })))
}

/// Minimal instance list (id + name only, no pagination).
///
/// For dropdown menus, routing-bind pickers, and other "pick an instance"
/// scenarios. Returns all instances in one shot with only two fields,
/// minimising response size. For full details use the paginated endpoint.
#[utoipa::path(
    get,
    path = "/api/instances/list",
    responses(
        (status = 200, description = "Instance list", body = common::SuccessResponse<InstancePickerResponseDto>,
            example = json!({
                "success": true,
                "data": {
                    "list": [
                        {"id": 1, "name": "pump_01"},
                        {"id": 2, "name": "conveyor_01"}
                    ]
                }
            })
        )
    ),
    tag = "automation"
)]
pub async fn list_instances_slim(
    State(state): State<Arc<AppState>>,
) -> Result<Json<SuccessResponse<InstancePickerResponseDto>>, AutomationError> {
    let list = state
        .instance_manager
        .list_instance_identities()
        .await
        .map_err(|error| {
            AutomationError::InternalError(format!("Failed to list instances: {error}"))
        })?
        .into_iter()
        .map(|(id, name)| InstancePickerItemDto { id, name })
        .collect();

    Ok(Json(SuccessResponse::new(InstancePickerResponseDto {
        list,
    })))
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
        (status = 200, description = "Instance details", body = common::SuccessResponse<InstanceDetailResponseDto>,
            example = json!({
                "success": true,
                "data": {
                    "instance": {
                        "instance_id": 1,
                        "instance_name": "pump_01",
                        "product_name": "pump",
                        "properties": {
                            "max_flow_lpm": 500.0,
                            "manufacturer": "Example Corp"
                        }
                    }
                }
            })
        ),
        (status = 404, description = "Instance not found")
    ),
    tag = "automation"
)]
pub async fn get_instance(
    State(state): State<Arc<AppState>>,
    Path(id): Path<u32>,
) -> Result<Json<SuccessResponse<InstanceDetailResponseDto>>, AutomationError> {
    match state.instance_manager.get_instance(id).await {
        Ok(instance) => Ok(Json(SuccessResponse::new(InstanceDetailResponseDto {
            instance: InstanceSummaryDto::from(instance),
        }))),
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
/// Returns current measurement and action values from the authoritative SHM
/// generation. A plane filter returns only that plane's point-value map.
#[utoipa::path(
    get,
    path = "/api/instances/{id}/data",
    params(
        ("id" = u32, Path, description = "Instance ID"),
        ("type" = Option<String>, Query, description = "Optional data type filter (measurement/action)")
    ),
    responses(
        (status = 200, description = "Instance data", body = common::SuccessResponse<InstanceDataResponseDto>,
            example = json!({
                "success": true,
                "data": {
                    "measurements": {
                        "101": {"value": 650.5, "timestamp_ms": 1720000000000_u64}
                    },
                    "actions": {
                        "201": {"value": 4500.0, "timestamp_ms": 1720000000000_u64}
                    }
                }
            })
        ),
        (status = 400, description = "Malformed query"),
        (status = 404, description = "Instance not found"),
        (status = 422, description = "Unknown data plane")
    ),
    tag = "automation"
)]
pub async fn get_instance_data(
    State(state): State<Arc<AppState>>,
    Path(id): Path<u32>,
    query: Result<Query<DataTypeQuery>, QueryRejection>,
) -> Result<Json<SuccessResponse<InstanceDataResponseDto>>, AutomationError> {
    let Query(query) = query
        .map_err(|_| AutomationError::InvalidData("malformed instance data query".to_string()))?;
    match state
        .instance_manager
        .get_instance_data(id, query.data_type.map(Into::into))
        .await
    {
        Ok(data) => Ok(Json(SuccessResponse::new(InstanceDataResponseDto::from(
            data,
        )))),
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
                "logical_routing_revision": 7,
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
    match state.instance_manager.load_instance_points(id).await {
        Ok(response) => Ok(Json(SuccessResponse::new(InstancePointsResponse::from(
            response,
        )))),
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
