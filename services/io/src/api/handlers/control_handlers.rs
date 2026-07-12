//! Governed channel lifecycle and development-only point simulation handlers.
//!
//! This module contains handlers for:
//! - Channel control operations (start, stop, restart)
//! - Point-level control commands
//! - Point-level adjustment commands
//! - Batch control and adjustment operations

#![allow(clippy::disallowed_methods)] // json! macro used in multiple functions

use super::channel_management_handlers::{
    ChannelManagementHttpBoundary, path_channel_id, required_request_id,
};
use crate::api::routes::AppState;
use crate::dto::{
    AppError, ChannelCompletionAudit, ChannelCompletionAuditState, ChannelControlOperationResult,
    ChannelControlResponse, ChannelControlResult, ChannelOperation, ChannelOperationKind,
    ChannelRuntimeProjectionResult, SuccessResponse, WritePointRequest, WriteResponse,
};
use aether_application::{
    ChannelMutationAcceptance, ChannelReconciliationAcceptance, CompletionAuditStatus,
};
use aether_domain::ChannelId;
use aether_model::PointType;
use aether_ports::{ChannelMutation, ChannelReconciliationScope, ChannelRuntimeProjection};
use axum::{
    Extension,
    extract::{Path, State, rejection::JsonRejection},
    http::HeaderMap,
    response::Json,
};

/// Govern one channel's desired lifecycle or rebuildable runtime projection.
///
/// `start` and `stop` mutate authoritative desired enabled state. `restart`
/// reconciles the runtime from that desired state; it never writes SHM or
/// calls a protocol entry directly from the HTTP boundary.
#[utoipa::path(
    post,
    path = "/api/channels/{id}/control",
    params(
        ("id" = u32, Path, description = "Stable channel identifier below 10000", maximum = 9999),
        ("x-request-id" = String, Header, format = "uuid", description = "Required UUID audit correlation ID; this is not an idempotency key"),
        ("x-aether-confirmed" = bool, Header, description = "Required explicit confirmation; must be true")
    ),
    request_body = crate::dto::ChannelOperation,
    responses(
        (status = 200, description = "Accepted non-idempotent desired-state or runtime lifecycle operation. Degraded projection and incomplete terminal audit remain accepted; do not retry automatically.", body = ChannelControlResponse),
        (status = 400, description = "Malformed channel ID, request ID, JSON, or unsupported operation", body = common::ErrorResponse),
        (status = 403, description = "Missing/invalid Bearer token or io.channel.manage permission", body = common::ErrorResponse),
        (status = 404, description = "Channel not found", body = common::ErrorResponse),
        (status = 409, description = "Desired state or runtime reconciliation conflicts with current state", body = common::ErrorResponse),
        (status = 422, description = "Explicit confirmation is missing or false", body = common::ErrorResponse),
        (status = 503, description = "Mandatory pre-execution audit or channel adapter is unavailable", body = common::ErrorResponse),
        (status = 504, description = "Channel adapter timed out", body = common::ErrorResponse),
        (status = 500, description = "Permanent channel adapter failure", body = common::ErrorResponse)
    ),
    security(("bearer_auth" = [])),
    tag = "io"
)]
pub async fn control_channel(
    Path(id): Path<String>,
    Extension(boundary): Extension<ChannelManagementHttpBoundary>,
    headers: HeaderMap,
    payload: Result<Json<ChannelOperation>, JsonRejection>,
) -> Result<Json<ChannelControlResponse>, AppError> {
    let channel_id = ChannelId::new(path_channel_id(&id)?);
    required_request_id(&headers)?;
    let Json(operation) = payload
        .map_err(|_| AppError::bad_request("Request body must be valid application/json"))?;

    match operation.operation {
        ChannelOperationKind::Start => boundary
            .mutate(&headers, ChannelMutation::enable(channel_id))
            .await
            .map(|acceptance| {
                Json(control_response_from_mutation(
                    &acceptance,
                    ChannelControlOperationResult::Start,
                ))
            }),
        ChannelOperationKind::Stop => boundary
            .mutate(&headers, ChannelMutation::disable(channel_id))
            .await
            .map(|acceptance| {
                Json(control_response_from_mutation(
                    &acceptance,
                    ChannelControlOperationResult::Stop,
                ))
            }),
        ChannelOperationKind::Restart => {
            let acceptance = boundary
                .reconcile(&headers, ChannelReconciliationScope::One(channel_id))
                .await?;
            Ok(Json(control_response_from_reconciliation(
                &acceptance,
                channel_id,
            )))
        },
    }
}

