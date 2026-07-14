//! Instance Management API Handlers
//!
//! Handles CRUD operations and synchronization for model instances.

#![allow(clippy::disallowed_methods)] // json! macro used in multiple functions

use crate::config::CreateInstanceRequest;
use axum::{
    extract::{Path, Query, State},
    http::HeaderMap,
    response::Json,
};
use common::SuccessResponse;
use serde_json::json;
use std::sync::Arc;
use tracing::{error, info};

use crate::app_state::AppState;
use crate::dto::{
    ActionRequest, CreateInstanceDto, InstanceMutationConfirmation, UpdateInstanceDto,
};
use crate::error::AutomationError;
use crate::instance_configuration::{
    InstanceConfigurationAcceptance, InstanceConfigurationMutation, InstanceConfigurationPayload,
    InstanceConfigurationRevision,
};

/// Return the current authoritative instance-configuration CAS head.
pub async fn get_instance_configuration_revision(
    State(state): State<Arc<AppState>>,
) -> Result<Json<SuccessResponse<serde_json::Value>>, AutomationError> {
    let revision = state
        .instance_configuration_application
        .current_revision()
        .await?;
    Ok(Json(SuccessResponse::new(json!({
        "scope": "instances",
        "revision": revision.get()
    }))))
}

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
                    "instance_name": "pump_01",
                    "product_name": "pump",
                    "properties": {
                        "max_flow_lpm": 500.0,
                        "manufacturer": "Example Corp",
                        "model": "P-500"
                    },
                    "created_at": "2025-10-15T10:30:00Z",
                    "updated_at": "2025-10-15T10:30:00Z"
                }
            })
        ),
        (status = 403, description = "Missing/invalid credentials or actor lacks automation.instance.manage"),
        (status = 409, description = "Stale instances revision or duplicate identity"),
        (status = 422, description = "Explicit confirmation, revision, or desired state is invalid")
    ),
    security(
        ("bearer_auth" = [])
    ),
    tag = "automation"
)]
pub async fn create_instance(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(dto): Json<CreateInstanceDto>,
) -> Result<Json<SuccessResponse<serde_json::Value>>, AutomationError> {
    let expected_revision = InstanceConfigurationRevision::new(dto.expected_revision);
    let confirmed = dto.confirmed;
    let req = CreateInstanceRequest {
        instance_id: dto.instance_id,
        instance_name: dto.instance_name,
        product_name: dto.product_name,
        parent_id: dto.parent_id,
        properties: dto.properties.unwrap_or_default(),
    };

    let acceptance = apply_instance_mutation(
        &state,
        &headers,
        confirmed,
        InstanceConfigurationMutation::Create {
            request: req,
            expected_revision,
        },
    )
    .await?;
    let InstanceConfigurationPayload::Created(instance) = acceptance.payload() else {
        return Err(AutomationError::InternalError(
            "instance create returned an unexpected payload".to_string(),
        ));
    };
    Ok(Json(SuccessResponse::new(json!({
        "instance": instance,
        "governance": governance_response(&acceptance)
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
        ("id" = u32, Path, description = "Instance ID")
    ),
    request_body = UpdateInstanceDto,
    responses(
        (status = 200, description = "Instance updated successfully", body = serde_json::Value,
            example = json!({
                "instance": {
                    "instance_id": 1,
                    "instance_name": "pump_renamed",
                    "product_name": "pump",
                    "properties": {
                        "max_flow_lpm": 500.0,
                        "manufacturer": "Example Corp",
                        "model": "P-500"
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
    security(
        ("bearer_auth" = [])
    ),
    tag = "automation"
)]
pub async fn update_instance(
    State(state): State<Arc<AppState>>,
    Path(id): Path<u32>,
    headers: HeaderMap,
    Json(dto): Json<UpdateInstanceDto>,
) -> Result<Json<SuccessResponse<serde_json::Value>>, AutomationError> {
    // Validate: at least one field must be provided
    if dto.instance_name.is_none() && dto.properties.is_none() {
        return Err(AutomationError::InvalidData(
            "At least one field (instance_name or properties) must be provided".to_string(),
        ));
    }

    let acceptance = apply_instance_mutation(
        &state,
        &headers,
        dto.confirmed,
        InstanceConfigurationMutation::Update {
            instance_id: id,
            instance_name: dto.instance_name,
            properties: dto.properties,
            expected_revision: InstanceConfigurationRevision::new(dto.expected_revision),
        },
    )
    .await?;
    let InstanceConfigurationPayload::Updated { instance_name, .. } = acceptance.payload() else {
        return Err(AutomationError::InternalError(
            "instance update returned an unexpected payload".to_string(),
        ));
    };

    info!("Instance {} updated successfully", id);

    // Query and return updated instance
    match state.instance_manager.get_instance(id).await {
        Ok(instance) => Ok(Json(SuccessResponse::new(json!({
            "instance": instance,
            "governance": governance_response(&acceptance)
        })))),
        Err(e) => {
            error!("Failed to query updated instance {}: {}", id, e);
            // Update succeeded but query failed - return id as fallback
            Ok(Json(SuccessResponse::new(json!({
                "instance_id": id,
                "instance_name": instance_name,
                "message": "Instance updated successfully but failed to retrieve details",
                "governance": governance_response(&acceptance)
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
        ("id" = u32, Path, description = "Instance ID")
    ),
    responses(
        (status = 200, description = "Instance deleted", body = serde_json::Value,
            example = json!({
                "message": "Instance 1 deleted"
            })
        ),
        (status = 403, description = "Missing/invalid credentials or actor lacks automation.instance.manage"),
        (status = 409, description = "Stale instances revision or routed subtree"),
        (status = 422, description = "Explicit confirmation or revision is invalid")
    ),
    security(
        ("bearer_auth" = [])
    ),
    tag = "automation"
)]
pub async fn delete_instance(
    State(state): State<Arc<AppState>>,
    Path(id): Path<u32>,
    headers: HeaderMap,
    Query(request): Query<InstanceMutationConfirmation>,
) -> Result<Json<SuccessResponse<serde_json::Value>>, AutomationError> {
    let acceptance = apply_instance_mutation(
        &state,
        &headers,
        request.confirmed,
        InstanceConfigurationMutation::DeleteSubtree {
            instance_id: id,
            expected_revision: InstanceConfigurationRevision::new(request.expected_revision),
        },
    )
    .await?;
    let InstanceConfigurationPayload::Deleted {
        deleted_instance_ids,
        ..
    } = acceptance.payload()
    else {
        return Err(AutomationError::InternalError(
            "instance deletion returned an unexpected payload".to_string(),
        ));
    };
    Ok(Json(SuccessResponse::new(json!({
        "message": format!("Instance {} subtree deleted", id),
        "deleted_instance_ids": deleted_instance_ids,
        "governance": governance_response(&acceptance)
    }))))
}

pub(crate) async fn apply_instance_mutation(
    state: &AppState,
    headers: &HeaderMap,
    confirmed: bool,
    mutation: InstanceConfigurationMutation,
) -> Result<InstanceConfigurationAcceptance, AutomationError> {
    let timestamp =
        aether_domain::TimestampMs::new(chrono::Utc::now().timestamp_millis().max(0) as u64);
    let invocation = crate::infra::application_control::command_invocation_from_headers(
        &state.control_authenticator,
        headers,
        confirmed,
        timestamp,
    );
    let acceptance = state
        .instance_configuration_application
        .mutate(invocation.context(), mutation)
        .await?;
    if let Some(failure) = acceptance.audit_status().failure() {
        error!(
            request_id = acceptance.request_id(),
            error = %failure,
            "instance configuration committed but terminal audit is incomplete; do not retry"
        );
    }
    if let Some(failure) = acceptance.runtime_status().failure() {
        error!(
            request_id = acceptance.request_id(),
            error = %failure,
            "instance configuration committed but cache reconciliation is degraded; do not retry"
        );
    }
    Ok(acceptance)
}

pub(crate) fn governance_response(
    acceptance: &InstanceConfigurationAcceptance,
) -> serde_json::Value {
    json!({
        "request_id": acceptance.request_id(),
        "operation": acceptance.kind().as_str(),
        "resulting_revision": acceptance.resulting_revision().get(),
        "audit": {
            "status": acceptance.audit_status().as_str(),
            "retryable": false
        },
        "runtime": {
            "status": acceptance.runtime_status().as_str(),
            "reconciliation_required": acceptance.runtime_status().reconciliation_required()
        },
        "retryable": acceptance.is_retryable()
    })
}

/// Reload instances from database
///
/// Rebuilds process-local caches from authoritative SQLite configuration.
///
#[cfg_attr(feature = "swagger-ui", utoipa::path(
    post,
    path = "/api/instances/reload",
    responses(
        (status = 200, description = "Instances reloaded from SQLite", body = serde_json::Value),
        (status = 500, description = "Reload failed", body = serde_json::Value)
    ),
    tag = "automation"
))]
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

/// Submit an instance action to the local command plane.
///
/// A successful response means SHM mirroring and the IO transport notification
/// were accepted locally. It does not mean the physical device executed or
/// acknowledged the command.
#[utoipa::path(
    post,
    path = "/api/instances/{id}/action",
    params(
        ("id" = u32, Path, description = "Instance ID"),
        ("x-request-id" = Option<String>, Header, description = "Optional UUID audit correlation ID")
    ),
    request_body = crate::dto::ActionRequest,
    responses(
        (status = 200, description = "Action accepted by the local command plane. `completed_at_ms` is the local transport-acceptance timestamp retained for compatibility; it is not a device completion time. A terminal-audit append failure is returned here as `audit.status=incomplete` with `retryable=false`, never as a retryable dispatch error.", body = serde_json::Value,
            example = json!({
                "message": "Action accepted by local command plane",
                "command_id": "018f0000000070008000000000000007",
                "request_id": "018f0000-0000-7000-8000-000000000007",
                "audit": { "status": "recorded", "retryable": false },
                "completed_at_ms": 1720000000000_u64
            })
        ),
        (status = 403, description = "Credentials or permission denied the action"),
        (status = 422, description = "Invalid action request or required confirmation was not provided"),
        (status = 503, description = "The required attempted audit or downstream dispatch failed before command acceptance")
    ),
    security(
        ("bearer_auth" = []),
        ("aether_service_auth" = [])
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
    let acceptance = state
        .control_application
        .write_point(
            invocation.context(),
            invocation.command_id(),
            target,
            req.value,
        )
        .await
        .map_err(AutomationError::from)?;
    if let Some(failure) = acceptance.completion_audit().failure() {
        error!(
            request_id = acceptance.request_id(),
            command_id = %format_args!("{:032x}", acceptance.command_id().get()),
            error = %failure,
            "device command was accepted but its terminal audit is incomplete; do not retry"
        );
    }
    Ok(Json(SuccessResponse::new(json!({
        "message": "Action accepted by local command plane",
        "instance_id": id,
        "point_id": req.point_id,
        "value": req.value,
        "command_id": format!("{:032x}", acceptance.command_id().get()),
        "request_id": acceptance.request_id(),
        "audit": crate::infra::application_control::completion_audit_response(
            acceptance.completion_audit()
        ),
        "completed_at_ms": acceptance.completed_at().get()
    }))))
}
