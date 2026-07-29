//! Governed channel runtime reconciliation handlers.

use super::{ChannelManagementHttpBoundary, path_channel_id};
use crate::dto::{
    AppError, ChannelCompletionAudit, ChannelCompletionAuditState, ChannelDesiredStateResult,
    ChannelReconciliationItemResult, ChannelReconciliationResponse, ChannelReconciliationResult,
    ChannelReconciliationScopeResult, ChannelRuntimeProjectionResult,
};

use aether_application::{ChannelReconciliationAcceptance, CompletionAuditStatus};
use aether_ports::{
    ChannelDesiredStateObservation, ChannelReconciliationScope, ChannelRuntimeProjection,
};
use axum::{Extension, extract::Path, http::HeaderMap, response::Json};

/// Reconcile every commissioned channel runtime from authoritative desired
/// state through the shared application command.
#[utoipa::path(
    post,
    path = "/api/channels/reconcile",
    params(
        ("x-request-id" = String, Header, format = "uuid", description = "Required UUID audit correlation ID; this is not an idempotency key"),
        ("x-aether-confirmed" = bool, Header, description = "Required explicit confirmation; must be true")
    ),
    responses(
        (status = 200, description = "Accepted non-idempotent full runtime reconciliation. Per-channel degradation and incomplete terminal audit remain accepted; do not retry automatically.", body = ChannelReconciliationResponse),
        (status = 400, description = "Malformed request ID or invalid reconciliation scope", body = common::ErrorResponse),
        (status = 403, description = "Missing/invalid Bearer token or io.channel.manage permission", body = common::ErrorResponse),
        (status = 409, description = "Runtime reconciliation conflicts with current state", body = common::ErrorResponse),
        (status = 422, description = "Explicit confirmation is missing or false", body = common::ErrorResponse),
        (status = 503, description = "Mandatory pre-execution audit or reconciliation adapter is unavailable", body = common::ErrorResponse),
        (status = 504, description = "Reconciliation adapter timed out", body = common::ErrorResponse),
        (status = 500, description = "Permanent reconciliation adapter failure", body = common::ErrorResponse)
    ),
    security(("bearer_auth" = [])),
    tag = "io"
)]
pub async fn reconcile_channels_handler(
    Extension(boundary): Extension<ChannelManagementHttpBoundary>,
    headers: HeaderMap,
) -> Result<Json<ChannelReconciliationResponse>, AppError> {
    reconcile_scope(&boundary, &headers, ChannelReconciliationScope::All).await
}

/// Reconcile one commissioned channel runtime from authoritative desired
/// state through the shared application command.
#[utoipa::path(
    post,
    path = "/api/channels/{id}/reconcile",
    params(
        ("id" = u32, Path, description = "Stable channel identifier below 10000", maximum = 9999),
        ("x-request-id" = String, Header, format = "uuid", description = "Required UUID audit correlation ID; this is not an idempotency key"),
        ("x-aether-confirmed" = bool, Header, description = "Required explicit confirmation; must be true")
    ),
    responses(
        (status = 200, description = "Accepted non-idempotent single-channel runtime reconciliation. An absent desired channel is fenced and reported as an accepted removed projection; a degraded projection or incomplete terminal audit also remains accepted; do not retry automatically.", body = ChannelReconciliationResponse),
        (status = 400, description = "Malformed channel ID or request ID", body = common::ErrorResponse),
        (status = 403, description = "Missing/invalid Bearer token or io.channel.manage permission", body = common::ErrorResponse),
        (status = 409, description = "Runtime reconciliation conflicts with current state", body = common::ErrorResponse),
        (status = 422, description = "Explicit confirmation is missing or false", body = common::ErrorResponse),
        (status = 503, description = "Mandatory pre-execution audit or reconciliation adapter is unavailable", body = common::ErrorResponse),
        (status = 504, description = "Reconciliation adapter timed out", body = common::ErrorResponse),
        (status = 500, description = "Permanent reconciliation adapter failure", body = common::ErrorResponse)
    ),
    security(("bearer_auth" = [])),
    tag = "io"
)]
pub async fn reconcile_channel_handler(
    Path(id): Path<String>,
    Extension(boundary): Extension<ChannelManagementHttpBoundary>,
    headers: HeaderMap,
) -> Result<Json<ChannelReconciliationResponse>, AppError> {
    let channel_id = aether_domain::ChannelId::new(path_channel_id(&id)?);
    reconcile_scope(
        &boundary,
        &headers,
        ChannelReconciliationScope::One(channel_id),
    )
    .await
}

/// Execute canonical all-channel or single-channel reconciliation through the
/// same governed application boundary.
async fn reconcile_scope(
    boundary: &ChannelManagementHttpBoundary,
    headers: &HeaderMap,
    scope: ChannelReconciliationScope,
) -> Result<Json<ChannelReconciliationResponse>, AppError> {
    let acceptance = boundary.reconcile(headers, scope).await?;
    Ok(Json(reconciliation_response(&acceptance)))
}

fn reconciliation_response(
    acceptance: &ChannelReconciliationAcceptance,
) -> ChannelReconciliationResponse {
    let (scope, channel_id) = match acceptance.scope() {
        ChannelReconciliationScope::All => (ChannelReconciliationScopeResult::All, None),
        ChannelReconciliationScope::One(channel_id) => (
            ChannelReconciliationScopeResult::One,
            Some(channel_id.get()),
        ),
    };
    let items = acceptance
        .items()
        .iter()
        .map(|item| {
            let desired = match item.desired() {
                ChannelDesiredStateObservation::Present { revision, enabled } => {
                    ChannelDesiredStateResult::Present {
                        revision: revision.get(),
                        enabled,
                    }
                },
                ChannelDesiredStateObservation::Absent { last_revision } => {
                    ChannelDesiredStateResult::Absent {
                        last_revision: last_revision.map(aether_ports::ChannelRevision::get),
                    }
                },
            };
            ChannelReconciliationItemResult {
                channel_id: item.channel_id().get(),
                desired,
                runtime_projection: runtime_projection(item.runtime_projection()),
                reconciliation_required: item.reconciliation_required(),
            }
        })
        .collect();
    let completion_audit = match acceptance.completion_audit() {
        CompletionAuditStatus::Recorded => ChannelCompletionAudit {
            status: ChannelCompletionAuditState::Recorded,
            retryable: false,
            message: None,
        },
        CompletionAuditStatus::Incomplete { failure } => {
            tracing::error!(
                request_id = acceptance.request_id(),
                error = %failure,
                "channel reconciliation was accepted but terminal audit is incomplete; do not retry"
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
    let scope_name = match scope {
        ChannelReconciliationScopeResult::All => "all channels",
        ChannelReconciliationScopeResult::One => "one channel",
    };

    ChannelReconciliationResponse {
        success: true,
        data: ChannelReconciliationResult {
            request_id: acceptance.request_id().to_string(),
            scope,
            channel_id,
            items,
            degraded_count: acceptance.degraded_count(),
            reconciliation_required: acceptance.reconciliation_required(),
            completion_audit,
            retryable: acceptance.is_retryable(),
            message: format!(
                "runtime reconciliation for {scope_name} accepted; automatic retry is forbidden"
            ),
        },
    }
}

const fn runtime_projection(
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