fn control_response_from_mutation(
    acceptance: &ChannelMutationAcceptance,
    operation: ChannelControlOperationResult,
) -> ChannelControlResponse {
    control_response(
        acceptance.channel_id(),
        acceptance.request_id(),
        operation,
        Some(acceptance.resulting_revision().get()),
        Some(acceptance.desired_enabled()),
        acceptance.runtime_projection(),
        acceptance.reconciliation_required(),
        acceptance.completion_audit(),
        acceptance.is_retryable(),
    )
}

fn control_response_from_reconciliation(
    acceptance: &ChannelReconciliationAcceptance,
    channel_id: ChannelId,
) -> ChannelControlResponse {
    let item = acceptance
        .items()
        .iter()
        .find(|item| item.channel_id() == channel_id);
    let (desired_revision, desired_enabled, runtime_projection, reconciliation_required) = item
        .map_or(
            (None, None, ChannelRuntimeProjection::Degraded, true),
            |item| {
                (
                    item.desired_revision()
                        .map(aether_ports::ChannelRevision::get),
                    item.desired_enabled(),
                    item.runtime_projection(),
                    item.reconciliation_required(),
                )
            },
        );
    if item.is_none() {
        tracing::error!(
            request_id = acceptance.request_id(),
            channel_id = channel_id.get(),
            "single-channel reconciliation returned no matching receipt item"
        );
    }

    control_response(
        channel_id,
        acceptance.request_id(),
        ChannelControlOperationResult::Restart,
        desired_revision,
        desired_enabled,
        runtime_projection,
        reconciliation_required,
        acceptance.completion_audit(),
        acceptance.is_retryable(),
    )
}

#[allow(clippy::too_many_arguments)]
fn control_response(
    channel_id: ChannelId,
    request_id: &str,
    operation: ChannelControlOperationResult,
    desired_revision: Option<u64>,
    desired_enabled: Option<bool>,
    runtime_projection: ChannelRuntimeProjection,
    reconciliation_required: bool,
    completion_audit: &CompletionAuditStatus,
    retryable: bool,
) -> ChannelControlResponse {
    let completion_audit = match completion_audit {
        CompletionAuditStatus::Recorded => ChannelCompletionAudit {
            status: ChannelCompletionAuditState::Recorded,
            retryable: false,
            message: None,
        },
        CompletionAuditStatus::Incomplete { failure } => {
            tracing::error!(
                request_id,
                channel_id = channel_id.get(),
                error = %failure,
                "channel lifecycle operation was accepted but terminal audit is incomplete; do not retry"
            );
            ChannelCompletionAudit {
                status: ChannelCompletionAuditState::Incomplete,
                retryable: false,
                message: Some(
                    "operation was accepted but its terminal audit is incomplete; do not retry"
                        .to_string(),
                ),
            }
        },
    };
    let operation_name = match operation {
        ChannelControlOperationResult::Start => "start",
        ChannelControlOperationResult::Stop => "stop",
        ChannelControlOperationResult::Restart => "restart",
    };

    ChannelControlResponse {
        success: true,
        data: ChannelControlResult {
            channel_id: channel_id.get(),
            request_id: request_id.to_string(),
            operation,
            desired_revision,
            desired_enabled,
            runtime_projection: runtime_projection_result(runtime_projection),
            reconciliation_required,
            completion_audit,
            retryable,
            message: format!(
                "channel {} {operation_name} accepted; automatic retry is forbidden",
                channel_id.get()
            ),
        },
        metadata: std::collections::HashMap::new(),
    }
}

