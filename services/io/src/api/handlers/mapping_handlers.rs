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
    extract::{Path, Query, State},
    response::Json,
};
use serde::{Deserialize, Deserializer};
use serde_json::json;

/// Deserialize a u32 that may arrive as either a JSON number or a hex string like "0x351".
fn deserialize_u32_hex<'de, D: Deserializer<'de>>(deserializer: D) -> Result<u32, D::Error> {
    use serde::de::{self, Visitor};
    use std::fmt;

    struct U32HexVisitor;

    impl<'de> Visitor<'de> for U32HexVisitor {
        type Value = u32;

        fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(f, "a u32 integer or a hex string like \"0x351\"")
        }

        fn visit_u64<E: de::Error>(self, v: u64) -> Result<u32, E> {
            u32::try_from(v).map_err(|_| E::custom(format!("u32 out of range: {v}")))
        }

        fn visit_i64<E: de::Error>(self, v: i64) -> Result<u32, E> {
            u32::try_from(v).map_err(|_| E::custom(format!("negative or out-of-range: {v}")))
        }

        fn visit_str<E: de::Error>(self, s: &str) -> Result<u32, E> {
            let s = s.trim();
            if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
                u32::from_str_radix(hex, 16).map_err(E::custom)
            } else {
                s.parse::<u32>().map_err(E::custom)
            }
        }
    }

    deserializer.deserialize_any(U32HexVisitor)
}

// ============================================================================
// Validator Structures - Strong typing for runtime validation
// ============================================================================

/// Modbus mapping validator - Provides compile-time type safety through serde
///
/// This validator structure enables automatic type checking when deserializing
/// JSON mapping data. Instead of manual field-by-field validation, serde will:
/// - Reject non-numeric values for numeric fields
/// - Enforce range constraints (u8: 0-255, u16: 0-65535)
/// - Validate required fields existence
#[derive(Debug, Deserialize)]
#[allow(dead_code)] // Fields are read by serde during deserialization
struct ModbusMappingValidator {
    /// Modbus slave ID (1-247, 0 and 248-255 reserved)
    slave_id: u8,
    /// Modbus function code (1,2,3,4,5,6,15,16)
    function_code: u8,
    /// Register address (0-65535)
    register_address: u16,
    /// Data type (uint16, int16, uint32, int32, float32, float64)
    #[serde(default)]
    data_type: Option<String>,
    /// Byte order (ABCD, DCBA, BADC, CDAB, AB, BA)
    #[serde(default)]
    byte_order: Option<String>,
    /// Bit position for coil/discrete operations (optional)
    #[serde(default)]
    bit_position: Option<u8>,
}

/// Virtual mapping validator - Expression-based simulation validation
#[derive(Debug, Deserialize)]
struct VirtualMappingValidator {
    /// Mathematical expression for value calculation
    /// Supports: +, -, *, /, %, pow(), sqrt(), abs()
    /// Point references: P{id} (e.g., "P1 + P2 * 0.5")
    expression: String,
}

/// GPIO mapping validator for DI/DO protocol
///
/// Validates GPIO pin configuration for digital I/O operations.
/// Only supports Signal (S) and Control (C) point types:
/// - Signal (S): GPIO input (DI)
/// - Control (C): GPIO output (DO)
#[derive(Debug, Deserialize)]
#[allow(dead_code)] // Fields are read by serde during deserialization
struct GpioMappingValidator {
    /// GPIO pin number (e.g., 496, 504 for ECU-1170)
    gpio_number: u32,
}

