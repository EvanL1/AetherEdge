#![allow(clippy::disallowed_methods)]

//! Validation, reload, and query utility functions for point handlers

use crate::api::routes::AppState;
use crate::dto::AppError;

// ----------------------------------------------------------------------------
// Point Type Resolution
// ----------------------------------------------------------------------------

/// Resolve point type letter (T/S/C/A) to database table name
pub(super) fn point_type_to_table(point_type: &str) -> Result<&'static str, AppError> {
    match point_type {
        "T" | "t" => Ok("telemetry_points"),
        "S" | "s" => Ok("signal_points"),
        "C" | "c" => Ok("control_points"),
        "A" | "a" => Ok("adjustment_points"),
        _ => Err(AppError::bad_request(format!(
            "Invalid point type '{}'. Must be T, S, C, or A",
            point_type
        ))),
    }
}

// ----------------------------------------------------------------------------
// Protocol Mapping JSON Parsing
// ----------------------------------------------------------------------------

/// Parse protocol_mappings JSON string, returning None for null/empty/invalid
pub(super) fn parse_protocol_mapping_json(json_str: Option<&str>) -> Option<serde_json::Value> {
    let s = json_str?.trim();
    if s.is_empty() {
        return None;
    }
    match serde_json::from_str::<serde_json::Value>(s) {
        Ok(value) if !value.is_null() => Some(value),
        Ok(_) => None,
        Err(e) => {
            tracing::warn!("Parse protocol_mappings: {}", e);
            None
        },
    }
}

// ----------------------------------------------------------------------------
// Point Query Helpers
// ----------------------------------------------------------------------------

/// Fetch PointDefinition rows from a point table with optional unmapped filter
pub(super) async fn fetch_point_definitions(
    pool: &sqlx::SqlitePool,
    table: &str,
    channel_id: u32,
    unmapped_only: bool,
) -> Result<Vec<crate::dto::PointDefinition>, AppError> {
    let unmapped_clause = if unmapped_only {
        " AND (protocol_mappings IS NULL \
              OR protocol_mappings = '' \
              OR protocol_mappings = '{}' \
              OR protocol_mappings = 'null')"
    } else {
        ""
    };

    let query = format!(
        "SELECT point_id, signal_name, scale, offset, unit, data_type, reverse, \
         description, protocol_mappings \
         FROM {} WHERE channel_id = ?{} ORDER BY point_id",
        table, unmapped_clause
    );

    #[allow(clippy::type_complexity)]
    let rows: Vec<(
        u32,
        String,
        f64,
        f64,
        String,
        String,
        bool,
        String,
        Option<String>,
    )> = sqlx::query_as(&query)
        .bind(channel_id as i64)
        .fetch_all(pool)
        .await
        .map_err(|e| {
            tracing::error!("Fetch {} points: {}", table, e);
            AppError::internal_error("Database operation failed")
        })?;

    Ok(rows
        .into_iter()
        .map(
            |(
                point_id,
                signal_name,
                scale,
                offset,
                unit,
                data_type,
                reverse,
                description,
                pm_json,
            )| {
                let protocol_mapping = if unmapped_only {
                    None
                } else {
                    parse_protocol_mapping_json(pm_json.as_deref())
                };
                crate::dto::PointDefinition {
                    point_id,
                    signal_name,
                    scale,
                    offset,
                    unit,
                    data_type,
                    reverse,
                    description,
                    protocol_mapping,
                }
            },
        )
        .collect())
}

/// Fetch grouped points for a channel with optional type filter and unmapped filter
pub(super) async fn fetch_grouped_points(
    pool: &sqlx::SqlitePool,
    channel_id: u32,
    type_filter: Option<&str>,
    unmapped_only: bool,
) -> Result<crate::dto::GroupedPoints, AppError> {
    // Validate type filter if provided
    if let Some(filter) = type_filter {
        point_type_to_table(filter)?;
    }

    const TABLES: [(&str, &str); 4] = [
        ("T", "telemetry_points"),
        ("S", "signal_points"),
        ("C", "control_points"),
        ("A", "adjustment_points"),
    ];

    let mut grouped = crate::dto::GroupedPoints {
        telemetry: Vec::new(),
        signal: Vec::new(),
        control: Vec::new(),
        adjustment: Vec::new(),
    };

    for &(type_letter, table) in &TABLES {
        if let Some(filter) = type_filter
            && !filter.eq_ignore_ascii_case(type_letter)
        {
            continue;
        }

        let points = fetch_point_definitions(pool, table, channel_id, unmapped_only).await?;

        match type_letter {
            "T" => grouped.telemetry = points,
            "S" => grouped.signal = points,
            "C" => grouped.control = points,
            "A" => grouped.adjustment = points,
            _ => {
                return Err(AppError::internal_error(format!(
                    "Unsupported point type in table list: {}",
                    type_letter
                )));
            },
        }
    }

    Ok(grouped)
}

// ----------------------------------------------------------------------------
// Validation Helper Functions
// ----------------------------------------------------------------------------

/// Validate that a channel exists
pub(crate) async fn validate_channel_exists(
    pool: &sqlx::SqlitePool,
    channel_id: u32,
) -> Result<(), AppError> {
    let exists: Option<(i64,)> =
        sqlx::query_as("SELECT channel_id FROM channels WHERE channel_id = ?")
            .bind(channel_id as i64)
            .fetch_optional(pool)
            .await
            .map_err(|e| {
                tracing::error!("Ch check: {}", e);
                AppError::internal_error("Database operation failed")
            })?;

    if exists.is_none() {
        return Err(AppError::not_found(format!(
            "Channel {} not found",
            channel_id
        )));
    }

    Ok(())
}

// ============================================================================
// Auto-Reload Helper Functions
// ============================================================================

/// Reconcile the affected channel through the shared application boundary.
///
/// The operation is awaited so a detached stale snapshot can never reactivate
/// a channel after a later disable or delete.
pub async fn trigger_channel_reload_if_needed(
    channel_id: u32,
    state: &AppState,
    auto_reload: bool,
) -> bool {
    if !auto_reload {
        tracing::debug!(
            "Auto-reload disabled for channel {}, skipping hot reload",
            channel_id
        );
        return false;
    }

    let Some(application) = &state.channel_reconciliation else {
        tracing::warn!(
            "Ch{} reconciliation deferred: application boundary unavailable",
            channel_id
        );
        return false;
    };
    let request_id = uuid::Uuid::new_v4().to_string();
    let timestamp = chrono::Utc::now().timestamp_millis().max(0) as u64;
    let context = aether_application::RequestContext::new(
        &request_id,
        aether_application::Actor::new("io.point-topology").with_permission("io.channel.manage"),
        true,
        aether_domain::TimestampMs::new(timestamp),
    );
    match application
        .reconcile(
            &context,
            aether_ports::ChannelReconciliationScope::One(aether_domain::ChannelId::new(
                channel_id,
            )),
        )
        .await
    {
        Ok(acceptance) => {
            let converged = !acceptance.reconciliation_required();
            if converged {
                tracing::debug!("Ch{} reconciled", channel_id);
            } else {
                tracing::warn!(
                    request_id = acceptance.request_id(),
                    "Ch{} reconciliation remains degraded",
                    channel_id
                );
            }
            converged
        },
        Err(error) => {
            tracing::error!(
                request_id,
                "Ch{} reconciliation failed: {}",
                channel_id,
                error
            );
            false
        },
    }
}
