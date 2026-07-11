//! Instance Management API Handlers
//!
//! Handles CRUD operations and synchronization for model instances.

#![allow(clippy::disallowed_methods)] // json! macro used in multiple functions

use crate::config::CreateInstanceRequest;
use axum::{
    extract::{Path, State},
    http::HeaderMap,
    response::Json,
};
use common::SuccessResponse;
use serde_json::json;
use std::sync::Arc;
use tracing::{error, info};

use crate::app_state::AppState;
use crate::dto::{ActionRequest, CreateInstanceDto, UpdateInstanceDto};
use crate::error::AutomationError;

/// Create a new model instance
///
/// Creates an instance from a product template with optional property overrides.
#[utoipa::path(
    post,
    path = "/api/instances",
    request_body = crate::dto::CreateInstanceDto,
    responses(
        (status = 200, description = "Instance created", body = serde_json::Value,
            example = json!({
                "instance": {
                    "instance_id": 1,
                    "instance_name": "pv_inverter_01",
                    "product_name": "pv_inverter",
                    "properties": {
                        "rated_power": 5000.0,
                        "manufacturer": "Huawei",
                        "model": "SUN2000-5KTL-L1"
                    },
                    "created_at": "2025-10-15T10:30:00Z",
                    "updated_at": "2025-10-15T10:30:00Z"
                }
            })
        )
    ),
    tag = "automation"
)]
pub async fn create_instance(
    State(state): State<Arc<AppState>>,
    Json(dto): Json<CreateInstanceDto>,
) -> Result<Json<SuccessResponse<serde_json::Value>>, AutomationError> {
    let req = CreateInstanceRequest {
        instance_id: dto.instance_id,
        instance_name: dto.instance_name,
        product_name: dto.product_name,
        parent_id: dto.parent_id,
        properties: dto.properties.unwrap_or_default(),
    };

    let instance = state.instance_manager.create_instance(req).await?;
    Ok(Json(SuccessResponse::new(json!({
        "instance": instance
    }))))
}

/// Update instance name and/or properties
///
/// Updates the instance_name and/or properties of an existing instance.
/// At least one field (instance_name or properties) must be provided.
#[utoipa::path(
    put,
    path = "/api/instances/{id}",
    params(
        ("id" = u16, Path, description = "Instance ID")
    ),
    request_body = UpdateInstanceDto,
    responses(
        (status = 200, description = "Instance updated successfully", body = serde_json::Value,
            example = json!({
                "instance": {
                    "instance_id": 1,
                    "instance_name": "pv_inverter_renamed",
                    "product_name": "pv_inverter",
                    "properties": {
                        "rated_power": 5000.0,
                        "manufacturer": "Huawei",
                        "model": "SUN2000-5KTL-L1"
                    },
                    "created_at": "2025-10-15T10:30:00Z",
                    "updated_at": "2025-10-20T14:25:00Z"
                }
            })
        ),
        (status = 400, description = "No fields to update"),
        (status = 404, description = "Instance not found"),
        (status = 409, description = "Instance name already exists"),
        (status = 500, description = "Database error")
    ),
    tag = "automation"
)]
pub async fn update_instance(
    State(state): State<Arc<AppState>>,
    Path(id): Path<u32>,
    Json(dto): Json<UpdateInstanceDto>,
) -> Result<Json<SuccessResponse<serde_json::Value>>, AutomationError> {
    // Validate: at least one field must be provided
    if dto.instance_name.is_none() && dto.properties.is_none() {
        return Err(AutomationError::InvalidData(
            "At least one field (instance_name or properties) must be provided".to_string(),
        ));
    }

    // Query current instance_name for local cache maintenance.
    let old_instance_name: String =
        match sqlx::query_scalar("SELECT instance_name FROM instances WHERE instance_id = ?")
            .bind(id as i32)
            .fetch_one(&state.instance_manager.pool)
            .await
        {
            Ok(name) => name,
            Err(_) => return Err(AutomationError::InstanceNotFound(id.to_string())),
        };

    // Determine the final instance name
    let new_instance_name = dto.instance_name.as_deref().unwrap_or(&old_instance_name);
    let is_renaming = dto.instance_name.is_some() && new_instance_name != old_instance_name;

    // Handle renaming
    if is_renaming {
        // Rename in SQLite (includes transaction)
        state
            .instance_manager
            .rename_instance(id, new_instance_name)
            .await?;
    }

    // Handle properties update.
    //
    // Semantics: this endpoint replaces the property map atomically — keys
    // omitted from `dto.properties` are removed. (For single-point edits use
    // PUT /api/instances/{id}/properties/{property_id}, which does not touch
    // sibling properties.)
    if let Some(ref properties) = dto.properties {
        let product_name: String =
            match sqlx::query_scalar("SELECT product_name FROM instances WHERE instance_id = ?")
                .bind(id as i64)
                .fetch_one(&state.instance_manager.pool)
                .await
            {
                Ok(name) => name,
                Err(_) => return Err(AutomationError::InstanceNotFound(id.to_string())),
            };

        let mut tx =
            state.instance_manager.pool.begin().await.map_err(|e| {
                AutomationError::DatabaseError(format!("Failed to begin tx: {}", e))
            })?;

        // Wipe existing rows so omitted keys disappear (replace, not merge).
        if let Err(e) = sqlx::query("DELETE FROM instance_properties WHERE instance_id = ?")
            .bind(id as i64)
            .execute(&mut *tx)
            .await
        {
            error!("Failed to clear properties for instance {}: {}", id, e);
            let _ = tx.rollback().await;
            return Err(AutomationError::DatabaseError(format!(
                "Database update failed: {}",
                e
            )));
        }

        if let Err(e) = state
            .instance_manager
            .write_properties_tx(&mut tx, id, &product_name, properties)
            .await
        {
            error!("Failed to write properties for instance {}: {}", id, e);
            let _ = tx.rollback().await;
            return Err(AutomationError::InvalidData(format!(
                "Failed to write properties: {}",
                e
            )));
        }

        if let Err(e) =
            sqlx::query("UPDATE instances SET updated_at = CURRENT_TIMESTAMP WHERE instance_id = ?")
                .bind(id as i64)
                .execute(&mut *tx)
                .await
        {
            error!("Failed to bump updated_at for instance {}: {}", id, e);
            let _ = tx.rollback().await;
            return Err(AutomationError::DatabaseError(format!(
                "Database update failed: {}",
                e
            )));
        }

        if let Err(e) = tx.commit().await {
            error!("Failed to commit properties tx for instance {}: {}", id, e);
            return Err(AutomationError::DatabaseError(format!(
                "Database commit failed: {}",
                e
            )));
        }
    }

    info!(
        "Instance {} updated successfully (renamed: {}, properties: {})",
        id,
        is_renaming,
        dto.properties.is_some()
    );

    // Query and return updated instance
    match state.instance_manager.get_instance(id).await {
        Ok(instance) => Ok(Json(SuccessResponse::new(json!({
            "instance": instance
        })))),
        Err(e) => {
            error!("Failed to query updated instance {}: {}", id, e);
            // Update succeeded but query failed - return id as fallback
            Ok(Json(SuccessResponse::new(json!({
                "instance_id": id,
                "instance_name": new_instance_name,
                "message": "Instance updated successfully but failed to retrieve details"
            }))))
        },
    }
}