/// CAN mapping validator
///
/// Validates CAN bus point mapping configuration.
/// Fields align with `CanPoint` in the CAN adapter's config.
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct CanMappingValidator {
    /// CAN frame ID — accepts either a JSON number (849) or a hex string ("0x351")
    #[serde(deserialize_with = "deserialize_u32_hex")]
    can_id: u32,
    /// Byte offset within the 8-byte CAN data field (0-7)
    byte_offset: u8,
    /// Bit start position within the byte (0-7, LSB=0)
    #[serde(default)]
    bit_position: Option<u8>,
    /// Bit length (1/8/16/32/64)
    bit_length: u8,
    /// Data type (uint8, uint16, int16, uint32, int32, float32, ascii)
    #[serde(default)]
    data_type: Option<String>,
    /// Scale factor: value = raw * scale + offset (default 1.0)
    #[serde(default)]
    scale: Option<f64>,
    /// Offset for linear transformation (default 0.0)
    #[serde(default)]
    offset: Option<f64>,
}

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

    // Structural validation: table & point existence
    let mut structure_errors = Vec::new();
    for (idx, item) in req.mappings.iter().enumerate() {
        let table = match four_remote_to_table(&item.four_remote) {
            Ok(t) => t,
            Err(_) => {
                structure_errors.push(format!(
                    "Item {}: invalid four_remote {}",
                    idx, item.four_remote
                ));
                continue;
            },
        };
        let exists: Option<(i64,)> = sqlx::query_as(&format!(
            "SELECT point_id FROM {} WHERE channel_id = ? AND point_id = ?",
            table
        ))
        .bind(channel_id as i64)
        .bind(item.point_id as i64)
        .fetch_optional(&state.sqlite_pool)
        .await
        .map_err(|e| AppError::internal_error(format!("DB error: {}", e)))?;
        if exists.is_none() {
            structure_errors.push(format!(
                "Item {}: point_id {} not found in {} for channel {}",
                idx, item.point_id, table, channel_id
            ));
        }
    }
    if !structure_errors.is_empty() {
        return Err(AppError::bad_request(structure_errors.join("; ")));
    }

    if req.validate_only {
        return Ok(Json(SuccessResponse::new(MappingBatchUpdateResult {
            updated_count: req.mappings.len(),
            channel_reloaded: false,
            validation_errors: vec![],
            message: format!("Validation OK for {} mappings", req.mappings.len()),
        })));
    }

    let mut tx = state
        .sqlite_pool
        .begin()
        .await
        .map_err(|e| AppError::internal_error(format!("Failed to start transaction: {}", e)))?;

    let mut updated = 0usize;
    for item in &req.mappings {
        let table = four_remote_to_table(&item.four_remote)?;

        // Merge/Replace
        let mut new_json = match req.mode {
            MappingUpdateMode::Replace => Some(item.protocol_data.clone()),
            MappingUpdateMode::Merge => {
                let existing: Option<(Option<String>,)> = sqlx::query_as(&format!(
                    "SELECT protocol_mappings FROM {} WHERE channel_id = ? AND point_id = ?",
                    table
                ))
                .bind(channel_id as i64)
                .bind(item.point_id as i64)
                .fetch_optional(&mut *tx)
                .await
                .map_err(|e| AppError::internal_error(format!("DB read error: {}", e)))?;

                let mut base = existing
                    .and_then(|row| row.0)
                    .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
                    .unwrap_or(json!({}));
                if let serde_json::Value::Object(ref mut base_map) = base {
                    // Clone once and extend (avoids per-field clone in the loop)
                    if let serde_json::Value::Object(new_map) = item.protocol_data.clone() {
                        base_map.extend(new_map);
                    }
                }
                Some(base)
            },
        };

        // Normalize merged data before validation (may contain old un-normalized data from database)
        new_json = new_json.map(|v| normalize_protocol_data(&protocol, &v));

        // For Merge mode, validate the merged JSON before writing
        if matches!(req.mode, crate::dto::MappingUpdateMode::Merge)
            && let Some(ref merged) = new_json
        {
            let merged_item = crate::dto::PointMappingItem {
                point_id: item.point_id,
                four_remote: item.four_remote.clone(),
                protocol_data: merged.clone(),
            };
            let errors = validate_mappings(&protocol, &[merged_item]);
            if !errors.is_empty() {
                return Err(AppError::bad_request(format!(
                    "Validation errors: {}",
                    errors.join("; ")
                )));
            }
        }

        // Serialize the normalized JSON for database storage
        let serialized = match new_json {
            Some(serde_json::Value::Object(ref m)) if m.is_empty() => None,
            Some(v) => Some(serde_json::to_string(&v).unwrap_or("{}".to_string())),
            None => None,
        };

        sqlx::query(&format!(
            "UPDATE {} SET protocol_mappings = ? WHERE channel_id = ? AND point_id = ?",
            table
        ))
        .bind(serialized)
        .bind(channel_id as i64)
        .bind(item.point_id as i64)
        .execute(&mut *tx)
        .await
        .map_err(|e| AppError::internal_error(format!("DB update error: {}", e)))?;
        updated += 1;
    }

    tx.commit()
        .await
        .map_err(|e| AppError::internal_error(format!("Commit failed: {}", e)))?;

    // Trigger auto-reload if enabled (after mappings are updated)
    let channel_reloaded = crate::api::handlers::point_handlers::trigger_channel_reload_if_needed(
        channel_id,
        &state,
        reload_query.auto_reload,
    )
    .await;

    let mode_str = match req.mode {
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
    })))
}

