//! Command-audit read endpoint.
//!
//! Every governed channel mutation is recorded before it is reported as
//! complete, but the audit sink only ever wrote. The events existed and could
//! not be read back, so "who disabled that channel, and did it succeed" was
//! answerable only by opening this service's SQLite file by hand.
//!
//! The query vocabulary and response shape are shared with every other
//! service's audit endpoint; only the store is local.

use aether_ports::AuditQuery;
use aether_store_local::SqliteAuditQuery;
use axum::{
    extract::{Query, State},
    response::Json,
};
use common::audit_api::{AuditEventsQuery, events_payload, filter_from_query};

use crate::api::dto::{AppError, SuccessResponse};
use crate::api::routes::AppState;

/// Read recorded command-audit events, newest first.
///
/// Every governed mutation this service accepted or refused leaves a row here,
/// including the ones policy rejected. `total` counts every match rather than
/// the returned page, so a caller can tell whether more remain.
#[utoipa::path(
    get,
    path = "/api/audit/events",
    params(
        ("request_id" = Option<String>, Query, description = "Correlates one governed command"),
        ("actor_id" = Option<String>, Query, description = "Authorized identity, such as user:7"),
        ("capability" = Option<String>, Query, description = "Capability name, such as io.channel.manage"),
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
    tag = "io"
)]
pub async fn list_audit_events(
    State(state): State<AppState>,
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
