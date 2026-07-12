#![allow(clippy::disallowed_methods)]

//! Single-point CRUD handlers (Create, Update, Delete)

use crate::api::routes::AppState;
use crate::core::config::TelemetryPoint;
use crate::dto::{AppError, SuccessResponse};
use axum::{
    extract::{Path, Query, State},
    response::Json,
};

use super::point_helpers::{
    point_type_to_table, trigger_channel_reload_if_needed, validate_channel_exists,
    validate_point_uniqueness,
};
use super::point_types::{PointCrudResult, PointUpdateRequest};

// ----------------------------------------------------------------------------
// Helper: Extract common fields from point creation payload
// ----------------------------------------------------------------------------

/// Common fields for S/C/A point creation
struct CreatePointFields {
    signal_name: String,
    scale: f64,
    offset: f64,
    unit: String,
    reverse: bool,
    data_type: String,
    description: String,
}

/// Extract and validate common fields from a JSON payload for point creation
fn extract_create_fields(
    payload: &serde_json::Value,
    point_id: u32,
    default_data_type: &str,
) -> Result<CreatePointFields, AppError> {
    let payload_point_id = payload
        .get("point_id")
        .and_then(|v| v.as_u64())
        .ok_or_else(|| AppError::bad_request("Missing field: point_id"))?;
    let payload_point_id = u32::try_from(payload_point_id).map_err(|_| {
        AppError::bad_request(format!("point_id {} out of range", payload_point_id))
    })?;

    if payload_point_id != point_id {
        return Err(AppError::bad_request(format!(
            "Point ID mismatch: path has {}, body has {}",
            point_id, payload_point_id
        )));
    }

    let signal_name = payload
        .get("signal_name")
        .and_then(|v| v.as_str())
        .ok_or_else(|| AppError::bad_request("Missing field: signal_name"))?
        .to_string();

    Ok(CreatePointFields {
        signal_name,
        scale: payload.get("scale").and_then(|v| v.as_f64()).unwrap_or(1.0),
        offset: payload
            .get("offset")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0),
        unit: payload
            .get("unit")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        reverse: payload
            .get("reverse")
            .and_then(|v| {
                v.as_str()
                    .and_then(|s| s.parse::<bool>().ok())
                    .or_else(|| v.as_bool())
            })
            .unwrap_or(false),
        data_type: payload
            .get("data_type")
            .and_then(|v| v.as_str())
            .unwrap_or(default_data_type)
            .to_string(),
        description: payload
            .get("description")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
    })
}

// ----------------------------------------------------------------------------
// Create Point Handlers
// ----------------------------------------------------------------------------

