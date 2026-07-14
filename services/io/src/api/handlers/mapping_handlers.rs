//! Protocol mapping handlers
//!
//! This module contains handlers for:
//! - Getting all mapping configurations for a channel
//! - Batch updating mapping configurations
//! - Validating mapping configurations based on protocol

#![allow(clippy::disallowed_methods)] // json! macro used in multiple functions

use crate::api::routes::AppState;
use crate::dto::{AppError, MappingBatchUpdateResult, MappingUpdateMode, SuccessResponse};
use axum::{
    Extension,
    extract::{Path, Query, State},
    http::HeaderMap,
    response::Json,
};
use serde_json::json;

// ============================================================================
// Helpers
// ============================================================================

/// Map four_remote type code to database table name
fn four_remote_to_table(four_remote: &str) -> Result<&'static str, AppError> {
    match four_remote {
        "T" => Ok("telemetry_points"),
        "S" => Ok("signal_points"),
        "C" => Ok("control_points"),
        "A" => Ok("adjustment_points"),
        other => Err(AppError::bad_request(format!(
            "Invalid four_remote type: '{}'. Must be T, S, C, or A",
            other
        ))),
    }
}

/// Parse protocol_mappings JSON, defaulting to empty object on null/error
fn parse_protocol_json(json_str: Option<&str>, table: &str, point_id: i64) -> serde_json::Value {
    let value = match json_str {
        Some(s) => serde_json::from_str(s).unwrap_or_else(|e| {
            tracing::error!("Parse mapping {}:{}: {}", table, point_id, e);
            json!({})
        }),
        None => json!({}),
    };
    if value.is_null() { json!({}) } else { value }
}

/// Get all mapping configurations for a channel
///
/// Returns all protocol-specific mapping configurations for the channel.
#[utoipa::path(
    get,
    path = "/api/channels/{id}/mappings",
    params(
        ("id" = u16, Path, description = "Channel identifier")
    ),
    responses((status = 200, description = "Mappings retrieved", body = crate::dto::GroupedMappings)),
    tag = "io"
)]
pub async fn get_channel_mappings_handler(
    Path(channel_id): Path<u32>,
    State(state): State<AppState>,
) -> Result<Json<SuccessResponse<crate::dto::GroupedMappings>>, AppError> {
    crate::api::handlers::point_handlers::validate_channel_exists(&state.sqlite_pool, channel_id)
        .await?;

    let tables = [
        "telemetry_points",
        "signal_points",
        "control_points",
        "adjustment_points",
    ];

    let mut results: [Vec<crate::dto::PointMappingDetail>; 4] = Default::default();

    for (i, table) in tables.iter().enumerate() {
        let query = format!(
            "SELECT point_id, signal_name, protocol_mappings FROM {} WHERE channel_id = ? ORDER BY point_id",
            table
        );
        let rows: Vec<(i64, String, Option<String>)> = sqlx::query_as(&query)
            .bind(channel_id as i64)
            .fetch_all(&state.sqlite_pool)
            .await
            .map_err(|e| {
                tracing::error!("Query {}: {}", table, e);
                AppError::internal_error("Database operation failed")
            })?;

        for (point_id, signal_name, json_str) in rows {
            let protocol_data = parse_protocol_json(json_str.as_deref(), table, point_id);
            let point_id_u32 = u32::try_from(point_id).map_err(|_| {
                AppError::internal_error(format!("point_id {} out of range", point_id))
            })?;
            results[i].push(crate::dto::PointMappingDetail {
                point_id: point_id_u32,
                signal_name,
                protocol_data,
            });
        }
    }

    let [telemetry, signal, control, adjustment] = results;
    Ok(Json(SuccessResponse::new(crate::dto::GroupedMappings {
        telemetry,
        signal,
        control,
        adjustment,
    })))
}

