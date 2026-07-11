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

/// Validate that a point ID is unique within a channel
pub(super) async fn validate_point_uniqueness(
    pool: &sqlx::SqlitePool,
    channel_id: u32,
    table: &str,
    point_id: u32,
) -> Result<(), AppError> {
    let query = format!(
        "SELECT point_id FROM {} WHERE channel_id = ? AND point_id = ?",
        table
    );

    let exists: Option<(i64,)> = sqlx::query_as(&query)
        .bind(channel_id as i64)
        .bind(point_id as i64)
        .fetch_optional(pool)
        .await
        .map_err(|e| {
            tracing::error!("Point uniqueness check: {}", e);
            AppError::internal_error("Database operation failed")
        })?;

    if exists.is_some() {
        return Err(AppError::conflict(format!(
            "Point {} already exists in channel {}",
            point_id, channel_id
        )));
    }

    Ok(())
}

// ============================================================================
// Auto-Reload Helper Functions
// ============================================================================

/// Trigger channel reload if auto_reload is enabled
///
/// Uses `tokio::spawn` for async execution to avoid blocking the API response.
pub async fn trigger_channel_reload_if_needed(
    channel_id: u32,
    state: &AppState,
    auto_reload: bool,
) {
    if !auto_reload {
        tracing::debug!(
            "Auto-reload disabled for channel {}, skipping hot reload",
            channel_id
        );
        return;
    }

    tracing::debug!("Ch{} auto-reload", channel_id);

    let state_clone = state.clone();
    tokio::spawn(async move {
        match perform_channel_reload(channel_id, &state_clone).await {
            Err(e) => {
                tracing::error!("Ch{} reload: {}", channel_id, e);
            },
            _ => {
                tracing::debug!("Ch{} reloaded", channel_id);
            },
        }
    });
}

/// Perform channel reload (load config from SQLite and hot-reload)
async fn perform_channel_reload(channel_id: u32, state: &AppState) -> anyhow::Result<()> {
    use crate::core::channels::channel_manager::ChannelManager;

    let config = ChannelManager::load_channel_from_db(&state.sqlite_pool, channel_id)
        .await
        .map_err(|e| anyhow::anyhow!("Failed to load channel config: {}", e))?;

    let manager = &state.channel_manager;
    if let Err(e) = manager.remove_channel(channel_id).await {
        tracing::warn!("Ch{} remove: {}", channel_id, e);
    }

    let entry = manager
        .create_channel(std::sync::Arc::new(config))
        .await
        .map_err(|e| anyhow::anyhow!("Failed to create channel: {}", e))?;

    tokio::spawn(async move {
        match entry.connect().await {
            Ok(_) => tracing::debug!("Ch{} connected", channel_id),
            Err(e) => tracing::warn!("Ch{} connect: {}", channel_id, e),
        }
    });

    Ok(())
}