/// Create a new telemetry point (Telemetry / type "T").
///
/// T points are read-only floating-point measurements (temperature, pressure, flow,
/// humidity, etc.) polled periodically from the device. Writes to the `telemetry_points`
/// table and registers the corresponding SHM slot (if the channel is already running).
/// Register address, byte order, linear scaling, and unit are supplied in the request.
/// `point_id` must be unique within a channel.
#[utoipa::path(
    post,
    path = "/api/channels/{channel_id}/T/points/{point_id}",
    params(
        ("channel_id" = u32, Path, description = "Channel identifier"),
        ("point_id" = u32, Path, description = "Point identifier"),
        ("auto_reload" = bool, Query, description = "Reconcile the channel through the governed application boundary after creation (default: false)")
    ),
    responses(
        (status = 200, description = "Point created", body = PointCrudResult),
        (status = 400, description = "Invalid request"),
        (status = 404, description = "Channel not found"),
        (status = 409, description = "Point ID already exists")
    ),
    tag = "io"
)]
pub async fn create_telemetry_point_handler(
    Path((channel_id, point_id)): Path<(u32, u32)>,
    State(state): State<AppState>,
    Query(reload_query): Query<crate::dto::AutoReloadQuery>,
    Json(payload): Json<serde_json::Value>,
) -> Result<Json<SuccessResponse<PointCrudResult>>, AppError> {
    let point: TelemetryPoint = serde_json::from_value(payload)
        .map_err(|e| AppError::bad_request(format!("Invalid request body: {}", e)))?;
    validate_channel_exists(&state.sqlite_pool, channel_id).await?;

    if point.base.point_id != point_id {
        return Err(AppError::bad_request(format!(
            "Point ID mismatch: path has {}, body has {}",
            point_id, point.base.point_id
        )));
    }

    validate_point_uniqueness(&state.sqlite_pool, channel_id, "telemetry_points", point_id).await?;

    sqlx::query(
        "INSERT INTO telemetry_points
         (channel_id, point_id, signal_name, scale, offset, unit, data_type, reverse, description, protocol_mappings)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, NULL)",
    )
    .bind(channel_id as i64)
    .bind(point.base.point_id as i64)
    .bind(&point.base.signal_name)
    .bind(point.scale)
    .bind(point.offset)
    .bind(&point.base.unit)
    .bind(&point.data_type)
    .bind(point.reverse)
    .bind(&point.base.description)
    .execute(&state.sqlite_pool)
    .await
    .map_err(|e| {
        tracing::error!("Create T point: {}", e);
        AppError::internal_error("Failed to create point")
    })?;

    tracing::debug!("Ch{}:T:{} created", channel_id, point_id);
    trigger_channel_reload_if_needed(channel_id, &state, reload_query.auto_reload).await;

    Ok(Json(SuccessResponse::new(PointCrudResult {
        channel_id,
        point_type: "T".to_string(),
        point_id,
        signal_name: point.base.signal_name.clone(),
        message: "Telemetry point created successfully".to_string(),
    })))
}

/// Create a new signal point (Signal / type "S").
///
/// S points are read-only discrete inputs / status bits (circuit breaker on/off,
/// run/fault flags, alarm bits, etc.) read from device discrete inputs. Compared to T,
/// S has an extra `normal_state` field indicating whether the normal state is 0 or 1 —
/// alarm rules use this to detect state inversion. All other behavior is the same as T.
#[utoipa::path(
    post,
    path = "/api/channels/{channel_id}/S/points/{point_id}",
    params(
        ("channel_id" = u32, Path, description = "Channel identifier"),
        ("point_id" = u32, Path, description = "Point identifier"),
        ("auto_reload" = bool, Query, description = "Reconcile the channel through the governed application boundary after creation (default: false)")
    ),
    responses(
        (status = 200, description = "Point created", body = PointCrudResult),
        (status = 400, description = "Invalid request"),
        (status = 404, description = "Channel not found"),
        (status = 409, description = "Point ID already exists")
    ),
    tag = "io"
)]
pub async fn create_signal_point_handler(
    Path((channel_id, point_id)): Path<(u32, u32)>,
    State(state): State<AppState>,
    Query(reload_query): Query<crate::dto::AutoReloadQuery>,
    Json(payload): Json<serde_json::Value>,
) -> Result<Json<SuccessResponse<PointCrudResult>>, AppError> {
    let fields = extract_create_fields(&payload, point_id, "bool")?;
    validate_channel_exists(&state.sqlite_pool, channel_id).await?;
    validate_point_uniqueness(&state.sqlite_pool, channel_id, "signal_points", point_id).await?;

    let normal_state = payload
        .get("normal_state")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);

    sqlx::query(
        "INSERT INTO signal_points
         (channel_id, point_id, signal_name, scale, offset, unit, reverse, normal_state, data_type, description, protocol_mappings)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, NULL)",
    )
    .bind(channel_id as i64)
    .bind(point_id as i64)
    .bind(&fields.signal_name)
    .bind(fields.scale)
    .bind(fields.offset)
    .bind(&fields.unit)
    .bind(fields.reverse)
    .bind(normal_state)
    .bind(&fields.data_type)
    .bind(&fields.description)
    .execute(&state.sqlite_pool)
    .await
    .map_err(|e| {
        tracing::error!("Create S point: {}", e);
        AppError::internal_error("Failed to create point")
    })?;

    tracing::debug!("Ch{}:S:{} created", channel_id, point_id);
    trigger_channel_reload_if_needed(channel_id, &state, reload_query.auto_reload).await;

    Ok(Json(SuccessResponse::new(PointCrudResult {
        channel_id,
        point_type: "S".to_string(),
        point_id,
        signal_name: fields.signal_name,
        message: "Signal point created successfully".to_string(),
    })))
}

