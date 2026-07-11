//! Single Property API Handlers
//!
//! Single-point endpoints for property values, mirroring the routing-handler
//! shape used by measurements/actions. Property values are stored in the
//! `instance_properties` table (one row per `(instance_id, property_id)`);
//! these endpoints write/delete a single row at a time, leaving sibling
//! properties untouched — unlike `PUT /api/instances/{id}` which replaces
//! the whole property map.

#![allow(clippy::disallowed_methods)] // json! macro used in utoipa attribute examples

use axum::{
    extract::{Path, State},
    response::Json,
};
use common::SuccessResponse;
use std::sync::Arc;

use crate::app_state::AppState;
use crate::dto::{InstancePropertyPoint, UpsertPropertyRequest};
use crate::error::AutomationError;

/// Upsert a single property value (PUT).
///
/// Validates `property_id` against the instance's product PropertyTemplate.
/// Sibling property values are untouched. Returns the updated property entry.
#[utoipa::path(
    put,
    path = "/api/instances/{id}/properties/{property_id}",
    params(
        ("id" = u32, Path, description = "Instance ID"),
        ("property_id" = i32, Path, description = "Property ID (declared by the product template)")
    ),
    request_body = UpsertPropertyRequest,
    responses(
        (status = 200, description = "Property upserted", body = InstancePropertyPoint,
            example = json!({
                "property_id": 1,
                "name": "Max Power",
                "unit": "kw",
                "description": null,
                "value": 6000.0
            })
        ),
        (status = 400, description = "property_id not declared by product template"),
        (status = 404, description = "Instance not found"),
        (status = 500, description = "Database error")
    ),
    tag = "automation"
)]
pub async fn upsert_property(
    State(state): State<Arc<AppState>>,
    Path((id, property_id)): Path<(u32, i32)>,
    Json(request): Json<UpsertPropertyRequest>,
) -> Result<Json<SuccessResponse<InstancePropertyPoint>>, AutomationError> {
    let updated = state
        .instance_manager
        .upsert_single_property(id, property_id, request.value)
        .await?;
    Ok(Json(SuccessResponse::new(updated)))
}

/// Delete a single property value (DELETE).
///
/// Removes the row for `(instance_id, property_id)` if present. Returns the
/// template metadata with `value` absent so the frontend can render the
/// post-delete state directly. 400 if `property_id` is not in the product
/// template, 404 if the instance does not exist.
#[utoipa::path(
    delete,
    path = "/api/instances/{id}/properties/{property_id}",
    params(
        ("id" = u32, Path, description = "Instance ID"),
        ("property_id" = i32, Path, description = "Property ID")
    ),
    responses(
        (status = 200, description = "Property deleted (or already absent)", body = InstancePropertyPoint,
            example = json!({
                "property_id": 1,
                "name": "Max Power",
                "unit": "kw",
                "description": null
            })
        ),
        (status = 400, description = "property_id not declared by product template"),
        (status = 404, description = "Instance not found"),
        (status = 500, description = "Database error")
    ),
    tag = "automation"
)]
pub async fn delete_property(
    State(state): State<Arc<AppState>>,
    Path((id, property_id)): Path<(u32, i32)>,
) -> Result<Json<SuccessResponse<InstancePropertyPoint>>, AutomationError> {
    let updated = state
        .instance_manager
        .delete_single_property(id, property_id)
        .await?;
    Ok(Json(SuccessResponse::new(updated)))
}
