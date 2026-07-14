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
    extract::{Path, Query, State},
    http::HeaderMap,
    response::Json,
};
use common::SuccessResponse;
use std::sync::Arc;

use crate::app_state::AppState;
use crate::dto::{InstanceMutationConfirmation, InstancePropertyPoint, UpsertPropertyRequest};
use crate::error::AutomationError;
use crate::instance_configuration::{
    InstanceConfigurationMutation, InstanceConfigurationPayload, InstanceConfigurationRevision,
};

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
    security(
        ("bearer_auth" = [])
    ),
    tag = "automation"
)]
pub async fn upsert_property(
    State(state): State<Arc<AppState>>,
    Path((id, property_id)): Path<(u32, i32)>,
    headers: HeaderMap,
    Json(request): Json<UpsertPropertyRequest>,
) -> Result<Json<SuccessResponse<serde_json::Value>>, AutomationError> {
    let acceptance = super::instance_management_handlers::apply_instance_mutation(
        &state,
        &headers,
        request.confirmed,
        InstanceConfigurationMutation::UpsertProperty {
            instance_id: id,
            property_id,
            value: request.value,
            expected_revision: InstanceConfigurationRevision::new(request.expected_revision),
        },
    )
    .await?;
    let InstanceConfigurationPayload::Property(updated) = acceptance.payload() else {
        return Err(AutomationError::InternalError(
            "property upsert returned an unexpected payload".to_string(),
        ));
    };
    Ok(Json(SuccessResponse::new(serde_json::json!({
        "property": updated,
        "governance": super::instance_management_handlers::governance_response(&acceptance)
    }))))
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
    security(
        ("bearer_auth" = [])
    ),
    tag = "automation"
)]
pub async fn delete_property(
    State(state): State<Arc<AppState>>,
    Path((id, property_id)): Path<(u32, i32)>,
    headers: HeaderMap,
    Query(request): Query<InstanceMutationConfirmation>,
) -> Result<Json<SuccessResponse<serde_json::Value>>, AutomationError> {
    let acceptance = super::instance_management_handlers::apply_instance_mutation(
        &state,
        &headers,
        request.confirmed,
        InstanceConfigurationMutation::DeleteProperty {
            instance_id: id,
            property_id,
            expected_revision: InstanceConfigurationRevision::new(request.expected_revision),
        },
    )
    .await?;
    let InstanceConfigurationPayload::Property(updated) = acceptance.payload() else {
        return Err(AutomationError::InternalError(
            "property deletion returned an unexpected payload".to_string(),
        ));
    };
    Ok(Json(SuccessResponse::new(serde_json::json!({
        "property": updated,
        "governance": super::instance_management_handlers::governance_response(&acceptance)
    }))))
}