/// Internal: create control or adjustment point (identical schema)
async fn create_ca_point_inner(
    channel_id: u32,
    point_type: &str,
    point_id: u32,
    state: AppState,
    reload_query: crate::dto::AutoReloadQuery,
    payload: serde_json::Value,
    default_data_type: &str,
) -> Result<Json<SuccessResponse<PointCrudResult>>, AppError> {
    let table = point_type_to_table(point_type)?;
    let fields = extract_create_fields(&payload, point_id, default_data_type)?;
    validate_channel_exists(&state.sqlite_pool, channel_id).await?;
    validate_point_uniqueness(&state.sqlite_pool, channel_id, table, point_id).await?;

    let adjustment_constraints = if point_type == "A" {
        let minimum = payload.get("min_value").and_then(serde_json::Value::as_f64);
        let maximum = payload.get("max_value").and_then(serde_json::Value::as_f64);
        let step = payload
            .get("step")
            .and_then(serde_json::Value::as_f64)
            .unwrap_or(1.0);
        aether_domain::CommandConstraints::new(minimum, maximum, Some(step)).map_err(|error| {
            AppError::bad_request(format!("Invalid adjustment constraints: {error}"))
        })?;
        Some((minimum, maximum, step))
    } else {
        None
    };

    if let Some((minimum, maximum, step)) = adjustment_constraints {
        sqlx::query(
            "INSERT INTO adjustment_points
             (channel_id, point_id, signal_name, scale, offset, unit, reverse, data_type,
              description, protocol_mappings, min_value, max_value, step)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, NULL, ?, ?, ?)",
        )
        .bind(channel_id as i64)
        .bind(point_id as i64)
        .bind(&fields.signal_name)
        .bind(fields.scale)
        .bind(fields.offset)
        .bind(&fields.unit)
        .bind(fields.reverse)
        .bind(&fields.data_type)
        .bind(&fields.description)
        .bind(minimum)
        .bind(maximum)
        .bind(step)
        .execute(&state.sqlite_pool)
        .await
        .map_err(|error| {
            tracing::error!("Create adjustment point: {error}");
            AppError::internal_error("Failed to create point")
        })?;
    } else {
        let query = format!(
        "INSERT INTO {}
         (channel_id, point_id, signal_name, scale, offset, unit, reverse, data_type, description, protocol_mappings)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, NULL)",
        table
    );
        sqlx::query(&query)
            .bind(channel_id as i64)
            .bind(point_id as i64)
            .bind(&fields.signal_name)
            .bind(fields.scale)
            .bind(fields.offset)
            .bind(&fields.unit)
            .bind(fields.reverse)
            .bind(&fields.data_type)
            .bind(&fields.description)
            .execute(&state.sqlite_pool)
            .await
            .map_err(|e| {
                tracing::error!("Create {} point: {}", point_type, e);
                AppError::internal_error("Failed to create point")
            })?;
    }

    let type_name = match point_type {
        "C" => "Control",
        "A" => "Adjustment",
        _ => point_type,
    };
    tracing::debug!("Ch{}:{}:{} created", channel_id, point_type, point_id);
    trigger_channel_reload_if_needed(channel_id, &state, reload_query.auto_reload).await;

    Ok(Json(SuccessResponse::new(PointCrudResult {
        channel_id,
        point_type: point_type.to_string(),
        point_id,
        signal_name: fields.signal_name,
        message: format!("{} point created successfully", type_name),
    })))
}

