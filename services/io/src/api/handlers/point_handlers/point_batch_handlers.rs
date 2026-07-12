#![allow(clippy::disallowed_methods)]

//! Batch point operations handler (create, update, delete in bulk)

use crate::api::routes::AppState;
use crate::dto::{AppError, SuccessResponse};
use axum::{
    extract::{Path, Query, State},
    response::Json,
};

use super::point_crud_handlers::{delete_point_handler_inner, update_point_handler_inner};
use super::point_helpers::{trigger_channel_reload_if_needed, validate_channel_exists};
use super::point_types::*;

// ============================================================================
// Batch Point CRUD Handler
// ============================================================================

/// Batch point operations (create, update, delete)
///
/// Process multiple point operations in a single request. Supports creating,
/// updating, and deleting points of any type (T/S/C/A). Operations are processed
/// independently - a single failure does not affect other operations.
#[utoipa::path(
    post,
    path = "/api/channels/{channel_id}/points/batch",
    params(
        ("channel_id" = u32, Path, description = "Channel identifier"),
        ("auto_reload" = bool, Query, description = "Reconcile the channel through the governed application boundary after the batch (default: false)")
    ),
    request_body(
        content = PointBatchRequest,
        description = "Batch operations request. Provide create, update, and/or delete arrays.",
        examples(
            ("Mixed Operations" = (
                summary = "Create, update, and delete in one request",
                description = "Example showing all three operation types",
                value = json!({
                    "create": [
                        {
                            "point_type": "T",
                            "point_id": 101,
                            "data": {
                                "signal_name": "DC_Voltage",
                                "scale": 0.1,
                                "offset": 0.0,
                                "unit": "V",
                                "data_type": "float32",
                                "reverse": false,
                                "description": "DC bus voltage"
                            }
                        },
                        {
                            "point_type": "S",
                            "point_id": 201,
                            "data": {
                                "signal_name": "Grid_Connected",
                                "data_type": "bool",
                                "reverse": false,
                                "description": "Grid connection status"
                            }
                        }
                    ],
                    "update": [
                        {
                            "point_type": "T",
                            "point_id": 102,
                            "data": {
                                "signal_name": "DC_Current",
                                "scale": 0.01,
                                "description": "Updated DC current"
                                // Partial update: only these 3 fields updated, others unchanged
                            }
                        }
                    ],
                    "delete": [
                        {
                            "point_type": "A",
                            "point_id": 301
                        }
                    ]
                })
            )),
            ("Batch Create Only" = (
                summary = "Create multiple points",
                description = "Batch create telemetry points",
                value = json!({
                    "create": [
                        {
                            "point_type": "T",
                            "point_id": 103,
                            "data": {
                                "signal_name": "Temperature_1",
                                "scale": 0.1,
                                "offset": -40.0,
                                "unit": "°C",
                                "data_type": "int16",
                                "description": "Temperature sensor 1"
                            }
                        },
                        {
                            "point_type": "T",
                            "point_id": 104,
                            "data": {
                                "signal_name": "Temperature_2",
                                "scale": 0.1,
                                "offset": -40.0,
                                "unit": "°C",
                                "data_type": "int16",
                                "description": "Temperature sensor 2"
                            }
                        }
                    ]
                })
            )),
            ("Batch Update Only" = (
                summary = "Update multiple points (partial update supported)",
                description = "Batch update point configurations. **Only provide fields you want to update** - other fields remain unchanged. This example shows updating only 2 fields for point 101, and only 1 field for point 102.",
                value = json!({
                    "update": [
                        {
                            "point_type": "T",
                            "point_id": 101,
                            "scale": 0.2,              // Only update scale
                            "description": "Updated description"  // Only update description
                            // Other fields (unit, offset, data_type, etc.) remain unchanged
                        },
                        {
                            "point_type": "T",
                            "point_id": 102,
                            "unit": "kW"              // Only update unit, all other fields unchanged
                        }
                    ]
                })
            )),
            ("Batch Delete Only" = (
                summary = "Delete multiple points",
                description = "Batch delete obsolete points",
                value = json!({
                    "delete": [
                        {
                            "point_type": "A",
                            "point_id": 301
                        },
                        {
                            "point_type": "A",
                            "point_id": 302
                        }
                    ]
                })
            )),
            ("Force Create (UPSERT)" = (
                summary = "Force create with INSERT OR REPLACE behavior",
                description = "Use force=true to enable UPSERT mode: if point exists, it will be replaced; if not, it will be created. This is useful for batch imports where you want to ensure the data matches exactly what you provide, regardless of existing state. **Default behavior (force=false)**: CREATE fails if point already exists.",
                value = json!({
                    "create": [
                        {
                            "point_type": "T",
                            "point_id": 105,
                            "force": false,  // Default mode: fail if point 105 exists
                            "data": {
                                "signal_name": "Voltage_L1",
                                "scale": 0.1,
                                "offset": 0.0,
                                "unit": "V",
                                "data_type": "float32",
                                "reverse": false,
                                "description": "Phase L1 voltage"
                            }
                        },
                        {
                            "point_type": "T",
                            "point_id": 106,
                            "force": true,   // UPSERT mode: replace if exists, create if not
                            "data": {
                                "signal_name": "Voltage_L2",
                                "scale": 0.1,
                                "offset": 0.0,
                                "unit": "V",
                                "data_type": "float32",
                                "reverse": false,
                                "description": "Phase L2 voltage (will replace existing config if any)"
                            }
                        }
                    ]
                })
            ))
        )
    ),
    responses(
        (status = 200, description = "Batch operation completed", body = PointBatchResult,
            example = json!({
                "success": true,
                "data": {
                    "total_operations": 4,
                    "succeeded": 3,
                    "failed": 1,
                    "operation_stats": {
                        "create": {
                            "total": 2,
                            "succeeded": 1,
                            "failed": 1
                        },
                        "update": {
                            "total": 1,
                            "succeeded": 1,
                            "failed": 0
                        },
                        "delete": {
                            "total": 1,
                            "succeeded": 1,
                            "failed": 0
                        }
                    },
                    "errors": [
                        {
                            "operation": "create",
                            "point_type": "S",
                            "point_id": 201,
                            "error": "Point 201 already exists"
                        }
                    ],
                    "duration_ms": 145
                }
            })
        ),
        (status = 400, description = "Invalid request (empty operations)"),
        (status = 404, description = "Channel not found")
    ),
    tag = "io"
)]
pub async fn batch_point_operations_handler(
    Path(channel_id): Path<u32>,
    State(state): State<AppState>,
    Query(reload_query): Query<crate::dto::AutoReloadQuery>,
    Json(request): Json<PointBatchRequest>,
) -> Result<Json<SuccessResponse<PointBatchResult>>, AppError> {
    use std::time::Instant;
    let start_time = Instant::now();

    // Validate at least one operation is provided
    if request.create.is_empty() && request.update.is_empty() && request.delete.is_empty() {
        return Err(AppError::bad_request(
            "At least one operation (create/update/delete) must be provided",
        ));
    }

    // Validate channel exists (fail fast for invalid channel)
    validate_channel_exists(&state.sqlite_pool, channel_id).await?;

    // Initialize statistics
    let mut create_stat = OperationStat::default();
    let mut update_stat = OperationStat::default();
    let mut delete_stat = OperationStat::default();
    let mut errors = Vec::new();

    // Process operations in order: DELETE -> CREATE -> UPDATE
    // This order prevents ID conflicts when replacing a point (delete old, create new)

    // 1. Process DELETE operations first (free up IDs for potential re-creation)
    delete_stat.total = request.delete.len();
    for item in request.delete {
        match process_delete_operation(channel_id, &item, &state).await {
            Ok(_) => delete_stat.succeeded += 1,
            Err(e) => {
                delete_stat.failed += 1;
                errors.push(PointBatchError {
                    operation: "delete".to_string(),
                    point_type: item.point_type.to_uppercase(),
                    point_id: item.point_id,
                    error: e.to_string(),
                });
            },
        }
    }

    // 2. Process CREATE operations (can now use IDs freed by deletions)
    create_stat.total = request.create.len();
    for item in request.create {
        match process_create_operation(channel_id, &item, &state).await {
            Ok(_) => create_stat.succeeded += 1,
            Err(e) => {
                create_stat.failed += 1;
                errors.push(PointBatchError {
                    operation: "create".to_string(),
                    point_type: item.point_type.to_uppercase(),
                    point_id: item.point_id,
                    error: e.to_string(),
                });
            },
        }
    }

    // 3. Process UPDATE operations last (may reference newly created points)
    update_stat.total = request.update.len();
    for item in request.update {
        match process_update_operation(channel_id, &item, &state).await {
            Ok(_) => update_stat.succeeded += 1,
            Err(e) => {
                update_stat.failed += 1;
                errors.push(PointBatchError {
                    operation: "update".to_string(),
                    point_type: item.point_type.to_uppercase(),
                    point_id: item.point_id,
                    error: e.to_string(),
                });
            },
        }
    }

    let total_operations = create_stat.total + update_stat.total + delete_stat.total;
    let succeeded = create_stat.succeeded + update_stat.succeeded + delete_stat.succeeded;
    let failed = create_stat.failed + update_stat.failed + delete_stat.failed;

    let duration_ms = start_time.elapsed().as_millis() as u64;

    tracing::debug!(
        "Ch{} batch: {}/{} ok ({}ms)",
        channel_id,
        succeeded,
        total_operations,
        duration_ms
    );

    // Trigger auto-reload if enabled (unified reload after all batch operations)
    trigger_channel_reload_if_needed(channel_id, &state, reload_query.auto_reload).await;

    Ok(Json(SuccessResponse::new(PointBatchResult {
        total_operations,
        succeeded,
        failed,
        operation_stats: OperationStats {
            create: create_stat,
            update: update_stat,
            delete: delete_stat,
        },
        errors,
        duration_ms,
    })))
}

