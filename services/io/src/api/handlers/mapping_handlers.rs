//! Protocol mapping handlers
//!
//! Read-only protocol mapping projection for a commissioned channel.

#![allow(clippy::disallowed_methods)] // json! macro used in multiple functions

use crate::api::routes::AppState;
use crate::dto::{AppError, SuccessResponse};
use axum::{
    extract::{Path, State},
    response::Json,
};
use serde_json::json;

// ============================================================================
// Helpers
// ============================================================================

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