/// Create a new control point (Control / type "C").
///
/// C points are writable discrete outputs (FC05 write coil) used for discrete control
/// commands such as start/stop and open/close. They are the terminal of the
/// automation → SHM C slot → UDS notify → io → device write path. The point is
/// writable immediately after creation, but a M2C routing entry pointing to an
/// `instance.action_point` must exist before commands are dispatched to the device.
#[utoipa::path(
    post,
    path = "/api/channels/{channel_id}/C/points/{point_id}",
    params(
        ("channel_id" = u32, Path, description = "Channel identifier"),
        ("point_id" = u32, Path, description = "Point identifier"),
        ("auto_reload" = bool, Query, description = "Reconcile the channel through the governed application boundary after creation (default: false)")
    ),
    responses(
        (status = 200, description = "Point created", body = PointCrudResult),
        (status = 400, description = "Invalid request"),
        (status = 404, description = "Channel not found"),
        (status = 409, description = "Point ID already exists")
    ),
    tag = "io"
)]
pub async fn create_control_point_handler(
    Path((channel_id, point_id)): Path<(u32, u32)>,
    State(state): State<AppState>,
    Query(reload_query): Query<crate::dto::AutoReloadQuery>,
    Json(payload): Json<serde_json::Value>,
) -> Result<Json<SuccessResponse<PointCrudResult>>, AppError> {
    create_ca_point_inner(
        channel_id,
        "C",
        point_id,
        state,
        reload_query,
        payload,
        "bool",
    )
    .await
}

/// Create a new adjustment point (Adjustment / type "A").
///
/// A points are writable floating-point outputs (FC06 write single register / FC16
/// write multiple registers) used for continuous setpoint control such as power
/// setpoint, frequency adjustment, and voltage setpoint. A is the floating-point
/// counterpart of C; the only difference is the value domain (C is 0/1, A is float).
/// All other rules are the same (M2C routing required before commands reach the device).
#[utoipa::path(
    post,
    path = "/api/channels/{channel_id}/A/points/{point_id}",
    params(
        ("channel_id" = u32, Path, description = "Channel identifier"),
        ("point_id" = u32, Path, description = "Point identifier"),
        ("auto_reload" = bool, Query, description = "Reconcile the channel through the governed application boundary after creation (default: false)")
    ),
    responses(
        (status = 200, description = "Point created", body = PointCrudResult),
        (status = 400, description = "Invalid request"),
        (status = 404, description = "Channel not found"),
        (status = 409, description = "Point ID already exists")
    ),
    tag = "io"
)]
pub async fn create_adjustment_point_handler(
    Path((channel_id, point_id)): Path<(u32, u32)>,
    State(state): State<AppState>,
    Query(reload_query): Query<crate::dto::AutoReloadQuery>,
    Json(payload): Json<serde_json::Value>,
) -> Result<Json<SuccessResponse<PointCrudResult>>, AppError> {
    create_ca_point_inner(
        channel_id,
        "A",
        point_id,
        state,
        reload_query,
        payload,
        "int16",
    )
    .await
}

// ----------------------------------------------------------------------------
// Update Point Handler (Universal for all types)
// ----------------------------------------------------------------------------

