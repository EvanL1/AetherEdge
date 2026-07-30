#![allow(clippy::disallowed_methods)]

//! Single-point CRUD handlers (Create, Update, Delete)

use crate::api::routes::AppState;
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
use super::point_helpers::trigger_channel_reload_if_needed;
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

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct StrictPointCreateRequest {
    #[serde(default)]
    channel_id: Option<u32>,
    point_id: u32,
    signal_name: String,
    #[serde(default = "default_scale")]
    scale: f64,
    #[serde(default)]
    offset: f64,
    #[serde(default)]
    unit: Option<String>,
    #[serde(default)]
    reverse: bool,
    #[serde(default)]
    data_type: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    normal_state: Option<i64>,
    #[serde(default)]
    min_value: Option<f64>,
    #[serde(default)]
    max_value: Option<f64>,
    #[serde(default)]
    step: Option<f64>,
    #[serde(default)]
    protocol_mappings: Option<String>,
}

const fn default_scale() -> f64 {
    1.0
}

fn parse_create_definition(
    payload: serde_json::Value,
    channel_id: u32,
    point_id: u32,
    kind: PointKind,
) -> Result<PointDefinitionMutation, AppError> {
    let request: StrictPointCreateRequest = serde_json::from_value(payload)
        .map_err(|error| AppError::bad_request(format!("Invalid request body: {error}")))?;
    if request.point_id != point_id {
        return Err(AppError::bad_request(format!(
            "Point ID mismatch: path has {}, body has {}",
            point_id, request.point_id
        )));
    }
    if let Some(payload_channel_id) = request.channel_id
        && payload_channel_id != channel_id
    {
        return Err(AppError::bad_request(format!(
            "Channel ID mismatch: path has {channel_id}, body has {payload_channel_id}",
        )));
    }
    if kind != PointKind::Signal && request.normal_state.is_some() {
        return Err(AppError::bad_request(
            "normal_state is only valid for signal points",
        ));
    }
    if kind != PointKind::Adjustment
        && (request.min_value.is_some() || request.max_value.is_some() || request.step.is_some())
    {
        return Err(AppError::bad_request(
            "min_value, max_value, and step are only valid for adjustment points",
        ));
    }
    let default_data_type = match kind {
        PointKind::Telemetry => "float32",
        PointKind::Signal | PointKind::Control => "bool",
        PointKind::Adjustment => "int16",
    };

    Ok(PointDefinitionMutation {
        point_id,
        signal_name: request.signal_name,
        scale: request.scale,
        offset: request.offset,
        unit: request.unit.unwrap_or_default(),
        reverse: request.reverse,
        data_type: request
            .data_type
            .unwrap_or_else(|| default_data_type.to_string()),
        description: request.description.unwrap_or_default(),
        normal_state: request.normal_state.unwrap_or_default(),
        minimum: request.min_value,
        maximum: request.max_value,
        step: request.step.unwrap_or(1.0),
        protocol_mapping: Some(request.protocol_mappings),
    })
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct StrictPointUpdateRequest {
    signal_name: Option<String>,
    description: Option<String>,
    unit: Option<String>,
    scale: Option<f64>,
    offset: Option<f64>,
    data_type: Option<String>,
    reverse: Option<bool>,
    min_value: Option<f64>,
    max_value: Option<f64>,
    step: Option<f64>,
}

fn parse_update_request(payload: serde_json::Value) -> Result<PointUpdateRequest, AppError> {
    let request: StrictPointUpdateRequest = serde_json::from_value(payload)
        .map_err(|error| AppError::bad_request(format!("Invalid request body: {error}")))?;
    Ok(PointUpdateRequest {
        signal_name: request.signal_name,
        description: request.description,
        unit: request.unit,
        scale: request.scale,
        offset: request.offset,
        data_type: request.data_type,
        reverse: request.reverse,
        min_value: request.min_value,
        max_value: request.max_value,
        step: request.step,
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
    let definition = parse_create_definition(payload, channel_id, point_id, PointKind::Telemetry)?;

    let acceptance = boundary
        .mutate(
            &headers,
            PointTopologyMutation::single(
                channel_id,
                PointMutation::Create {
                    kind: PointKind::Telemetry,
                    definition,
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
    let definition = parse_create_definition(payload, channel_id, point_id, PointKind::Signal)?;

    let acceptance = boundary
        .mutate(
            &headers,
            PointTopologyMutation::single(
                channel_id,
                PointMutation::Create {
                    kind: PointKind::Signal,
                    definition,
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
) -> Result<Json<SuccessResponse<PointCrudResult>>, AppError> {
    let kind = PointKind::parse(point_type).map_err(AppError::bad_request)?;
    let definition = parse_create_definition(payload, channel_id, point_id, kind)?;
    let acceptance = boundary
        .mutate(
            &headers,
            PointTopologyMutation::single(
                channel_id,
                PointMutation::Create {
                    kind,
                    definition,
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
        point_type_upper,
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
        point_type_upper,
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
    Json(payload): Json<serde_json::Value>,
) -> Result<Json<SuccessResponse<PointCrudResult>>, AppError> {
    let update = parse_update_request(payload)?;
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
    Json(payload): Json<serde_json::Value>,
) -> Result<Json<SuccessResponse<PointCrudResult>>, AppError> {
    let update = parse_update_request(payload)?;
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
    Json(payload): Json<serde_json::Value>,
) -> Result<Json<SuccessResponse<PointCrudResult>>, AppError> {
    let update = parse_update_request(payload)?;
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
    Json(payload): Json<serde_json::Value>,
) -> Result<Json<SuccessResponse<PointCrudResult>>, AppError> {
    let update = parse_update_request(payload)?;
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
