#![allow(clippy::disallowed_methods)]

//! Single-point CRUD handlers (Create, Update, Delete)

use crate::api::routes::AppState;
use crate::core::config::TelemetryPoint;
use crate::dto::{AppError, SuccessResponse};
use crate::point_topology::{
    PointDefinitionMutation, PointKind, PointMutation, PointPatchMutation, PointTopologyAcceptance,
    PointTopologyMutation, PointTopologyMutationResult,
};
use axum::{
    Extension,
    extract::{Path, Query, State},
    http::HeaderMap,
    response::Json,
};

use super::point_governance::{PointTopologyHttpBoundary, completion_audit};
use super::point_helpers::{point_type_to_table, trigger_channel_reload_if_needed};
use super::point_types::{PointCrudResult, PointUpdateRequest};

fn accepted_result(
    acceptance: PointTopologyAcceptance,
    channel_id: u32,
    point_type: String,
    point_id: u32,
    message: String,
) -> PointCrudResult {
    let request_id = acceptance.request_id().to_string();
    let resulting_revision = acceptance.resulting_revision().get();
    let audit = completion_audit(acceptance.completion_audit());
    let signal_name = match acceptance.into_result() {
        PointTopologyMutationResult::Single { signal_name } => signal_name,
        PointTopologyMutationResult::Batch { .. }
        | PointTopologyMutationResult::Provisioned { .. }
        | PointTopologyMutationResult::MappingsUpdated { .. } => {
            "point mutation completed".to_string()
        },
    };
    PointCrudResult {
        channel_id,
        point_type,
        point_id,
        signal_name,
        message,
        request_id,
        resulting_revision,
        completion_audit: audit,
        retryable: false,
    }
}

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
    Extension(boundary): Extension<PointTopologyHttpBoundary>,
    headers: HeaderMap,
    Json(payload): Json<serde_json::Value>,
) -> Result<Json<SuccessResponse<PointCrudResult>>, AppError> {
    let point: TelemetryPoint = serde_json::from_value(payload)
        .map_err(|e| AppError::bad_request(format!("Invalid request body: {}", e)))?;

    if point.base.point_id != point_id {
        return Err(AppError::bad_request(format!(
            "Point ID mismatch: path has {}, body has {}",
            point_id, point.base.point_id
        )));
    }

    let acceptance = boundary
        .mutate(
            &headers,
            PointTopologyMutation::single(
                channel_id,
                PointMutation::Create {
                    kind: PointKind::Telemetry,
                    definition: PointDefinitionMutation {
                        point_id,
                        signal_name: point.base.signal_name,
                        scale: point.scale,
                        offset: point.offset,
                        unit: point.base.unit.unwrap_or_default(),
                        reverse: point.reverse,
                        data_type: point.data_type,
                        description: point.base.description.unwrap_or_default(),
                        normal_state: 0,
                        minimum: None,
                        maximum: None,
                        step: 1.0,
                        protocol_mapping: Some(None),
                    },
                    force: false,
                },
            ),
        )
        .await?;

    tracing::debug!("Ch{}:T:{} created", channel_id, point_id);
    trigger_channel_reload_if_needed(channel_id, &state, reload_query.auto_reload).await;

    Ok(Json(SuccessResponse::new(accepted_result(
        acceptance,
        channel_id,
        "T".to_string(),
        point_id,
        "Telemetry point created successfully".to_string(),
    ))))
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
    Extension(boundary): Extension<PointTopologyHttpBoundary>,
    headers: HeaderMap,
    Json(payload): Json<serde_json::Value>,
) -> Result<Json<SuccessResponse<PointCrudResult>>, AppError> {
    let fields = extract_create_fields(&payload, point_id, "bool")?;

    let normal_state = payload
        .get("normal_state")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);

    let acceptance = boundary
        .mutate(
            &headers,
            PointTopologyMutation::single(
                channel_id,
                PointMutation::Create {
                    kind: PointKind::Signal,
                    definition: PointDefinitionMutation {
                        point_id,
                        signal_name: fields.signal_name,
                        scale: fields.scale,
                        offset: fields.offset,
                        unit: fields.unit,
                        reverse: fields.reverse,
                        data_type: fields.data_type,
                        description: fields.description,
                        normal_state,
                        minimum: None,
                        maximum: None,
                        step: 1.0,
                        protocol_mapping: Some(None),
                    },
                    force: false,
                },
            ),
        )
        .await?;

    tracing::debug!("Ch{}:S:{} created", channel_id, point_id);
    trigger_channel_reload_if_needed(channel_id, &state, reload_query.auto_reload).await;

    Ok(Json(SuccessResponse::new(accepted_result(
        acceptance,
        channel_id,
        "S".to_string(),
        point_id,
        "Signal point created successfully".to_string(),
    ))))
}