/// Update the definition of a point of any type (unified entry point).
///
/// Paired with the four create endpoints — `point_type` in the path determines which
/// table to update. Updatable fields include register address, scale factor, unit, and
/// alarm limits. Changing `point_id` or `channel_id` is not allowed (delete and
/// recreate instead, to avoid breaking SHM slot mappings). The new configuration takes
/// effect on the next poll cycle; no channel restart is required.
/// All four point tables share the same updatable columns,
/// so a single parameterized query works for all types.
pub(super) async fn update_point_handler_inner(
    channel_id: u32,
    point_type: &str,
    point_id: u32,
    state: AppState,
    reload_query: crate::dto::AutoReloadQuery,
    update: PointUpdateRequest,
) -> Result<Json<SuccessResponse<PointCrudResult>>, AppError> {
    let point_type_upper = point_type.to_ascii_uppercase();
    let table = point_type_to_table(point_type)?;
    validate_channel_exists(&state.sqlite_pool, channel_id).await?;

    let has_update = update.signal_name.is_some()
        || update.description.is_some()
        || update.unit.is_some()
        || update.scale.is_some()
        || update.offset.is_some()
        || update.data_type.is_some()
        || update.reverse.is_some()
        || update.min_value.is_some()
        || update.max_value.is_some()
        || update.step.is_some();

    if !has_update {
        return Err(AppError::bad_request("No fields provided for update"));
    }

    let signal_name = if point_type_upper == "A" {
        let existing = sqlx::query_as::<_, (Option<f64>, Option<f64>, f64)>(
            "SELECT min_value, max_value, step FROM adjustment_points
             WHERE channel_id = ? AND point_id = ?",
        )
        .bind(channel_id as i64)
        .bind(point_id as i64)
        .fetch_optional(&state.sqlite_pool)
        .await
        .map_err(|error| {
            tracing::error!("Read adjustment constraints: {error}");
            AppError::internal_error("Failed to read point")
        })?
        .ok_or_else(|| {
            AppError::not_found(format!(
                "Point {} (type A) not found in channel {}",
                point_id, channel_id
            ))
        })?;
        let minimum = update.min_value.or(existing.0);
        let maximum = update.max_value.or(existing.1);
        let step = update.step.unwrap_or(existing.2);
        aether_domain::CommandConstraints::new(minimum, maximum, Some(step)).map_err(|error| {
            AppError::bad_request(format!("Invalid adjustment constraints: {error}"))
        })?;

        sqlx::query_scalar::<_, String>(
            "UPDATE adjustment_points SET
                signal_name = COALESCE(?, signal_name),
                description = COALESCE(?, description),
                unit = COALESCE(?, unit),
                scale = COALESCE(?, scale),
                offset = COALESCE(?, offset),
                data_type = COALESCE(?, data_type),
                reverse = COALESCE(?, reverse),
                min_value = ?, max_value = ?, step = ?
             WHERE channel_id = ? AND point_id = ?
             RETURNING signal_name",
        )
        .bind(update.signal_name.as_deref())
        .bind(update.description.as_deref())
        .bind(update.unit.as_deref())
        .bind(update.scale)
        .bind(update.offset)
        .bind(update.data_type.as_deref())
        .bind(update.reverse)
        .bind(minimum)
        .bind(maximum)
        .bind(step)
        .bind(channel_id as i64)
        .bind(point_id as i64)
        .fetch_one(&state.sqlite_pool)
        .await
        .map_err(|error| {
            tracing::error!("Update adjustment point: {error}");
            AppError::internal_error("Failed to update point")
        })?
    } else {
        if update.min_value.is_some() || update.max_value.is_some() || update.step.is_some() {
            return Err(AppError::bad_request(
                "min_value, max_value, and step are only valid for adjustment points",
            ));
        }
        let query = format!(
            "UPDATE {} SET
            signal_name = COALESCE(?, signal_name),
            description = COALESCE(?, description),
            unit = COALESCE(?, unit),
            scale = COALESCE(?, scale),
            offset = COALESCE(?, offset),
            data_type = COALESCE(?, data_type),
            reverse = COALESCE(?, reverse)
        WHERE channel_id = ? AND point_id = ?
        RETURNING signal_name",
            table
        );

        sqlx::query_scalar::<_, String>(&query)
            .bind(update.signal_name.as_deref())
            .bind(update.description.as_deref())
            .bind(update.unit.as_deref())
            .bind(update.scale)
            .bind(update.offset)
            .bind(update.data_type.as_deref())
            .bind(update.reverse)
            .bind(channel_id as i64)
            .bind(point_id as i64)
            .fetch_optional(&state.sqlite_pool)
            .await
            .map_err(|e| {
                tracing::error!("Update {} point: {}", table, e);
                AppError::internal_error("Failed to update point")
            })?
            .ok_or_else(|| {
                AppError::not_found(format!(
                    "Point {} (type {}) not found in channel {}",
                    point_id, point_type_upper, channel_id
                ))
            })?
    };

    tracing::debug!("Ch{}:{}:{} updated", channel_id, point_type_upper, point_id);
    trigger_channel_reload_if_needed(channel_id, &state, reload_query.auto_reload).await;

    Ok(Json(SuccessResponse::new(PointCrudResult {
        channel_id,
        point_type: point_type_upper,
        point_id,
        signal_name,
        message: "Point updated successfully".to_string(),
    })))
}

