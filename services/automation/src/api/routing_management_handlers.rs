//! Instance Routing Management API Handlers
//!
//! This module provides API handlers for managing routing configurations.
//! It includes functions to create, update, delete, and validate routing
//! configurations between channels and model instances.

#![allow(clippy::disallowed_methods)] // json! macro used in multiple functions

use axum::{
    extract::{Path, State},
    response::Json,
};
use common::SuccessResponse;
use serde_json::json;
use std::sync::Arc;

use crate::app_state::AppState;
use crate::dto::{PointType, RoutingRequest};
use crate::error::AutomationError;
use crate::routing_loader::{ActionRoutingRow, MeasurementRoutingRow};

/// Create a new routing for an instance
///
/// Creates a new channel-to-instance point routing. Validates that both
/// the channel and instance points exist before creating.
#[utoipa::path(
    post,
    path = "/api/instances/{id}/routing",
    params(
        ("id" = u16, Path, description = "Instance ID")
    ),
    request_body = crate::dto::RoutingRequest,
    responses(
        (status = 200, description = "Routing created", body = serde_json::Value,
            example = json!({
                "routing": {
                    "instance_id": 1,
                    "channel": {
                        "id": 1,
                        "four_remote": "T",
                        "point_id": 101
                    },
                    "point_id": 101
                }
            })
        ),
        (status = 400, description = "Validation error"),
        (status = 404, description = "Instance not found"),
        (status = 500, description = "Database error")
    ),
    tag = "automation"
)]
pub async fn create_instance_routing(
    State(state): State<Arc<AppState>>,
    Path(id): Path<u32>,
    Json(routing): Json<RoutingRequest>,
) -> Result<Json<SuccessResponse<serde_json::Value>>, AutomationError> {
    // Validate instance exists
    let instance_exists = match sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS(SELECT 1 FROM instances WHERE instance_id = ?)",
    )
    .bind(id)
    .fetch_one(&state.instance_manager.pool)
    .await
    {
        Ok(exists) => exists,
        Err(e) => {
            return Err(AutomationError::InternalError(format!(
                "Database error: {}",
                e
            )));
        },
    };

    if !instance_exists {
        return Err(AutomationError::InternalError(format!(
            "Not found: Instance {} does not exist",
            id
        )));
    }

    // Get instance name for validation
    let instance =
        state.instance_manager.get_instance(id).await.map_err(|e| {
            AutomationError::InternalError(format!("Failed to get instance: {}", e))
        })?;
    let instance_name = &instance.core.instance_name;

    // Get routing type from request (explicit M/A specification)
    let routing_type = routing.point_type;

    // Validate based on routing type (determined by point_id, not four_remote)
    let validation_result = match routing_type {
        PointType::Measurement => {
            let routing_row = MeasurementRoutingRow {
                channel_id: routing.channel_id,
                channel_type: routing.four_remote,
                channel_point_id: routing.channel_point_id,
                measurement_id: routing.point_id,
            };
            state
                .instance_manager
                .validate_measurement_routing(&routing_row, instance_name)
                .await
        },
        PointType::Action => {
            let routing_row = ActionRoutingRow {
                action_id: routing.point_id,
                channel_id: routing.channel_id,
                channel_type: routing.four_remote,
                channel_point_id: routing.channel_point_id,
            };
            state
                .instance_manager
                .validate_action_routing(&routing_row, instance_name)
                .await
        },
    };

    match validation_result {
        Ok(validation) => {
            if !validation.is_valid {
                return Err(AutomationError::InvalidData(format!(
                    "Invalid routing: {:?}",
                    validation.errors
                )));
            }
        },
        Err(e) => {
            return Err(AutomationError::InvalidData(format!(
                "Validation failed: {}",
                e
            )));
        },
    }

    // Insert into database based on routing type (M or A)
    let insert_result = match routing_type {
        PointType::Measurement => {
            sqlx::query(
                r#"
                INSERT INTO measurement_routing
                (instance_id, instance_name, channel_id, channel_type, channel_point_id,
                 measurement_id, enabled)
                VALUES (?, (SELECT instance_name FROM instances WHERE instance_id = ?), ?, ?, ?, ?, true)
                "#,
            )
            .bind(id)
            .bind(id)
            .bind(routing.channel_id)
            .bind(routing.four_remote.map(|fr| fr.as_str()))
            .bind(routing.channel_point_id)
            .bind(routing.point_id)
            .execute(&state.instance_manager.pool)
            .await
        },
        PointType::Action => {
            sqlx::query(
                r#"
                INSERT INTO action_routing
                (instance_id, instance_name, action_id, channel_id, channel_type,
                 channel_point_id, enabled)
                VALUES (?, (SELECT instance_name FROM instances WHERE instance_id = ?), ?, ?, ?, ?, true)
                "#,
            )
            .bind(id)
            .bind(id)
            .bind(routing.point_id)
            .bind(routing.channel_id)
            .bind(routing.four_remote.map(|fr| fr.as_str()))
            .bind(routing.channel_point_id)
            .execute(&state.instance_manager.pool)
            .await
        },
    };

    // Check insert result
    if let Err(e) = insert_result {
        return Err(AutomationError::InternalError(format!(
            "Failed to insert routing: {}",
            e
        )));
    }

    state
        .instance_manager
        .refresh_routing()
        .await
        .map_err(|e| {
            AutomationError::InternalError(format!("Failed to refresh routing after create: {}", e))
        })?;

    Ok(Json(SuccessResponse::new(json!({
        "routing": {
            "instance_id": id,
            "channel": {
                "id": routing.channel_id,
                "four_remote": routing.four_remote.map(|fr| fr.as_str()),
                "point_id": routing.channel_point_id
            },
            "point_id": routing.point_id
        }
    }))))
}