/// Internal: create control or adjustment point (identical schema)
#[allow(clippy::too_many_arguments)]
async fn create_ca_point_inner(
    channel_id: u32,
    point_type: &str,
    point_id: u32,
    state: AppState,
    reload_query: crate::dto::AutoReloadQuery,
    boundary: PointTopologyHttpBoundary,
    headers: HeaderMap,
    payload: serde_json::Value,
    default_data_type: &str,
) -> Result<Json<SuccessResponse<PointCrudResult>>, AppError> {
    point_type_to_table(point_type)?;
    let fields = extract_create_fields(&payload, point_id, default_data_type)?;

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

    let (minimum, maximum, step) = adjustment_constraints.unwrap_or((None, None, 1.0));
    let kind = PointKind::parse(point_type).map_err(AppError::bad_request)?;
    let acceptance = boundary
        .mutate(
            &headers,
            PointTopologyMutation::single(
                channel_id,
                PointMutation::Create {
                    kind,
                    definition: PointDefinitionMutation {
                        point_id,
                        signal_name: fields.signal_name,
                        scale: fields.scale,
                        offset: fields.offset,
                        unit: fields.unit,
                        reverse: fields.reverse,
                        data_type: fields.data_type,
                        description: fields.description,
                        normal_state: 0,
                        minimum,
                        maximum,
                        step,
                        protocol_mapping: Some(None),
                    },
                    force: false,
                },
            ),
        )
        .await?;

    let type_name = match point_type {
        "C" => "Control",
        "A" => "Adjustment",
        _ => point_type,
    };
    tracing::debug!("Ch{}:{}:{} created", channel_id, point_type, point_id);
    trigger_channel_reload_if_needed(channel_id, &state, reload_query.auto_reload).await;

    Ok(Json(SuccessResponse::new(accepted_result(
        acceptance,
        channel_id,
        point_type.to_string(),
        point_id,
        format!("{} point created successfully", type_name),
    ))))
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
    Extension(boundary): Extension<PointTopologyHttpBoundary>,
    headers: HeaderMap,
    Json(payload): Json<serde_json::Value>,
) -> Result<Json<SuccessResponse<PointCrudResult>>, AppError> {
    create_ca_point_inner(
        channel_id,
        "C",
        point_id,
        state,
        reload_query,
        boundary,
        headers,
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
    Extension(boundary): Extension<PointTopologyHttpBoundary>,
    headers: HeaderMap,
    Json(payload): Json<serde_json::Value>,
) -> Result<Json<SuccessResponse<PointCrudResult>>, AppError> {
    create_ca_point_inner(
        channel_id,
        "A",
        point_id,
        state,
        reload_query,
        boundary,
        headers,
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
    boundary: PointTopologyHttpBoundary,
    headers: HeaderMap,
    update: PointUpdateRequest,
) -> Result<Json<SuccessResponse<PointCrudResult>>, AppError> {
    let point_type_upper = point_type.to_ascii_uppercase();
    let kind = PointKind::parse(point_type).map_err(AppError::bad_request)?;
    let response_type = point_type_upper.clone();
    let acceptance = boundary
        .mutate(
            &headers,
            PointTopologyMutation::single(
                channel_id,
                PointMutation::Update {
                    kind,
                    point_id,
                    patch: PointPatchMutation {
                        signal_name: update.signal_name,
                        description: update.description,
                        unit: update.unit,
                        scale: update.scale,
                        offset: update.offset,
                        data_type: update.data_type,
                        reverse: update.reverse,
                        minimum: update.min_value,
                        maximum: update.max_value,
                        step: update.step,
                    },
                },
            ),
        )
        .await?;

    tracing::debug!("Ch{}:{}:{} updated", channel_id, point_type_upper, point_id);
    trigger_channel_reload_if_needed(channel_id, &state, reload_query.auto_reload).await;

    Ok(Json(SuccessResponse::new(accepted_result(
        acceptance,
        channel_id,
        response_type,
        point_id,
        "Point updated successfully".to_string(),
    ))))
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
    boundary: PointTopologyHttpBoundary,
    headers: HeaderMap,
) -> Result<Json<SuccessResponse<PointCrudResult>>, AppError> {
    let point_type_upper = point_type.to_ascii_uppercase();
    let kind = PointKind::parse(point_type).map_err(AppError::bad_request)?;
    let response_type = point_type_upper.clone();
    let acceptance = boundary
        .mutate(
            &headers,
            PointTopologyMutation::single(channel_id, PointMutation::Delete { kind, point_id }),
        )
        .await?;

    tracing::debug!("Ch{}:{}:{} deleted", channel_id, point_type_upper, point_id);

    trigger_channel_reload_if_needed(channel_id, &state, reload_query.auto_reload).await;

    Ok(Json(SuccessResponse::new(accepted_result(
        acceptance,
        channel_id,
        response_type,
        point_id,
        "Point deleted successfully".to_string(),
    ))))
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
    Extension(boundary): Extension<PointTopologyHttpBoundary>,
    headers: HeaderMap,
    Json(update): Json<PointUpdateRequest>,
) -> Result<Json<SuccessResponse<PointCrudResult>>, AppError> {
    update_point_handler_inner(
        channel_id,
        "T",
        point_id,
        state,
        reload_query,
        boundary,
        headers,
        update,
    )
    .await
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
    Extension(boundary): Extension<PointTopologyHttpBoundary>,
    headers: HeaderMap,
    Json(update): Json<PointUpdateRequest>,
) -> Result<Json<SuccessResponse<PointCrudResult>>, AppError> {
    update_point_handler_inner(
        channel_id,
        "S",
        point_id,
        state,
        reload_query,
        boundary,
        headers,
        update,
    )
    .await
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
    Extension(boundary): Extension<PointTopologyHttpBoundary>,
    headers: HeaderMap,
    Json(update): Json<PointUpdateRequest>,
) -> Result<Json<SuccessResponse<PointCrudResult>>, AppError> {
    update_point_handler_inner(
        channel_id,
        "C",
        point_id,
        state,
        reload_query,
        boundary,
        headers,
        update,
    )
    .await
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
    Extension(boundary): Extension<PointTopologyHttpBoundary>,
    headers: HeaderMap,
    Json(update): Json<PointUpdateRequest>,
) -> Result<Json<SuccessResponse<PointCrudResult>>, AppError> {
    update_point_handler_inner(
        channel_id,
        "A",
        point_id,
        state,
        reload_query,
        boundary,
        headers,
        update,
    )
    .await
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
    Extension(boundary): Extension<PointTopologyHttpBoundary>,
    headers: HeaderMap,
) -> Result<Json<SuccessResponse<PointCrudResult>>, AppError> {
    delete_point_handler_inner(
        channel_id,
        "T",
        point_id,
        state,
        reload_query,
        boundary,
        headers,
    )
    .await
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
    Extension(boundary): Extension<PointTopologyHttpBoundary>,
    headers: HeaderMap,
) -> Result<Json<SuccessResponse<PointCrudResult>>, AppError> {
    delete_point_handler_inner(
        channel_id,
        "S",
        point_id,
        state,
        reload_query,
        boundary,
        headers,
    )
    .await
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
    Extension(boundary): Extension<PointTopologyHttpBoundary>,
    headers: HeaderMap,
) -> Result<Json<SuccessResponse<PointCrudResult>>, AppError> {
    delete_point_handler_inner(
        channel_id,
        "C",
        point_id,
        state,
        reload_query,
        boundary,
        headers,
    )
    .await
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
    Extension(boundary): Extension<PointTopologyHttpBoundary>,
    headers: HeaderMap,
) -> Result<Json<SuccessResponse<PointCrudResult>>, AppError> {
    delete_point_handler_inner(
        channel_id,
        "A",
        point_id,
        state,
        reload_query,
        boundary,
        headers,
    )
    .await
}