/// Batch update mapping configurations for a channel
///
/// Updates all protocol-specific mapping configurations for the channel in a single transaction.
/// Supports validate-only mode for pre-checking without writing.
/// Can optionally trigger automatic channel reload.
#[utoipa::path(
    put,
    path = "/api/channels/{id}/mappings",
    params(
        ("id" = u16, Path, description = "Channel identifier"),
        ("auto_reload" = bool, Query, description = "Reconcile the channel through the governed application boundary after mappings update (default: false)")
    ),
    request_body(
        content = crate::dto::MappingBatchUpdateRequest,
        description = "Batch update protocol-specific mappings with validation support",
        examples(
            ("Modbus TCP - Telemetry Points" = (
                summary = "Modbus TCP telemetry mapping (FC 3 - Read Holding Registers)",
                description = "Map telemetry points using function code 3 for reading measurements",
                value = json!({
                    "mappings": [
                        {
                            "point_id": 101,
                            "four_remote": "T",
                            "protocol_data": {
                                "slave_id": 1,
                                "function_code": 3,
                                "register_address": 100,
                                "data_type": "float32",
                                "byte_order": "ABCD"
                            }
                        },
                        {
                            "point_id": 102,
                            "four_remote": "T",
                            "protocol_data": {
                                "slave_id": 1,
                                "function_code": 3,
                                "register_address": 102,
                                "data_type": "uint16",
                                "byte_order": "AB"
                            }
                        }
                    ],
                    "validate_only": false,
                    "reload_channel": false,
                    "mode": "replace"
                })
            )),
            ("Modbus TCP - Control Points" = (
                summary = "Modbus TCP control mapping (FC 5 - Write Single Coil)",
                description = "Map control points using function code 5 for on/off control",
                value = json!({
                    "mappings": [
                        {
                            "point_id": 201,
                            "four_remote": "C",
                            "protocol_data": {
                                "slave_id": 1,
                                "function_code": 5,
                                "register_address": 0,
                                "data_type": "uint16",
                                "byte_order": "AB"
                            }
                        },
                        {
                            "point_id": 202,
                            "four_remote": "C",
                            "protocol_data": {
                                "slave_id": 1,
                                "function_code": 5,
                                "register_address": 1,
                                "data_type": "uint16",
                                "byte_order": "AB"
                            }
                        }
                    ],
                    "validate_only": false,
                    "reload_channel": true,
                    "mode": "replace"
                })
            )),
            ("Modbus TCP - Adjustment Points" = (
                summary = "Modbus TCP adjustment mapping (FC 16 - Write Multiple Registers)",
                description = "Map adjustment points using function code 16 for setpoint control",
                value = json!({
                    "mappings": [
                        {
                            "point_id": 301,
                            "four_remote": "A",
                            "protocol_data": {
                                "slave_id": 1,
                                "function_code": 16,
                                "register_address": 200,
                                "data_type": "float32",
                                "byte_order": "ABCD"
                            }
                        },
                        {
                            "point_id": 302,
                            "four_remote": "A",
                            "protocol_data": {
                                "slave_id": 1,
                                "function_code": 6,
                                "register_address": 202,
                                "data_type": "int16",
                                "byte_order": "AB"
                            }
                        }
                    ],
                    "validate_only": false,
                    "reload_channel": false,
                    "mode": "replace"
                })
            )),
            ("Modbus RTU - Mixed Types" = (
                summary = "Modbus RTU mixed point types (T/S/C/A)",
                description = "Complete example with all four remote types on RTU channel",
                value = json!({
                    "mappings": [
                        {
                            "point_id": 101,
                            "four_remote": "T",
                            "protocol_data": {
                                "slave_id": 1,
                                "function_code": 3,
                                "register_address": 0,
                                "data_type": "float32",
                                "byte_order": "ABCD"
                            }
                        },
                        {
                            "point_id": 151,
                            "four_remote": "S",
                            "protocol_data": {
                                "slave_id": 1,
                                "function_code": 2,
                                "register_address": 100,
                                "data_type": "uint16",
                                "byte_order": "AB"
                            }
                        },
                        {
                            "point_id": 201,
                            "four_remote": "C",
                            "protocol_data": {
                                "slave_id": 1,
                                "function_code": 5,
                                "register_address": 0,
                                "data_type": "uint16",
                                "byte_order": "AB"
                            }
                        },
                        {
                            "point_id": 301,
                            "four_remote": "A",
                            "protocol_data": {
                                "slave_id": 1,
                                "function_code": 16,
                                "register_address": 200,
                                "data_type": "float32",
                                "byte_order": "ABCD"
                            }
                        }
                    ],
                    "validate_only": false,
                    "reload_channel": false,
                    "mode": "replace"
                })
            )),
            ("Virtual - Expression Mapping" = (
                summary = "Virtual protocol with expression-based calculations",
                description = "Map virtual points using mathematical expressions",
                value = json!({
                    "mappings": [
                        {
                            "point_id": 101,
                            "four_remote": "T",
                            "protocol_data": {
                                "expression": "P1 + P2"
                            }
                        },
                        {
                            "point_id": 102,
                            "four_remote": "T",
                            "protocol_data": {
                                "expression": "P1 * 0.5 + P3"
                            }
                        },
                        {
                            "point_id": 103,
                            "four_remote": "T",
                            "protocol_data": {
                                "expression": "pow(P1, 2) + sqrt(P2)"
                            }
                        }
                    ],
                    "validate_only": false,
                    "reload_channel": false,
                    "mode": "replace"
                })
            )),
            ("DI/DO GPIO - Digital I/O Mapping" = (
                summary = "GPIO digital input/output mapping",
                description = "Map GPIO pins for digital I/O on industrial controllers (e.g., ECU-1170). S=Digital Input, C=Digital Output",
                value = json!({
                    "mappings": [
                        {
                            "point_id": 401,
                            "four_remote": "S",
                            "protocol_data": {
                                "gpio_number": 496
                            }
                        },
                        {
                            "point_id": 402,
                            "four_remote": "S",
                            "protocol_data": {
                                "gpio_number": 497
                            }
                        },
                        {
                            "point_id": 501,
                            "four_remote": "C",
                            "protocol_data": {
                                "gpio_number": 504
                            }
                        }
                    ],
                    "validate_only": false,
                    "reload_channel": false,
                    "mode": "replace"
                })
            )),
            ("CAN Bus - Telemetry Mapping" = (
                summary = "CAN Bus point mapping (Discover LYNK Serial CAN)",
                description = "Map CAN points using can_id + byte_offset + bit_length. `data_type` defaults to uint16, `scale`/`offset` default to 1.0/0.0. value = raw * scale + offset.",
                value = json!({
                    "mappings": [
                        {
                            "point_id": 101,
                            "four_remote": "T",
                            "protocol_data": {
                                "can_id": 854,
                                "byte_offset": 0,
                                "bit_position": 0,
                                "bit_length": 16,
                                "data_type": "uint16",
                                "scale": 0.1,
                                "offset": 0.0
                            }
                        },
                        {
                            "point_id": 102,
                            "four_remote": "T",
                            "protocol_data": {
                                "can_id": 854,
                                "byte_offset": 2,
                                "bit_position": 0,
                                "bit_length": 16,
                                "data_type": "int16",
                                "scale": 0.1,
                                "offset": 0.0
                            }
                        }
                    ],
                    "validate_only": false,
                    "reload_channel": true,
                    "mode": "replace"
                })
            )),
            ("Validation Only - Dry Run" = (
                summary = "Validate mappings without writing to database",
                description = "Use validate_only mode to check configuration before applying",
                value = json!({
                    "mappings": [
                        {
                            "point_id": 101,
                            "four_remote": "T",
                            "protocol_data": {
                                "slave_id": 1,
                                "function_code": 3,
                                "register_address": 100,
                                "data_type": "float32",
                                "byte_order": "ABCD"
                            }
                        }
                    ],
                    "validate_only": true,
                    "reload_channel": false,
                    "mode": "replace"
                })
            )),
            ("Merge Mode - Partial Update" = (
                summary = "Merge mode (default) - partial field update",
                description = "**Merge mode (default)**: Updates only specified fields while preserving all others. Example: if point 101 has {slave_id:1, function_code:3, register_address:100, data_type:\"float32\", byte_order:\"ABCD\"}, this request only updates register_address to 150, all other fields remain unchanged. The merged result is validated before saving.",
                value = json!({
                    "mappings": [
                        {
                            "point_id": 101,
                            "four_remote": "T",
                            "protocol_data": {
                                "register_address": 150,  // Only update this field
                                "data_type": "uint16"      // Only update this field
                                // Other fields (slave_id, function_code, byte_order) remain unchanged
                            }
                        },
                        {
                            "point_id": 102,
                            "four_remote": "T",
                            "protocol_data": {
                                "byte_order": "DCBA"  // Only change byte order, keep everything else
                            }
                        }
                    ],
                    "validate_only": false,
                    "reload_channel": false,
                    "mode": "merge"  // Default mode - can be omitted
                })
            ))
        )
    ),
    responses(
        (status = 200, description = "Mappings updated successfully", body = crate::dto::MappingBatchUpdateResult),
        (status = 400, description = "Validation error (invalid parameters or protocol mismatch)"),
        (status = 404, description = "Channel not found"),
        (status = 500, description = "Internal server error (database operation failed)")
    ),
    tag = "io"
)]
pub async fn update_channel_mappings_handler(
    Path(channel_id): Path<u32>,
    State(state): State<AppState>,
    Query(reload_query): Query<crate::dto::AutoReloadQuery>,
    Extension(boundary): Extension<crate::api::handlers::point_handlers::PointTopologyHttpBoundary>,
    headers: HeaderMap,
    Json(mut req): Json<crate::dto::MappingBatchUpdateRequest>,
) -> Result<Json<SuccessResponse<crate::dto::MappingBatchUpdateResult>>, AppError> {
    // 1. Verify channel exists and get protocol
    let channel_info: Option<(String, bool)> =
        sqlx::query_as("SELECT protocol, enabled FROM channels WHERE channel_id = ?")
            .bind(channel_id as i64)
            .fetch_optional(&state.sqlite_pool)
            .await
            .map_err(|e| {
                tracing::error!("Ch check: {}", e);
                AppError::internal_error("Database operation failed")
            })?;

    let Some((protocol, _is_enabled)) = channel_info else {
        return Err(AppError::internal_error(format!(
            "Channel {} not found",
            channel_id
        )));
    };

    // 1.5. Normalize protocol_data types BEFORE validation
    // This ensures validation works with properly typed numeric fields
    for item in req.mappings.iter_mut() {
        item.protocol_data = normalize_protocol_data(&protocol, &item.protocol_data);
    }

    // 2. Validate input when in Replace mode. In Merge mode, we will validate after merging with existing.
    if matches!(req.mode, crate::dto::MappingUpdateMode::Replace) {
        let validation_errors = validate_mappings(&protocol, &req.mappings);
        if !validation_errors.is_empty() {
            return Err(AppError::bad_request(format!(
                "Validation errors: {}",
                validation_errors.join("; ")
            )));
        }
    }

    if req.validate_only {
        let mut validation_errors = Vec::new();
        let mut merged_state: std::collections::HashMap<(&'static str, u32), serde_json::Value> =
            std::collections::HashMap::new();
        for (index, item) in req.mappings.iter().enumerate() {
            let table = match four_remote_to_table(&item.four_remote) {
                Ok(table) => table,
                Err(_) => {
                    validation_errors.push(format!(
                        "Item {index}: invalid four_remote {}",
                        item.four_remote
                    ));
                    continue;
                },
            };
            let existing: Option<Option<String>> = sqlx::query_scalar(&format!(
                "SELECT protocol_mappings FROM {table} WHERE channel_id = ? AND point_id = ?"
            ))
            .bind(i64::from(channel_id))
            .bind(i64::from(item.point_id))
            .fetch_optional(&state.sqlite_pool)
            .await
            .map_err(|error| AppError::internal_error(format!("DB error: {error}")))?;
            let Some(existing) = existing else {
                validation_errors.push(format!(
                    "Item {index}: point_id {} not found in {table} for channel {channel_id}",
                    item.point_id
                ));
                continue;
            };
            if matches!(req.mode, MappingUpdateMode::Merge) {
                let key = (table, item.point_id);
                let mut merged = if let Some(current) = merged_state.get(&key) {
                    current.clone()
                } else if let Some(existing) = existing {
                    match serde_json::from_str::<serde_json::Value>(&existing) {
                        Ok(value) => value,
                        Err(error) => {
                            validation_errors.push(format!(
                                "Item {index}: existing mapping for point {} is invalid JSON: {error}",
                                item.point_id
                            ));
                            continue;
                        },
                    }
                } else {
                    json!({})
                };
                match (&mut merged, &item.protocol_data) {
                    (_, serde_json::Value::Null) => merged = serde_json::Value::Null,
                    (serde_json::Value::Object(base), serde_json::Value::Object(update)) => {
                        base.extend(update.clone())
                    },
                    _ => {
                        validation_errors.push(format!(
                            "Item {index}: protocol_data must be an object or null"
                        ));
                        continue;
                    },
                }
                merged = normalize_protocol_data(&protocol, &merged);
                if let Err(error) = crate::point_topology::validate_protocol_mapping(
                    &protocol,
                    crate::point_topology::PointKind::parse(&item.four_remote)
                        .map_err(AppError::bad_request)?,
                    item.point_id,
                    &merged,
                ) {
                    validation_errors.push(error.message().to_string());
                }
                merged_state.insert(key, merged);
            }
        }
        if !validation_errors.is_empty() {
            return Err(AppError::bad_request(validation_errors.join("; ")));
        }
    }

    if req.validate_only {
        return Ok(Json(SuccessResponse::new(MappingBatchUpdateResult {
            updated_count: req.mappings.len(),
            channel_reloaded: false,
            validation_errors: vec![],
            message: format!("Validation OK for {} mappings", req.mappings.len()),
            request_id: None,
            resulting_revision: None,
            completion_audit: None,
            retryable: false,
        })));
    }

    let mode = req.mode.clone();
    let mappings = req
        .mappings
        .into_iter()
        .map(|item| {
            Ok(crate::point_topology::PointMappingMutation {
                kind: crate::point_topology::PointKind::parse(&item.four_remote)
                    .map_err(AppError::bad_request)?,
                point_id: item.point_id,
                protocol_data: item.protocol_data,
            })
        })
        .collect::<Result<Vec<_>, AppError>>()?;
    let acceptance = boundary
        .mutate(
            &headers,
            crate::point_topology::PointTopologyMutation::Mappings {
                channel_id,
                merge: matches!(&mode, MappingUpdateMode::Merge),
                mappings,
            },
        )
        .await?;
    let request_id = acceptance.request_id().to_string();
    let resulting_revision = acceptance.resulting_revision().get();
    let audit =
        crate::api::handlers::point_handlers::completion_audit(acceptance.completion_audit());
    let updated = match acceptance.into_result() {
        crate::point_topology::PointTopologyMutationResult::MappingsUpdated { mapping_count } => {
            mapping_count
        },
        _ => {
            return Err(AppError::internal_error(
                "Point topology application returned an invalid mapping receipt",
            ));
        },
    };

    // Trigger auto-reload if enabled (after mappings are updated)
    let channel_reloaded = crate::api::handlers::point_handlers::trigger_channel_reload_if_needed(
        channel_id,
        &state,
        reload_query.auto_reload,
    )
    .await;

    let mode_str = match mode {
        MappingUpdateMode::Replace => "replace",
        MappingUpdateMode::Merge => "merge",
    };
    let reload_suffix = if channel_reloaded {
        "and reconciled the channel runtime"
    } else if reload_query.auto_reload {
        "(runtime reconciliation remains pending)"
    } else {
        "(reload disabled)"
    };
    let message = format!(
        "Updated {} mapping(s) in {} mode {}",
        updated, mode_str, reload_suffix
    );

    Ok(Json(SuccessResponse::new(MappingBatchUpdateResult {
        updated_count: updated,
        channel_reloaded,
        validation_errors: vec![],
        message,
        request_id: Some(request_id),
        resulting_revision: Some(resulting_revision),
        completion_audit: Some(audit),
        retryable: false,
    })))
}

/// Validate mapping configurations based on protocol
///
/// Uses strong-typed Validator structures to automatically validate types and ranges.
/// Serde deserialization provides automatic type checking (u8, u16, u32, etc.).
/// Additional business rules are enforced after type validation.
fn validate_mappings(protocol: &str, mappings: &[crate::dto::PointMappingItem]) -> Vec<String> {
    mappings
        .iter()
        .filter_map(|mapping| {
            let kind = match crate::point_topology::PointKind::parse(&mapping.four_remote) {
                Ok(kind) => kind,
                Err(error) => return Some(error),
            };
            crate::point_topology::validate_protocol_mapping(
                protocol,
                kind,
                mapping.point_id,
                &mapping.protocol_data,
            )
            .err()
            .map(|error| error.message().to_string())
        })
        .collect()
}

/// Normalize protocol_data field types to ensure consistent JSON storage
///
/// Ensures numeric fields are stored as JSON numbers (not strings) for consistency.
/// This prevents type mismatches between GET and PUT operations.
///
/// ## Type Rules
/// ### Modbus Protocol
/// - `slave_id`: number
/// - `function_code`: number
/// - `register_address`: number
/// - `bit_position`: number (if present)
/// - `byte_order`: string (unchanged)
/// - `data_type`: string (unchanged)
///
/// ### CAN Protocol
/// - `can_id`: number
/// - `start_bit`: number
/// - `bit_length`: number
/// - `scale`: number
/// - `offset`: number
/// - `byte_order`: string (unchanged)
/// - `data_type`: string (unchanged)
/// - `signed`: boolean (unchanged)
///
/// ### Virtual Protocol
/// - No numeric normalization needed (expression-based)
///
fn normalize_protocol_data(protocol: &str, value: &serde_json::Value) -> serde_json::Value {
    use serde_json::{Number, Value};

    let Some(obj) = value.as_object() else {
        // Not an object, return as-is
        return value.clone();
    };

    // Helper: check if string value needs conversion to number
    let needs_conversion = |v: &Value| -> bool {
        matches!(v, Value::String(s) if s.parse::<i64>().is_ok() || s.parse::<f64>().is_ok())
    };

    // Helper: convert string to number if possible
    let to_number = |v: &Value| -> Option<Value> {
        match v {
            Value::Number(n) => Some(Value::Number(n.clone())),
            Value::String(s) => {
                if let Ok(n) = s.parse::<i64>() {
                    Some(Value::Number(Number::from(n)))
                } else if let Ok(f) = s.parse::<f64>() {
                    Number::from_f64(f).map(Value::Number)
                } else {
                    None
                }
            },
            _ => None,
        }
    };

    // Determine which fields need normalization based on protocol
    let numeric_fields: &[&str] = if crate::utils::is_modbus_family(protocol) {
        &[
            "slave_id",
            "function_code",
            "register_address",
            "bit_position",
        ]
    } else {
        match protocol {
            "di_do" | "gpio" | "dido" => &["gpio_number"],
            "can" => &[
                "can_id",
                "start_bit",
                "byte_offset",
                "bit_position",
                "bit_length",
                "scale",
                "offset",
            ],
            _ => {
                // Virtual or unknown protocol: no normalization needed
                return value.clone();
            },
        }
    };

    // Check if any field actually needs conversion (lazy clone optimization)
    let needs_normalization = numeric_fields
        .iter()
        .any(|field| obj.get(*field).is_some_and(needs_conversion));

    if !needs_normalization {
        // No changes needed, return original to avoid clone
        return value.clone();
    }

    // Only clone when we actually need to modify
    let mut normalized = obj.clone();
    for field in numeric_fields {
        if let Some(v) = obj.get(*field)
            && let Some(normalized_v) = to_number(v)
        {
            normalized.insert((*field).to_string(), normalized_v);
        }
    }

    Value::Object(normalized)
}