/// Update routings for an instance (UPSERT)
///
/// Updates or creates routings for the specified points. Uses UPSERT semantics:
/// - Points in the request: created if not exists, updated if exists
/// - Points NOT in the request: remain unchanged (not deleted)
///
/// Uses a transaction to ensure atomic operation.
#[utoipa::path(
    put,
    path = "/api/instances/{id}/routing",
    params(
        ("id" = u16, Path, description = "Instance ID")
    ),
    request_body = [crate::dto::RoutingRequest],
    responses(
        (status = 200, description = "Routings updated", body = serde_json::Value,
            example = json!({"message": "Updated 5 routings"})
        ),
        (status = 400, description = "Validation errors"),
        (status = 500, description = "Transaction error")
    ),
    tag = "automation"
)]
pub async fn update_instance_routing(
    State(state): State<Arc<AppState>>,
    Path(id): Path<u32>,
    Json(routings): Json<Vec<RoutingRequest>>,
) -> Result<Json<SuccessResponse<serde_json::Value>>, AutomationError> {
    // Begin transaction
    let mut tx = match state.instance_manager.pool.begin().await {
        Ok(tx) => tx,
        Err(e) => {
            return Err(AutomationError::InternalError(format!(
                "Failed to start transaction: {}",
                e
            )));
        },
    };

    // Get instance name for validation
    let instance = match state.instance_manager.get_instance(id).await {
        Ok(inst) => inst,
        Err(e) => {
            if let Err(rb_err) = tx.rollback().await {
                tracing::error!("Transaction rollback failed: {}", rb_err);
            }
            return Err(AutomationError::InternalError(format!(
                "Failed to get instance: {}",
                e
            )));
        },
    };
    let instance_name = &instance.core.instance_name;

    // Insert new routings
    let mut success_count = 0;
    let mut errors = Vec::new();

    for routing in routings {
        // Get routing type from request (explicit M/A specification)
        let routing_type = routing.point_type;

        // Handle unbind routing: when channel_id is None, DELETE instead of UPSERT
        // This supports: null, "", or omitted field → None → DELETE
        if routing.channel_id.is_none() {
            let result = match routing_type {
                PointType::Measurement => sqlx::query(
                    "DELETE FROM measurement_routing WHERE instance_id = ? AND measurement_id = ?",
                )
                .bind(id)
                .bind(routing.point_id)
                .execute(&mut *tx)
                .await,
                PointType::Action => {
                    sqlx::query(
                        "DELETE FROM action_routing WHERE instance_id = ? AND action_id = ?",
                    )
                    .bind(id)
                    .bind(routing.point_id)
                    .execute(&mut *tx)
                    .await
                },
            };

            match result {
                Ok(_) => {
                    success_count += 1;
                    tracing::debug!(
                        instance_id = id,
                        point_id = routing.point_id,
                        routing_type = ?routing_type,
                        "Routing unbound"
                    );
                },
                Err(e) => {
                    errors.push(format!(
                        "Failed to delete routing for point {}: {}",
                        routing.point_id, e
                    ));
                },
            }
            continue;
        }

        // Validate based on routing type (determined by point_id, not four_remote)
        let validation_result = match routing_type {
            PointType::Measurement => {
                let routing_row = MeasurementRoutingRow {
                    channel_id: routing.channel_id,
                    channel_type: routing.four_remote,
                    channel_point_id: routing.channel_point_id,
                    measurement_id: routing.point_id,
                };
                state
                    .instance_manager
                    .validate_measurement_routing(&routing_row, instance_name)
                    .await
            },
            PointType::Action => {
                let routing_row = ActionRoutingRow {
                    action_id: routing.point_id,
                    channel_id: routing.channel_id,
                    channel_type: routing.four_remote,
                    channel_point_id: routing.channel_point_id,
                };
                state
                    .instance_manager
                    .validate_action_routing(&routing_row, instance_name)
                    .await
            },
        };

        match validation_result {
            Ok(validation) => {
                if !validation.is_valid {
                    errors.extend(validation.errors);
                    continue;
                }
            },
            Err(e) => {
                errors.push(e.to_string());
                continue;
            },
        }

        // UPSERT into appropriate table based on routing type (M or A)
        let result = match routing_type {
            PointType::Measurement => {
                sqlx::query(
                    r#"
                    INSERT INTO measurement_routing
                    (instance_id, instance_name, channel_id, channel_type, channel_point_id,
                     measurement_id, enabled)
                    VALUES (?, (SELECT instance_name FROM instances WHERE instance_id = ?), ?, ?, ?, ?, true)
                    ON CONFLICT(instance_id, measurement_id) DO UPDATE SET
                        channel_id = excluded.channel_id,
                        channel_type = excluded.channel_type,
                        channel_point_id = excluded.channel_point_id,
                        instance_name = excluded.instance_name,
                        enabled = true
                    "#,
                )
                .bind(id)
                .bind(id)
                .bind(routing.channel_id)
                .bind(routing.four_remote.map(|fr| fr.as_str()))
                .bind(routing.channel_point_id)
                .bind(routing.point_id)
                .execute(&mut *tx)
                .await
            },
            PointType::Action => {
                sqlx::query(
                    r#"
                    INSERT INTO action_routing
                    (instance_id, instance_name, action_id, channel_id, channel_type,
                     channel_point_id, enabled)
                    VALUES (?, (SELECT instance_name FROM instances WHERE instance_id = ?), ?, ?, ?, ?, true)
                    ON CONFLICT(instance_id, action_id) DO UPDATE SET
                        channel_id = excluded.channel_id,
                        channel_type = excluded.channel_type,
                        channel_point_id = excluded.channel_point_id,
                        instance_name = excluded.instance_name,
                        enabled = true
                    "#,
                )
                .bind(id)
                .bind(id)
                .bind(routing.point_id)
                .bind(routing.channel_id)
                .bind(routing.four_remote.map(|fr| fr.as_str()))
                .bind(routing.channel_point_id)
                .execute(&mut *tx)
                .await
            },
        };

        if result.is_ok() {
            success_count += 1;
        } else if let Err(e) = result {
            errors.push(e.to_string());
        }
    }

    if errors.is_empty() {
        if let Err(e) = tx.commit().await {
            return Err(AutomationError::InternalError(format!(
                "Failed to commit transaction: {}",
                e
            )));
        }

        state
            .instance_manager
            .refresh_routing()
            .await
            .map_err(|e| {
                AutomationError::InternalError(format!(
                    "Failed to refresh routing after update: {}",
                    e
                ))
            })?;

        Ok(Json(SuccessResponse::new(json!({
            "message": format!("Updated {} routings", success_count)
        }))))
    } else {
        if let Err(rb_err) = tx.rollback().await {
            tracing::error!("Transaction rollback failed: {}", rb_err);
        }
        Err(AutomationError::InvalidData(format!(
            "Update failed: {:?}",
            errors
        )))
    }
}

