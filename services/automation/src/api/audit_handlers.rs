//! Command-audit read endpoint.
//!
//! This service records every device command and every governed routing or
//! instance mutation, including the ones policy refused. Until now the sink
//! only wrote, so a rule that turned a switch off left a complete trail that
//! nothing could read: accountability existed in the database and nowhere in
//! the product.
//!
//! The query vocabulary and response shape are shared with every other
//! service's audit endpoint; only the store is local.

#![allow(clippy::disallowed_methods)] // json! macro used via the shared payload builder

use std::sync::Arc;

use aether_ports::AuditQuery;
use aether_store_local::SqliteAuditQuery;
use axum::{
    extract::{Query, State},
    response::Json,
};
use common::audit_api::{AuditEventsQuery, events_payload, filter_from_query};
use common::{AppError, SuccessResponse};

use crate::app_state::AppState;

/// Read recorded command-audit events, newest first.
///
/// `total` counts every match rather than the returned page, so a caller can
/// tell whether more remain.
#[cfg_attr(feature = "openapi", utoipa::path(
    get,
    path = "/api/audit/events",
    params(
        ("request_id" = Option<String>, Query, description = "Correlates one governed command"),
        ("actor_id" = Option<String>, Query, description = "Authorized identity, such as user:7"),
        ("capability" = Option<String>, Query, description = "Capability name, such as device.control"),
        ("outcome" = Option<String>, Query, description = "rejected | attempted | succeeded | failed"),
        ("since_ms" = Option<u64>, Query, description = "Inclusive lower bound, Unix milliseconds"),
        ("until_ms" = Option<u64>, Query, description = "Inclusive upper bound, Unix milliseconds"),
        ("limit" = Option<u32>, Query, description = "Page size, clamped to 1000"),
        ("offset" = Option<u32>, Query, description = "Page offset")
    ),
    responses(
        (status = 200, description = "Matching audit events, newest first", body = serde_json::Value),
        (status = 400, description = "Unrecognised outcome filter"),
        (status = 500, description = "Audit store unavailable")
    ),
    tag = "automation"
))]
pub async fn list_audit_events(
    State(state): State<Arc<AppState>>,
    Query(query): Query<AuditEventsQuery>,
) -> Result<Json<SuccessResponse<serde_json::Value>>, AppError> {
    let filter = filter_from_query(&query)
        .map_err(|rejection| AppError::bad_request(rejection.message()))?;
    let reader = SqliteAuditQuery::new(state.sqlite_pool.clone());
    let (records, total) = reader.query(&filter).await.map_err(|error| {
        tracing::error!("audit read failed: {}", error);
        AppError::internal_error(format!("command audit unavailable: {error}"))
    })?;

    Ok(Json(SuccessResponse::new(events_payload(
        records, total, &filter,
    ))))
}
