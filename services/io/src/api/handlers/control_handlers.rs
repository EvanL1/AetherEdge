//! Governed channel lifecycle and per-channel logging handlers.

#![allow(clippy::disallowed_methods)] // json! macro used in multiple functions

use super::channel_management_handlers::{
    ChannelManagementHttpBoundary, path_channel_id, required_request_id,
};
use crate::api::routes::AppState;
use crate::dto::{
    AppError, ChannelCompletionAudit, ChannelCompletionAuditState, ChannelControlOperationResult,
    ChannelControlResponse, ChannelControlResult, ChannelOperation, ChannelOperationKind,
    ChannelRuntimeProjectionResult, SuccessResponse,
};
use aether_application::{
    ChannelMutationAcceptance, ChannelReconciliationAcceptance, CompletionAuditStatus,
};
use aether_domain::ChannelId;
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