/// Delete all C2M and M2C routings for an instance.
///
/// **Destructive**: removes every row matching `instance_id = ?` from both
/// `measurement_routing` and `action_routing`. The instance itself is
/// preserved (product model unchanged), but all channel associations are
/// severed — measurements stop flowing in and control commands have no
/// path to the device. Typical use: clear old routings before
/// reconfiguring a replaced device. Triggers a routing-cache reload on
/// completion.
#[utoipa::path(
    delete,
    path = "/api/instances/{id}/routing",
    params(
        ("id" = u16, Path, description = "Instance ID")
    ),
    responses(
        (status = 200, description = "Routings deleted", body = serde_json::Value,
            example = json!({"message": "Deleted 5 routings (3 measurement, 2 action)"})
        ),
        (status = 500, description = "Database error")
    ),
    tag = "automation"
)]
pub async fn delete_instance_routing(
    State(state): State<Arc<AppState>>,
    Path(id): Path<u32>,
) -> Result<Json<SuccessResponse<serde_json::Value>>, AutomationError> {
    match state.instance_manager.delete_all_routing(id).await {
        Ok((measurement_count, action_count)) => {
            let total_count = measurement_count + action_count;

            // SQL is authoritative; refreshing the local cache immediately
            // revokes instance access to the physical SHM slots.
            state
                .instance_manager
                .refresh_routing()
                .await
                .map_err(|e| {
                    AutomationError::InternalError(format!(
                        "Failed to refresh routing after delete: {}",
                        e
                    ))
                })?;

            Ok(Json(SuccessResponse::new(json!({
                "message": format!(
                    "Deleted {} routings ({} measurement, {} action)",
                    total_count, measurement_count, action_count
                )
            }))))
        },
        Err(e) => Err(AutomationError::InternalError(format!(
            "Failed to delete routings: {}",
            e
        ))),
    }
}

