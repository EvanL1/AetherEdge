//! Governed channel runtime reconciliation and routing reload handlers.

use super::{ChannelManagementHttpBoundary, path_channel_id};
use crate::api::routes::AppState;
use crate::dto::{
    AppError, ChannelCompletionAudit, ChannelCompletionAuditState, ChannelDesiredStateResult,
    ChannelReconciliationItemResult, ChannelReconciliationResponse, ChannelReconciliationResult,
    ChannelReconciliationScopeResult, ChannelRuntimeProjectionResult, SuccessResponse,
};

use aether_application::{ChannelReconciliationAcceptance, CompletionAuditStatus};
use aether_ports::{
    ChannelDesiredStateObservation, ChannelReconciliationScope, ChannelRuntimeProjection,
};
use axum::{
    Extension,
    extract::{Path, State},
    http::HeaderMap,
    response::Json,
};

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

/// Compatibility alias for full channel reconciliation.
///
/// New clients must use `POST /api/channels/reconcile`. This deprecated alias
/// executes the same governed application command and has the same receipt.
#[utoipa::path(
    post,
    path = "/api/channels/reload",
    params(
        ("x-request-id" = String, Header, format = "uuid", description = "Required UUID audit correlation ID; this is not an idempotency key"),
        ("x-aether-confirmed" = bool, Header, description = "Required explicit confirmation; must be true")
    ),
    responses(
        (status = 200, description = "Accepted non-idempotent full runtime reconciliation through the compatibility alias. Per-channel degradation and incomplete terminal audit remain accepted; do not retry automatically.", body = ChannelReconciliationResponse),
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
pub async fn reload_configuration_handler(
    Extension(boundary): Extension<ChannelManagementHttpBoundary>,
    headers: HeaderMap,
) -> Result<Json<ChannelReconciliationResponse>, AppError> {
    reconcile_scope(&boundary, &headers, ChannelReconciliationScope::All).await
}

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
        metadata: std::collections::HashMap::new(),
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

/// Reload the routing cache only (does not touch channels).
///
/// Unlike `/reload`, this only refreshes the C2M / M2C / C2C routing tables without
/// touching the channel protocol layer. Use this when routing changes without point
/// changes — it is lighter and faster than `/reload` and does not interrupt device
/// connections. The routing table is replaced atomically via ArcSwap. Note: automation
/// maintains its own independent routing cache and will sync on its next periodic reload.
#[utoipa::path(
    post,
    path = "/api/routing/reload",
    responses(
        (status = 200, description = "Routing cache reloaded successfully", body = crate::dto::RoutingReloadResult),
        (status = 500, description = "Internal server error")
    ),
    tag = "io"
)]
pub async fn reload_routing_handler(
    State(state): State<AppState>,
) -> Result<Json<SuccessResponse<crate::dto::RoutingReloadResult>>, AppError> {
    tracing::debug!("Reloading routing");

    let start_time = std::time::Instant::now();
    let mut errors = Vec::new();

    let (c2m_count, m2c_count, c2c_count) =
        match aether_routing::load_routing_maps(&state.sqlite_pool).await {
            Ok(maps) => {
                let counts = (maps.c2m.len(), maps.m2c.len(), maps.c2c.len());
                state
                    .channel_manager
                    .routing_cache
                    .update(maps.c2m, maps.m2c, maps.c2c);
                // SHM layout is based on channel points, not routing.
                // No SHM rebuild is needed for routing changes.
                counts
            },
            Err(error) => {
                tracing::error!(error = %error, "failed to reload routing cache");
                errors.push("Failed to reload routing cache".to_string());
                (0, 0, 0)
            },
        };

    let duration_ms = start_time.elapsed().as_millis() as u64;

    let result = crate::dto::RoutingReloadResult {
        c2m_count,
        m2c_count,
        c2c_count,
        errors,
        duration_ms,
    };

    if result.errors.is_empty() {
        tracing::info!(
            "Routing: {} C2M, {} M2C, {} C2C ({}ms)",
            c2m_count,
            m2c_count,
            c2c_count,
            duration_ms
        );
    } else {
        tracing::warn!(
            "Routing: {} errors ({}ms)",
            result.errors.len(),
            duration_ms
        );
    }

    Ok(Json(SuccessResponse::new(result)))
}