const fn runtime_projection_result(
    projection: ChannelRuntimeProjection,
) -> ChannelRuntimeProjectionResult {
    match projection {
        ChannelRuntimeProjection::Stopped => ChannelRuntimeProjectionResult::Stopped,
        ChannelRuntimeProjection::ActivationPending => {
            ChannelRuntimeProjectionResult::ActivationPending
        },
        ChannelRuntimeProjection::Active => ChannelRuntimeProjectionResult::Active,
        ChannelRuntimeProjection::Degraded => ChannelRuntimeProjectionResult::Degraded,
        ChannelRuntimeProjection::Removed => ChannelRuntimeProjectionResult::Removed,
    }
}

/// Simulation write endpoint for acquisition-owned T/S points.
///
/// Real C/A device commands are deliberately rejected here. They must enter
/// through automation's authenticated, confirmed, and audited application API,
/// then reach io through the SHM/UDS command plane.
///
/// ## Supported Point Types
/// - **T** / **Telemetry**: For testing/simulation (normally read-only)
/// - **S** / **Signal**: For testing/simulation (normally read-only)
#[utoipa::path(
    post,
    path = "/api/channels/{channel_id}/write",
    description = "Development-only injection of acquisition-owned T/S samples. The route returns 403 unless AETHER_ALLOW_SIMULATION_WRITES=true; C/A device commands are always rejected and must use the audited automation application API.",
    params(
        ("channel_id" = u16, Path, description = "Channel identifier", example = 1001)
    ),
    request_body = WritePointRequest,
    responses(
        (status = 200, description = "Simulation sample committed (single or batch)",
            body = WriteResponse),
        (status = 400, description = "Invalid T/S point type, identifier, value, or batch", body = String),
        (status = 403, description = "Simulation writes are disabled by default", body = String),
        (status = 500, description = "Write operation failed", body = String)
    ),
    tag = "io"
)]
pub async fn write_channel_point(
    State(state): State<AppState>,
    Path(channel_id): Path<u32>,
    Json(request): Json<WritePointRequest>,
) -> Result<Json<SuccessResponse<crate::dto::WriteResponse>>, AppError> {
    use crate::dto::{BatchCommandError, BatchCommandResult, WritePointData, WriteResponse};

    let point_type = normalize_point_type(&request.r#type)?;
    if !point_type.is_measurement() {
        return Err(AppError::bad_request(
            "Direct C/A writes are disabled; use an instance action through aether-automation",
        ));
    }
    if !state.allow_simulation_writes {
        return Err(AppError::new(
            axum::http::StatusCode::FORBIDDEN,
            common::ErrorInfo::new(
                "Simulation writes are disabled; set AETHER_ALLOW_SIMULATION_WRITES=true only in an isolated development environment",
            )
            .with_code(403),
        ));
    }

    match &request.data {
        WritePointData::Single { id, value } => {
            let point_id = id
                .parse::<u32>()
                .map_err(|_| AppError::bad_request(format!("Invalid point ID: {}", id)))?;

            let timestamp_ms = crate::core::channels::channel_manager::unix_timestamp_ms();
            use crate::protocols::core::data::{DataBatch, DataPoint};
            let point = match point_type {
                PointType::Telemetry => DataPoint::telemetry(point_id, *value),
                PointType::Signal => DataPoint::signal(point_id, *value),
                _ => unreachable!(),
            };
            state
                .channel_manager
                .data_store()
                .write_batch(channel_id, DataBatch::from_points(vec![point]))
                .await
                .map_err(|error| {
                    AppError::internal_error(format!("Failed to write SHM point: {error}"))
                })?;

            tracing::debug!(
                "Write Ch{}:{:?}:{} = {} @{}",
                channel_id,
                point_type,
                id,
                value,
                timestamp_ms
            );

            let response = crate::dto::WritePointResponse {
                channel_id,
                point_type: point_type.as_str().to_string(),
                point_id,
                value: *value,
                timestamp_ms,
            };

            Ok(Json(SuccessResponse::new(WriteResponse::Single(response))))
        },
        WritePointData::Batch { points } => {
            let mut errors = Vec::new();
            let total = points.len();
            // Parse all IDs up front; invalid IDs go to errors and skip.
            let mut parsed: Vec<(u32, f64)> = Vec::with_capacity(total);
            for point in points {
                match point.id.parse::<u32>() {
                    Ok(id) => parsed.push((id, point.value)),
                    Err(_) => {
                        tracing::warn!("Invalid ID: Ch{}:{}:{}", channel_id, point_type, point.id);
                        errors.push(BatchCommandError {
                            point_id: 0,
                            error: format!("Invalid point ID: {}", point.id),
                        });
                    },
                }
            }

            if !parsed.is_empty() {
                use crate::protocols::core::data::{DataBatch, DataPoint};
                let points = parsed
                    .iter()
                    .map(|(point_id, value)| match point_type {
                        PointType::Telemetry => DataPoint::telemetry(*point_id, *value),
                        PointType::Signal => DataPoint::signal(*point_id, *value),
                        _ => unreachable!(),
                    })
                    .collect();
                state
                    .channel_manager
                    .data_store()
                    .write_batch(channel_id, DataBatch::from_points(points))
                    .await
                    .map_err(|error| {
                        AppError::internal_error(format!("Failed to write SHM batch: {error}"))
                    })?;
            }
            let succeeded = parsed.len();

            tracing::debug!(
                "Batch Ch{}:{:?}: {}/{} ok",
                channel_id,
                point_type,
                succeeded,
                total
            );

            let result = BatchCommandResult {
                total,
                succeeded,
                failed: total - succeeded,
                errors,
            };

            Ok(Json(SuccessResponse::new(WriteResponse::Batch(result))))
        },
    }
}

/// Change a channel's log verbosity at runtime, no restart needed.
///
/// Per-channel knob (overrides global `RUST_LOG`) for trace-level
/// debugging without flooding everyone else's logs. Accepted levels:
/// `debug` / `verbose` (full protocol frames), `info` / `standard`
/// (default), `error` (only failures). Applies both to the protocol
/// adapter's internal logging config and the per-channel log file
/// handler. Effect persists for the channel's lifetime — restart the
/// channel and it goes back to the configured default.
#[utoipa::path(
    put,
    path = "/api/channels/{id}/logging",
    params(
        ("id" = u32, Path, description = "Channel identifier")
    ),
    request_body = common::admin_api::SetLogLevelRequest,
    responses(
        (status = 200, description = "Channel log level updated", body = String,
            example = json!({
                "success": true,
                "data": "Channel 1 log level set to debug"
            })
        ),
        (status = 400, description = "Invalid log level"),
        (status = 404, description = "Channel not found")
    ),
    tag = "io"
)]
pub async fn set_channel_log_level(
    State(state): State<AppState>,
    Path(id): Path<u32>,
    Json(req): Json<common::admin_api::SetLogLevelRequest>,
) -> Result<Json<SuccessResponse<String>>, AppError> {
    let manager = &state.channel_manager;

    let Some(entry) = manager.get_channel(id) else {
        return Err(AppError::not_found(format!("Channel {} not found", id)));
    };

    entry
        .set_log_level(&req.level)
        .await
        .map_err(|e| AppError::bad_request(e.to_string()))?;

    Ok(Json(SuccessResponse::new(format!(
        "Channel {} log level set to {}",
        id, req.level
    ))))
}

/// Normalize point type from full name or short name to single letter
fn normalize_point_type(type_str: &str) -> Result<PointType, AppError> {
    match type_str {
        "T" | "t" | "Telemetry" | "telemetry" | "TELEMETRY" => Ok(PointType::Telemetry),
        "S" | "s" | "Signal" | "signal" | "SIGNAL" => Ok(PointType::Signal),
        "C" | "c" | "Control" | "control" | "CONTROL" => Ok(PointType::Control),
        "A" | "a" | "Adjustment" | "adjustment" | "ADJUSTMENT" => Ok(PointType::Adjustment),
        _ => Err(AppError::bad_request(format!(
            "Invalid point type '{}'. Must be one of: T/Telemetry, S/Signal, C/Control, A/Adjustment",
            type_str
        ))),
    }
}