// ----------------------------------------------------------------------------
// Delete Point Handler
// ----------------------------------------------------------------------------

/// Delete a point of any type.
///
/// Removes the row from the corresponding `{type}_points` table and clears the
/// associated `protocol_mappings`. **The corresponding SHM slot becomes idle** (not
/// immediately reclaimed, to keep `routing_hash` stable and reduce automation rebuild
/// storms). If the point is the target of a M2C routing entry, that route becomes
/// stale but is not cascade-deleted — orphaned routing entries must be cleaned up
/// separately.
pub(super) async fn delete_point_handler_inner(
    channel_id: u32,
    point_type: &str,
    point_id: u32,
    state: AppState,
    reload_query: crate::dto::AutoReloadQuery,
) -> Result<Json<SuccessResponse<PointCrudResult>>, AppError> {
    let point_type_upper = point_type.to_ascii_uppercase();
    let table = point_type_to_table(point_type)?;
    validate_channel_exists(&state.sqlite_pool, channel_id).await?;

    // Get point info before deletion (for response)
    let query = format!(
        "SELECT signal_name FROM {} WHERE channel_id = ? AND point_id = ?",
        table
    );
    let existing: Option<(String,)> = sqlx::query_as(&query)
        .bind(channel_id as i64)
        .bind(point_id as i64)
        .fetch_optional(&state.sqlite_pool)
        .await
        .map_err(|e| {
            tracing::error!("Point check: {}", e);
            AppError::internal_error("Database operation failed")
        })?;

    let signal_name = existing
        .ok_or_else(|| {
            AppError::not_found(format!(
                "Point {} (type {}) not found in channel {}",
                point_id, point_type_upper, channel_id
            ))
        })?
        .0;

    // Delete point
    let delete_sql = format!(
        "DELETE FROM {} WHERE channel_id = ? AND point_id = ?",
        table
    );
    sqlx::query(&delete_sql)
        .bind(channel_id as i64)
        .bind(point_id as i64)
        .execute(&state.sqlite_pool)
        .await
        .map_err(|e| {
            tracing::error!("Delete point: {}", e);
            AppError::internal_error("Failed to delete point")
        })?;

    tracing::debug!("Ch{}:{}:{} deleted", channel_id, point_type_upper, point_id);

    trigger_channel_reload_if_needed(channel_id, &state, reload_query.auto_reload).await;

    Ok(Json(SuccessResponse::new(PointCrudResult {
        channel_id,
        point_type: point_type_upper,
        point_id,
        signal_name,
        message: "Point deleted successfully".to_string(),
    })))
}

// ============================================================================
// Type-specific wrapper handlers (delegate to *_inner functions)
// ============================================================================

// --- PUT wrappers ---

/// Update telemetry point
#[utoipa::path(
    put,
    path = "/api/channels/{channel_id}/T/points/{point_id}",
    params(
        ("channel_id" = u32, Path, description = "Channel identifier"),
        ("point_id" = u32, Path, description = "Telemetry point identifier"),
        ("auto_reload" = bool, Query, description = "Reconcile the channel through the governed application boundary after update (default: false)")
    ),
    request_body(content = PointUpdateRequest, description = "Telemetry point fields to update"),
    responses(
        (status = 200, description = "Telemetry point updated", body = PointCrudResult),
        (status = 400, description = "Invalid update"),
        (status = 404, description = "Channel or telemetry point not found")
    ),
    tag = "io"
)]
pub async fn update_telemetry_point_handler(
    Path((channel_id, point_id)): Path<(u32, u32)>,
    State(state): State<AppState>,
    Query(reload_query): Query<crate::dto::AutoReloadQuery>,
    Json(update): Json<PointUpdateRequest>,
) -> Result<Json<SuccessResponse<PointCrudResult>>, AppError> {
    update_point_handler_inner(channel_id, "T", point_id, state, reload_query, update).await
}