/// Validate mapping configurations based on protocol
///
/// Uses strong-typed Validator structures to automatically validate types and ranges.
/// Serde deserialization provides automatic type checking (u8, u16, u32, etc.).
/// Additional business rules are enforced after type validation.
fn validate_mappings(protocol: &str, mappings: &[crate::dto::PointMappingItem]) -> Vec<String> {
    let mut errors = Vec::new();

    for mapping in mappings {
        // Allow clearing mapping (unbind operation)
        // null or {} means clear the mapping - skip validation
        if mapping.protocol_data.is_null()
            || mapping
                .protocol_data
                .as_object()
                .is_some_and(|o| o.is_empty())
        {
            continue;
        }

        if crate::utils::is_modbus_family(protocol) {
            // Attempt strong-typed deserialization - automatic type/range validation
            match serde_json::from_value::<ModbusMappingValidator>(mapping.protocol_data.clone()) {
                Ok(validated) => {
                    // ✅ Type validation passed, now check business rules

                    // 1. Slave ID range (1-247, 0 and 248-255 reserved by Modbus spec)
                    if validated.slave_id == 0 || validated.slave_id >= 248 {
                        errors.push(format!(
                            "Point {}: slave_id {} invalid (must be 1-247, 0 and 248-255 are reserved)",
                            mapping.point_id, validated.slave_id
                        ));
                    }

                    // 2. Function code validity
                    let valid_fcs = [1u8, 2, 3, 4, 5, 6, 15, 16];
                    if !valid_fcs.contains(&validated.function_code) {
                        errors.push(format!(
                            "Point {}: function_code {} invalid (valid: 1,2,3,4,5,6,15,16)",
                            mapping.point_id, validated.function_code
                        ));
                    }

                    // 3. Optional data type enumeration
                    if let Some(ref dt) = validated.data_type {
                        let valid_types = [
                            "bool", "boolean", "uint16", "int16", "uint32", "int32", "float32",
                            "float64",
                        ];
                        if !valid_types.contains(&dt.as_str()) {
                            errors.push(format!(
                                "Point {}: data_type '{}' invalid (valid: {})",
                                mapping.point_id,
                                dt,
                                valid_types.join(", ")
                            ));
                        }
                    }

                    // 4. Optional byte order enumeration
                    if let Some(ref bo) = validated.byte_order {
                        let valid_orders = ["ABCD", "DCBA", "BADC", "CDAB", "AB", "BA"];
                        if !valid_orders.contains(&bo.as_str()) {
                            errors.push(format!(
                                "Point {}: byte_order '{}' invalid (valid: {})",
                                mapping.point_id,
                                bo,
                                valid_orders.join(", ")
                            ));
                        }
                    }

                    // 5. Business rule: Function code must match point type
                    let fc_error = validate_modbus_function_code_match(
                        validated.function_code,
                        mapping.four_remote.as_str(),
                        mapping.point_id,
                    );
                    if let Some(err) = fc_error {
                        errors.push(err);
                    }
                },
                Err(e) => {
                    // ❌ Type validation failed (wrong type, missing field, out of range)
                    errors.push(format!(
                        "Point {}: Modbus mapping validation failed - {}",
                        mapping.point_id, e
                    ));
                },
            }
            continue;
        }

        match protocol.to_lowercase().as_str() {
            "virtual" => {
                // Virtual protocol validation
                match serde_json::from_value::<VirtualMappingValidator>(
                    mapping.protocol_data.clone(),
                ) {
                    Ok(validated) => {
                        // Check expression is not empty
                        if validated.expression.trim().is_empty() {
                            errors.push(format!(
                                "Point {}: expression cannot be empty",
                                mapping.point_id
                            ));
                        }
                    },
                    Err(e) => {
                        errors.push(format!(
                            "Point {}: Virtual mapping validation failed - {}",
                            mapping.point_id, e
                        ));
                    },
                }
            },
            "di_do" | "gpio" | "dido" => {
                // GPIO/DI-DO protocol validation
                match serde_json::from_value::<GpioMappingValidator>(mapping.protocol_data.clone())
                {
                    Ok(validated) => {
                        // 1. GPIO number range validation (typical embedded Linux range)
                        if validated.gpio_number > 1023 {
                            errors.push(format!(
                                "Point {}: gpio_number {} out of range (0-1023)",
                                mapping.point_id, validated.gpio_number
                            ));
                        }

                        // 2. GPIO only supports Signal (input) and Control (output)
                        // Use eq_ignore_ascii_case to avoid String allocation from to_uppercase()
                        if !mapping.four_remote.eq_ignore_ascii_case("S")
                            && !mapping.four_remote.eq_ignore_ascii_case("C")
                        {
                            errors.push(format!(
                                "Point {}: GPIO only supports Signal (S) and Control (C) types, got: {}",
                                mapping.point_id, mapping.four_remote
                            ));
                        }
                    },
                    Err(e) => {
                        errors.push(format!(
                            "Point {}: GPIO mapping validation failed - {}",
                            mapping.point_id, e
                        ));
                    },
                }
            },
            "can" => {
                match serde_json::from_value::<CanMappingValidator>(mapping.protocol_data.clone()) {
                    Ok(validated) => {
                        // 1. byte_offset range (0-7 for standard CAN 8-byte frame)
                        if validated.byte_offset > 7 {
                            errors.push(format!(
                                "Point {}: byte_offset {} out of range (0-7)",
                                mapping.point_id, validated.byte_offset
                            ));
                        }

                        // 2. bit_position range (0-7)
                        if let Some(bp) = validated.bit_position
                            && bp > 7
                        {
                            errors.push(format!(
                                "Point {}: bit_position {} out of range (0-7)",
                                mapping.point_id, bp
                            ));
                        }

                        // 3. bit_length must be non-zero and reasonable
                        if validated.bit_length == 0 {
                            errors.push(format!(
                                "Point {}: bit_length must be > 0",
                                mapping.point_id
                            ));
                        }

                        // 4. data_type enumeration
                        if let Some(ref dt) = validated.data_type {
                            let valid_types = [
                                "uint8", "uint16", "int16", "uint32", "int32", "float32", "ascii",
                            ];
                            if !valid_types.contains(&dt.as_str()) {
                                errors.push(format!(
                                    "Point {}: data_type '{}' invalid (valid: {})",
                                    mapping.point_id,
                                    dt,
                                    valid_types.join(", ")
                                ));
                            }
                        }
                    },
                    Err(e) => {
                        errors.push(format!(
                            "Point {}: CAN mapping validation failed - {}",
                            mapping.point_id, e
                        ));
                    },
                }
            },
            other => {
                errors.push(format!("Unsupported protocol: {}", other));
                break; // Protocol error affects all mappings
            },
        }
    }

    errors
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

/// Validate Modbus function code matches point type (business rule)
///
/// Enforces the Modbus specification requirement that read/write function codes
/// must match the point's data direction:
/// - T/S points (read-only): FC 1/2/3/4
/// - C points (write coils): FC 5/6
/// - A points (write registers): FC 6/16
///
/// Returns Some(error_message) if validation fails, None if valid.
fn validate_modbus_function_code_match(
    function_code: u8,
    four_remote: &str,
    point_id: u32,
) -> Option<String> {
    match four_remote {
        "T" | "S" => {
            // Telemetry/Signal points must use read function codes
            if ![1, 2, 3, 4].contains(&function_code) {
                return Some(format!(
                    "Point {}: {} point requires read FC (1/2/3/4), got FC {} (write)",
                    point_id, four_remote, function_code
                ));
            }
        },
        "C" => {
            // Control points can use coil write (5/15) or register write (6/16)
            if ![5, 6, 15, 16].contains(&function_code) {
                return Some(format!(
                    "Point {}: C point requires write FC (5/6/15/16), got FC {}",
                    point_id, function_code
                ));
            }
        },
        "A" => {
            // Adjustment points must use register write function codes
            if ![6, 16].contains(&function_code) {
                return Some(format!(
                    "Point {}: A point requires register write FC (6/16), got FC {}",
                    point_id, function_code
                ));
            }
        },
        _ => {
            // Invalid four_remote type (should be caught by structural validation)
            return Some(format!(
                "Point {}: invalid four_remote type '{}'",
                point_id, four_remote
            ));
        },
    }

    None // Validation passed
}