/// Validate routing completeness and integrity for an instance.
///
/// Checks that every `measurement_point` maps to a real channel point, that
/// every `action_point` does the same, that the referenced channel is enabled,
/// that each `point_id` exists in the corresponding `{type}_points` table, and
/// that types are compatible (M must map to T/S; A must map to C/A). Returns
/// an `issues` list for use in the configuration health-check UI. Read-only —
/// no state is modified.
#[utoipa::path(
    post,
    path = "/api/instances/{id}/routing/validate",
    params(
        ("id" = u16, Path, description = "Instance ID")
    ),
    request_body = [crate::dto::RoutingRequest],
    responses(
        (status = 200, description = "Validation completed", body = serde_json::Value,
            example = json!({
                "instance_id": 1,
                "validations": [
                    {"channel": "1:T:101", "valid": true, "errors": []},
                    {"channel": "1:T:102", "valid": false, "errors": ["Point not found"]}
                ]
            })
        )
    ),
    tag = "automation"
)]
pub async fn validate_instance_routing(
    State(state): State<Arc<AppState>>,
    Path(id): Path<u32>,
    Json(routings): Json<Vec<RoutingRequest>>,
) -> Result<Json<SuccessResponse<serde_json::Value>>, AutomationError> {
    // Get instance name for validation
    let instance =
        state.instance_manager.get_instance(id).await.map_err(|e| {
            AutomationError::InternalError(format!("Failed to get instance: {}", e))
        })?;
    let instance_name = &instance.core.instance_name;

    let mut results = Vec::new();

    for routing in routings {
        // Save channel info for response
        let channel_info = format!(
            "{}:{}:{}",
            routing
                .channel_id
                .map_or("null".to_string(), |v| v.to_string()),
            routing
                .four_remote
                .as_ref()
                .map_or("null".to_string(), |fr| fr.to_string()),
            routing
                .channel_point_id
                .map_or("null".to_string(), |v| v.to_string())
        );

        // If all three channel fields are null, skip validation (unbound routing)
        if routing.channel_id.is_none()
            && routing.four_remote.is_none()
            && routing.channel_point_id.is_none()
        {
            results.push(json!({
                "channel": &channel_info,
                "valid": true,
                "errors": Vec::<String>::new()
            }));
            continue;
        }

        // Get routing type from request (explicit M/A specification)
        let routing_type = routing.point_type;

        // Validate based on routing type
        let validation_result = match routing_type {
            PointType::Measurement => {
                // Measurement routing (T/S → M)
                let routing_row = MeasurementRoutingRow {
                    channel_id: routing.channel_id,
                    channel_type: routing.four_remote,
                    channel_point_id: routing.channel_point_id,
                    measurement_id: routing.point_id,
                };
                state
                    .instance_manager
                    .validate_measurement_routing(&routing_row, instance_name)
                    .await
            },
            PointType::Action => {
                // Action routing (A → C/A)
                let routing_row = ActionRoutingRow {
                    action_id: routing.point_id,
                    channel_id: routing.channel_id,
                    channel_type: routing.four_remote,
                    channel_point_id: routing.channel_point_id,
                };
                state
                    .instance_manager
                    .validate_action_routing(&routing_row, instance_name)
                    .await
            },
        };

        match validation_result {
            Ok(validation) => {
                results.push(json!({
                    "channel": &channel_info,
                    "valid": validation.is_valid,
                    "errors": validation.errors
                }));
            },
            Err(e) => {
                results.push(json!({
                    "channel": &channel_info,
                    "valid": false,
                    "errors": vec![e.to_string()]
                }));
            },
        }
    }

    Ok(Json(SuccessResponse::new(json!({
        "instance_id": id,
        "validations": results
    }))))
}