/// Update signal point
#[utoipa::path(
    put,
    path = "/api/channels/{channel_id}/S/points/{point_id}",
    params(
        ("channel_id" = u32, Path, description = "Channel identifier"),
        ("point_id" = u32, Path, description = "Signal point identifier"),
        ("auto_reload" = bool, Query, description = "Reconcile the channel through the governed application boundary after update (default: false)")
    ),
    request_body(content = PointUpdateRequest, description = "Signal point fields to update"),
    responses(
        (status = 200, description = "Signal point updated", body = PointCrudResult),
        (status = 400, description = "Invalid update"),
        (status = 404, description = "Channel or signal point not found")
    ),
    tag = "io"
)]
pub async fn update_signal_point_handler(
    Path((channel_id, point_id)): Path<(u32, u32)>,
    State(state): State<AppState>,
    Query(reload_query): Query<crate::dto::AutoReloadQuery>,
    Json(update): Json<PointUpdateRequest>,
) -> Result<Json<SuccessResponse<PointCrudResult>>, AppError> {
    update_point_handler_inner(channel_id, "S", point_id, state, reload_query, update).await
}

/// Update control point
#[utoipa::path(
    put,
    path = "/api/channels/{channel_id}/C/points/{point_id}",
    params(
        ("channel_id" = u32, Path, description = "Channel identifier"),
        ("point_id" = u32, Path, description = "Control point identifier"),
        ("auto_reload" = bool, Query, description = "Reconcile the channel through the governed application boundary after update (default: false)")
    ),
    request_body(content = PointUpdateRequest, description = "Control point fields to update"),
    responses(
        (status = 200, description = "Control point updated", body = PointCrudResult),
        (status = 400, description = "Invalid update"),
        (status = 404, description = "Channel or control point not found")
    ),
    tag = "io"
)]
pub async fn update_control_point_handler(
    Path((channel_id, point_id)): Path<(u32, u32)>,
    State(state): State<AppState>,
    Query(reload_query): Query<crate::dto::AutoReloadQuery>,
    Json(update): Json<PointUpdateRequest>,
) -> Result<Json<SuccessResponse<PointCrudResult>>, AppError> {
    update_point_handler_inner(channel_id, "C", point_id, state, reload_query, update).await
}

/// Update adjustment point
#[utoipa::path(
    put,
    path = "/api/channels/{channel_id}/A/points/{point_id}",
    params(
        ("channel_id" = u32, Path, description = "Channel identifier"),
        ("point_id" = u32, Path, description = "Adjustment point identifier"),
        ("auto_reload" = bool, Query, description = "Reconcile the channel through the governed application boundary after update (default: false)")
    ),
    request_body(content = PointUpdateRequest, description = "Adjustment point fields to update"),
    responses(
        (status = 200, description = "Adjustment point updated", body = PointCrudResult),
        (status = 400, description = "Invalid update"),
        (status = 404, description = "Channel or adjustment point not found")
    ),
    tag = "io"
)]
pub async fn update_adjustment_point_handler(
    Path((channel_id, point_id)): Path<(u32, u32)>,
    State(state): State<AppState>,
    Query(reload_query): Query<crate::dto::AutoReloadQuery>,
    Json(update): Json<PointUpdateRequest>,
) -> Result<Json<SuccessResponse<PointCrudResult>>, AppError> {
    update_point_handler_inner(channel_id, "A", point_id, state, reload_query, update).await
}