// ----------------------------------------------------------------------------
/// Process single create operation
async fn process_create_operation(
    channel_id: u32,
    item: &PointBatchCreateItem,
    state: &AppState,
) -> Result<(), String> {
    use crate::core::config::{AdjustmentPoint, ControlPoint, SignalPoint, TelemetryPoint};

    let point_type_upper = item.point_type.to_ascii_uppercase();
    let table = match point_type_upper.as_str() {
        "T" => "telemetry_points",
        "S" => "signal_points",
        "C" => "control_points",
        "A" => "adjustment_points",
        _ => return Err(format!("Invalid point type '{}'", item.point_type)),
    };

    // Validate point uniqueness (skip if force=true for upsert behavior)
    if !item.force {
        let existing: Option<(i64,)> = sqlx::query_as(&format!(
            "SELECT point_id FROM {} WHERE channel_id = ? AND point_id = ?",
            table
        ))
        .bind(channel_id as i64)
        .bind(item.point_id as i64)
        .fetch_optional(&state.sqlite_pool)
        .await
        .map_err(|e| format!("Database error: {}", e))?;

        if existing.is_some() {
            return Err(format!("Point {} already exists", item.point_id));
        }
    }

    // Inject point_id into data before deserialization (required by Point struct)
    let mut data_with_id = item.data.clone();
    if let Some(obj) = data_with_id.as_object_mut() {
        obj.insert("point_id".to_string(), serde_json::json!(item.point_id));
    }

    // Extract protocol_mapping before deserialization (not part of Point structs)
    // Check if the field was explicitly provided in the request (even if null)
    let has_protocol_mapping_field = item.data.get("protocol_mapping").is_some();
    let protocol_mapping_json: Option<String> = item.data.get("protocol_mapping").and_then(|v| {
        // Handle explicit null or empty object: return None to clear the mapping
        // This is consistent with sqlite_loader.rs which filters out null/{}/""
        if v.is_null() || v.as_object().is_some_and(|o| o.is_empty()) {
            None
        } else {
            Some(serde_json::to_string(v).unwrap_or_default())
        }
    });

    // Deserialize and insert based on point type
    // Using ON CONFLICT ... DO UPDATE (UPSERT) to preserve protocol_mappings when not provided
    match point_type_upper.as_str() {
        "T" => {
            let point: TelemetryPoint = serde_json::from_value(data_with_id)
                .map_err(|e| format!("Invalid telemetry point data: {}", e))?;

            let sql = if item.force {
                if has_protocol_mapping_field {
                    // Request provided protocol_mapping (even if null) - update it
                    "INSERT INTO telemetry_points
                     (channel_id, point_id, signal_name, scale, offset, unit, data_type, reverse, description, protocol_mappings)
                     VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
                     ON CONFLICT(channel_id, point_id) DO UPDATE SET
                       signal_name = excluded.signal_name,
                       scale = excluded.scale,
                       offset = excluded.offset,
                       unit = excluded.unit,
                       data_type = excluded.data_type,
                       reverse = excluded.reverse,
                       description = excluded.description,
                       protocol_mappings = excluded.protocol_mappings"
                } else {
                    // Request did not provide protocol_mapping - preserve existing value
                    "INSERT INTO telemetry_points
                     (channel_id, point_id, signal_name, scale, offset, unit, data_type, reverse, description, protocol_mappings)
                     VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
                     ON CONFLICT(channel_id, point_id) DO UPDATE SET
                       signal_name = excluded.signal_name,
                       scale = excluded.scale,
                       offset = excluded.offset,
                       unit = excluded.unit,
                       data_type = excluded.data_type,
                       reverse = excluded.reverse,
                       description = excluded.description"
                    // protocol_mappings NOT included - preserves existing value
                }
            } else {
                "INSERT INTO telemetry_points
                 (channel_id, point_id, signal_name, scale, offset, unit, data_type, reverse, description, protocol_mappings)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"
            };

            sqlx::query(sql)
                .bind(channel_id as i64)
                .bind(item.point_id as i64)
                .bind(&point.base.signal_name)
                .bind(point.scale)
                .bind(point.offset)
                .bind(&point.base.unit)
                .bind(&point.data_type)
                .bind(point.reverse)
                .bind(&point.base.description)
                .bind(&protocol_mapping_json)
                .execute(&state.sqlite_pool)
                .await
                .map_err(|e| format!("Failed to insert: {}", e))?;
        },
        "S" => {
            let point: SignalPoint = serde_json::from_value(data_with_id)
                .map_err(|e| format!("Invalid signal point data: {}", e))?;

            let sql = if item.force {
                if has_protocol_mapping_field {
                    "INSERT INTO signal_points
                     (channel_id, point_id, signal_name, unit, reverse, description, protocol_mappings)
                     VALUES (?, ?, ?, ?, ?, ?, ?)
                     ON CONFLICT(channel_id, point_id) DO UPDATE SET
                       signal_name = excluded.signal_name,
                       unit = excluded.unit,
                       reverse = excluded.reverse,
                       description = excluded.description,
                       protocol_mappings = excluded.protocol_mappings"
                } else {
                    "INSERT INTO signal_points
                     (channel_id, point_id, signal_name, unit, reverse, description, protocol_mappings)
                     VALUES (?, ?, ?, ?, ?, ?, ?)
                     ON CONFLICT(channel_id, point_id) DO UPDATE SET
                       signal_name = excluded.signal_name,
                       unit = excluded.unit,
                       reverse = excluded.reverse,
                       description = excluded.description"
                }
            } else {
                "INSERT INTO signal_points
                 (channel_id, point_id, signal_name, unit, reverse, description, protocol_mappings)
                 VALUES (?, ?, ?, ?, ?, ?, ?)"
            };

            sqlx::query(sql)
                .bind(channel_id as i64)
                .bind(item.point_id as i64)
                .bind(&point.base.signal_name)
                .bind(&point.base.unit)
                .bind(point.reverse)
                .bind(&point.base.description)
                .bind(&protocol_mapping_json)
                .execute(&state.sqlite_pool)
                .await
                .map_err(|e| format!("Failed to insert: {}", e))?;
        },
        "C" => {
            let point: ControlPoint = serde_json::from_value(data_with_id)
                .map_err(|e| format!("Invalid control point data: {}", e))?;

            // Note: control_points table has same schema as telemetry_points
            // ControlPoint's control-specific fields (control_type, on_value, etc.) are not persisted
            let sql = if item.force {
                if has_protocol_mapping_field {
                    "INSERT INTO control_points
                     (channel_id, point_id, signal_name, scale, offset, unit, reverse, data_type, description, protocol_mappings)
                     VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
                     ON CONFLICT(channel_id, point_id) DO UPDATE SET
                       signal_name = excluded.signal_name,
                       scale = excluded.scale,
                       offset = excluded.offset,
                       unit = excluded.unit,
                       reverse = excluded.reverse,
                       data_type = excluded.data_type,
                       description = excluded.description,
                       protocol_mappings = excluded.protocol_mappings"
                } else {
                    "INSERT INTO control_points
                     (channel_id, point_id, signal_name, scale, offset, unit, reverse, data_type, description, protocol_mappings)
                     VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
                     ON CONFLICT(channel_id, point_id) DO UPDATE SET
                       signal_name = excluded.signal_name,
                       scale = excluded.scale,
                       offset = excluded.offset,
                       unit = excluded.unit,
                       reverse = excluded.reverse,
                       data_type = excluded.data_type,
                       description = excluded.description"
                }
            } else {
                "INSERT INTO control_points
                 (channel_id, point_id, signal_name, scale, offset, unit, reverse, data_type, description, protocol_mappings)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"
            };

            sqlx::query(sql)
                .bind(channel_id as i64)
                .bind(item.point_id as i64)
                .bind(&point.base.signal_name)
                .bind(1.0f64) // scale: default for control points
                .bind(0.0f64) // offset: default for control points
                .bind(&point.base.unit)
                .bind(point.reverse)
                .bind("bool") // data_type: default for control points
                .bind(&point.base.description)
                .bind(&protocol_mapping_json)
                .execute(&state.sqlite_pool)
                .await
                .map_err(|e| format!("Failed to insert: {}", e))?;
        },
        "A" => {
            // Extract reverse from JSON before consuming data_with_id (not in AdjustmentPoint struct)
            let reverse = data_with_id
                .get("reverse")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);

            let point: AdjustmentPoint = serde_json::from_value(data_with_id)
                .map_err(|e| format!("Invalid adjustment point data: {}", e))?;
            aether_domain::CommandConstraints::new(
                point.min_value,
                point.max_value,
                Some(point.step),
            )
            .map_err(|error| format!("Invalid adjustment constraints: {error}"))?;

            let sql = if item.force {
                if has_protocol_mapping_field {
                    "INSERT INTO adjustment_points
                     (channel_id, point_id, signal_name, scale, offset, unit, reverse, data_type, description, protocol_mappings, min_value, max_value, step)
                     VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
                     ON CONFLICT(channel_id, point_id) DO UPDATE SET
                       signal_name = excluded.signal_name,
                       scale = excluded.scale,
                       offset = excluded.offset,
                       unit = excluded.unit,
                       reverse = excluded.reverse,
                       data_type = excluded.data_type,
                       description = excluded.description,
                       min_value = excluded.min_value,
                       max_value = excluded.max_value,
                       step = excluded.step,
                       protocol_mappings = excluded.protocol_mappings"
                } else {
                    "INSERT INTO adjustment_points
                     (channel_id, point_id, signal_name, scale, offset, unit, reverse, data_type, description, protocol_mappings, min_value, max_value, step)
                     VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
                     ON CONFLICT(channel_id, point_id) DO UPDATE SET
                       signal_name = excluded.signal_name,
                       scale = excluded.scale,
                       offset = excluded.offset,
                       unit = excluded.unit,
                       reverse = excluded.reverse,
                       data_type = excluded.data_type,
                       description = excluded.description,
                       min_value = excluded.min_value,
                       max_value = excluded.max_value,
                       step = excluded.step"
                }
            } else {
                "INSERT INTO adjustment_points
                 (channel_id, point_id, signal_name, scale, offset, unit, reverse, data_type, description, protocol_mappings, min_value, max_value, step)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"
            };

            sqlx::query(sql)
                .bind(channel_id as i64)
                .bind(item.point_id as i64)
                .bind(&point.base.signal_name)
                .bind(point.scale)
                .bind(point.offset)
                .bind(&point.base.unit)
                .bind(reverse)
                .bind(&point.data_type)
                .bind(&point.base.description)
                .bind(&protocol_mapping_json)
                .bind(point.min_value)
                .bind(point.max_value)
                .bind(point.step)
                .execute(&state.sqlite_pool)
                .await
                .map_err(|e| format!("Failed to insert: {}", e))?;
        },
        other => {
            return Err(format!(
                "Invalid point type '{}'. Must be T, S, C, or A",
                other
            ));
        },
    }

    Ok(())
}

/// Process single update operation (reuse existing handler logic)
async fn process_update_operation(
    channel_id: u32,
    item: &PointBatchUpdateItem,
    state: &AppState,
) -> Result<(), String> {
    // Reuse the existing update_point_handler_inner logic
    // Note: Batch operations always trigger auto-reload at the end
    let reload_query = crate::dto::AutoReloadQuery { auto_reload: false };
    update_point_handler_inner(
        channel_id,
        &item.point_type,
        item.point_id,
        state.clone(),
        reload_query,
        item.data.clone(),
    )
    .await
    .map(|_| ())
    .map_err(|e| format!("{:?}", e))
}

/// Process single delete operation (reuse existing handler logic)
async fn process_delete_operation(
    channel_id: u32,
    item: &PointBatchDeleteItem,
    state: &AppState,
) -> Result<(), String> {
    // Reuse the existing delete_point_handler_inner logic
    // Note: Batch operations always trigger auto-reload at the end
    let reload_query = crate::dto::AutoReloadQuery { auto_reload: false };
    delete_point_handler_inner(
        channel_id,
        &item.point_type,
        item.point_id,
        state.clone(),
        reload_query,
    )
    .await
    .map(|_| ())
    .map_err(|e| format!("{:?}", e))
}