/// Delete an instance
///
/// Removes an instance from SQLite and process-local derived state.
#[utoipa::path(
    delete,
    path = "/api/instances/{id}",
    params(
        ("id" = u16, Path, description = "Instance ID")
    ),
    responses(
        (status = 200, description = "Instance deleted", body = serde_json::Value,
            example = json!({
                "message": "Instance 1 deleted"
            })
        )
    ),
    tag = "automation"
)]
pub async fn delete_instance(
    State(state): State<Arc<AppState>>,
    Path(id): Path<u32>,
) -> Result<Json<SuccessResponse<serde_json::Value>>, AutomationError> {
    state.instance_manager.delete_instance(id).await?;
    Ok(Json(SuccessResponse::new(json!({
        "message": format!("Instance {} deleted", id)
    }))))
}

/// Reload instances from database
///
/// Rebuilds process-local caches from authoritative SQLite configuration.
///
pub async fn reload_instances_from_db(
    State(state): State<Arc<AppState>>,
) -> Result<Json<SuccessResponse<serde_json::Value>>, AutomationError> {
    // Use unified ReloadableService interface for incremental sync
    use common::ReloadableService;
    match ReloadableService::reload_from_database(
        &*state.instance_manager,
        &state.instance_manager.pool,
    )
    .await
    {
        Ok(result) => {
            info!(
                "Instances reloaded: {} added, {} updated, {} removed, {} errors",
                result.added.len(),
                result.updated.len(),
                result.removed.len(),
                result.errors.len()
            );
            Ok(Json(SuccessResponse::new(json!({
                "message": "Instances reloaded successfully",
                "result": result
            }))))
        },
        Err(e) => {
            error!("Failed to reload instances: {}", e);
            Err(AutomationError::InternalError(format!(
                "Failed to reload instances: {}",
                e
            )))
        },
    }
}

// ============================================================================
// Action Execution
// ============================================================================

/// Execute an action on an instance
///
/// Triggers an action point with the specified value.
#[utoipa::path(
    post,
    path = "/api/instances/{id}/action",
    params(
        ("id" = u16, Path, description = "Instance ID")
    ),
    request_body = crate::dto::ActionRequest,
    responses(
        (status = 200, description = "Action executed", body = serde_json::Value,
            example = json!({
                "message": "Action executed"
            })
        )
    ),
    tag = "automation"
)]
pub async fn execute_instance_action(
    State(state): State<Arc<AppState>>,
    Path(id): Path<u32>,
    headers: HeaderMap,
    Json(req): Json<ActionRequest>,
) -> Result<Json<SuccessResponse<serde_json::Value>>, AutomationError> {
    let point_id = req.point_id.parse::<u32>().map_err(|_| {
        AutomationError::InvalidData(format!("action point_id must be numeric: {}", req.point_id))
    })?;
    let timestamp_ms = chrono::Utc::now().timestamp_millis().max(0) as u64;
    let invocation = crate::infra::application_control::command_invocation_from_headers(
        &state.control_authenticator,
        &headers,
        req.confirmed,
        aether_domain::TimestampMs::new(timestamp_ms),
    );
    let target = aether_domain::PointAddress::new(
        aether_domain::InstanceId::new(id),
        aether_domain::PointKind::Action,
        aether_domain::PointId::new(point_id),
    );
    let receipt = state
        .control_application
        .write_point(
            invocation.context(),
            invocation.command_id(),
            target,
            req.value,
        )
        .await
        .map_err(AutomationError::from)?;
    Ok(Json(SuccessResponse::new(json!({
        "message": "Action executed",
        "instance_id": id,
        "point_id": req.point_id,
        "value": req.value,
        "request_id": invocation.context().request_id(),
        "completed_at_ms": receipt.completed_at().get()
    }))))
}