// --- DELETE wrappers ---

/// Delete telemetry point
#[utoipa::path(
    delete,
    path = "/api/channels/{channel_id}/T/points/{point_id}",
    params(
        ("channel_id" = u32, Path, description = "Channel identifier"),
        ("point_id" = u32, Path, description = "Telemetry point identifier"),
        ("auto_reload" = bool, Query, description = "Reconcile the channel through the governed application boundary after deletion (default: false)")
    ),
    responses(
        (status = 200, description = "Telemetry point deleted", body = PointCrudResult),
        (status = 404, description = "Channel or telemetry point not found")
    ),
    tag = "io"
)]
pub async fn delete_telemetry_point_handler(
    Path((channel_id, point_id)): Path<(u32, u32)>,
    State(state): State<AppState>,
    Query(reload_query): Query<crate::dto::AutoReloadQuery>,
) -> Result<Json<SuccessResponse<PointCrudResult>>, AppError> {
    delete_point_handler_inner(channel_id, "T", point_id, state, reload_query).await
}

/// Delete signal point
#[utoipa::path(
    delete,
    path = "/api/channels/{channel_id}/S/points/{point_id}",
    params(
        ("channel_id" = u32, Path, description = "Channel identifier"),
        ("point_id" = u32, Path, description = "Signal point identifier"),
        ("auto_reload" = bool, Query, description = "Reconcile the channel through the governed application boundary after deletion (default: false)")
    ),
    responses(
        (status = 200, description = "Signal point deleted", body = PointCrudResult),
        (status = 404, description = "Channel or signal point not found")
    ),
    tag = "io"
)]
pub async fn delete_signal_point_handler(
    Path((channel_id, point_id)): Path<(u32, u32)>,
    State(state): State<AppState>,
    Query(reload_query): Query<crate::dto::AutoReloadQuery>,
) -> Result<Json<SuccessResponse<PointCrudResult>>, AppError> {
    delete_point_handler_inner(channel_id, "S", point_id, state, reload_query).await
}

/// Delete control point
#[utoipa::path(
    delete,
    path = "/api/channels/{channel_id}/C/points/{point_id}",
    params(
        ("channel_id" = u32, Path, description = "Channel identifier"),
        ("point_id" = u32, Path, description = "Control point identifier"),
        ("auto_reload" = bool, Query, description = "Reconcile the channel through the governed application boundary after deletion (default: false)")
    ),
    responses(
        (status = 200, description = "Control point deleted", body = PointCrudResult),
        (status = 404, description = "Channel or control point not found")
    ),
    tag = "io"
)]
pub async fn delete_control_point_handler(
    Path((channel_id, point_id)): Path<(u32, u32)>,
    State(state): State<AppState>,
    Query(reload_query): Query<crate::dto::AutoReloadQuery>,
) -> Result<Json<SuccessResponse<PointCrudResult>>, AppError> {
    delete_point_handler_inner(channel_id, "C", point_id, state, reload_query).await
}

/// Delete adjustment point
#[utoipa::path(
    delete,
    path = "/api/channels/{channel_id}/A/points/{point_id}",
    params(
        ("channel_id" = u32, Path, description = "Channel identifier"),
        ("point_id" = u32, Path, description = "Adjustment point identifier"),
        ("auto_reload" = bool, Query, description = "Reconcile the channel through the governed application boundary after deletion (default: false)")
    ),
    responses(
        (status = 200, description = "Adjustment point deleted", body = PointCrudResult),
        (status = 404, description = "Channel or adjustment point not found")
    ),
    tag = "io"
)]
pub async fn delete_adjustment_point_handler(
    Path((channel_id, point_id)): Path<(u32, u32)>,
    State(state): State<AppState>,
    Query(reload_query): Query<crate::dto::AutoReloadQuery>,
) -> Result<Json<SuccessResponse<PointCrudResult>>, AppError> {
    delete_point_handler_inner(channel_id, "A", point_id, state, reload_query).await
}
