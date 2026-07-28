//! Channel Query and Status Handlers
//!
//! Provides endpoints for querying channel information and status.

#![allow(clippy::disallowed_methods)] // json! macro used in multiple functions

use axum::{
    extract::{Path, Query, State},
    response::Json,
};
use chrono::{DateTime, Utc};

use crate::api::routes::AppState;
use crate::dto::{
    AppError, ChannelConfig, ChannelDetail, ChannelListQuery, ChannelRuntimeStatus,
    ChannelStatusDto, ChannelStatusResponse, PaginatedResponse, PointCounts, SuccessResponse,
};

/// Extract description field from config JSON string
fn extract_description_from_config(
    config_str: Option<&str>,
    channel_id: u32,
) -> Result<Option<String>, AppError> {
    let Some(s) = config_str else {
        return Ok(None);
    };
    let v: serde_json::Value = serde_json::from_str(s).map_err(|e| {
        tracing::error!("Ch{} invalid config JSON: {}", channel_id, e);
        AppError::internal_error(format!(
            "Invalid channel config JSON for {}: {}",
            channel_id, e
        ))
    })?;
    let obj = v.as_object().ok_or_else(|| {
        tracing::error!("Ch{} config must be a JSON object", channel_id);
        AppError::internal_error(format!(
            "Invalid channel config for {}: expected JSON object",
            channel_id
        ))
    })?;
    Ok(obj
        .get("description")
        .and_then(|d| d.as_str())
        .map(String::from))
}

/// List all channels with pagination (configuration and runtime status summary).
///
/// Results are ordered by `channel_id` ascending by default. Supports filtering by
/// `enabled`, `protocol`, and `keyword` (fuzzy match on name / address). Each record
/// includes connection state (connected / disconnected), last successful poll time, and
/// cumulative error counts, but **excludes** the full point list (query points via
/// `/channels/{id}` or `/points`). The response is moderately heavy; always supply a
/// `page_size` limit from the frontend list page.
#[utoipa::path(
    get,
    path = "/api/channels",
    params(
        ("page" = Option<usize>, Query, description = "Page number (default: 1)"),
        ("page_size" = Option<usize>, Query, description = "Items per page (default: 20)"),
        ("protocol" = Option<String>, Query, description = "Filter by protocol type"),
        ("enabled" = Option<bool>, Query, description = "Filter by enabled status"),
        ("connected" = Option<bool>, Query, description = "Filter by connection status")
    ),
    responses(
        (status = 200, description = "Paginated list of channels, including the desired-state revision used by x-aether-expected-revision", body = common::SuccessResponse<crate::dto::PaginatedResponse<crate::dto::ChannelStatusResponse>>,
            example = json!({
                "success": true,
                "data": {
                  "list": [
                    {
                        "id": 1,
                        "revision": 3,
                        "name": "PLC#1",
                        "protocol": "modbus_tcp",
                        "description": "Packaging Line Controller #1",
                        "enabled": true,
                        "connected": true,
                        "last_update": "2025-10-15T10:30:00Z"
                    },
                    {
                        "id": 2,
                        "revision": 1,
                        "name": "HVAC#1",
                        "protocol": "modbus_tcp",
                        "description": "Building Climate Controller #1",
                        "enabled": true,
                        "connected": true,
                        "last_update": "2025-10-15T10:28:15Z"
                    },
                    {
                        "id": 3,
                        "revision": 7,
                        "name": "METER#1",
                        "protocol": "modbus_rtu",
                        "description": "Serial Process Meter #1",
                        "enabled": true,
                        "connected": false,
                        "last_update": "2025-10-15T10:25:00Z"
                    },
                    {
                        "id": 4,
                        "revision": 2,
                        "name": "ECU1170_GPIO",
                        "protocol": "di_do",
                        "description": "ECU-1170 Onboard DI/DO",
                        "enabled": false,
                        "connected": false,
                        "last_update": "2025-10-15T10:30:05Z"
                    }
                ],
                  "page": 1,
                  "page_size": 20,
                  "total": 4
                }
            })
        )
    ),
    tag = "io"
)]
pub async fn get_all_channels(
    State(state): State<AppState>,
    Query(query): Query<ChannelListQuery>,
) -> Result<Json<SuccessResponse<PaginatedResponse<ChannelStatusResponse>>>, AppError> {
    // Load all channels from database first
    let db_channels: Vec<(i64, String, String, bool, Option<String>, i64)> = sqlx::query_as(
        "SELECT channel_id, name, protocol, enabled, config, revision FROM channels",
    )
    .fetch_all(&state.sqlite_pool)
    .await
    .map_err(|e| {
        tracing::error!("Load channels: {}", e);
        AppError::internal_error(format!("Failed to load channels from database: {}", e))
    })?;

    // Direct access without RwLock (lock-free)
    let manager = &state.channel_manager;
    let mut all_channels = Vec::new();

    for (id, name, protocol, enabled, config_str, revision) in db_channels {
        let channel_id = u32::try_from(id)
            .map_err(|_| AppError::internal_error(format!("Channel ID {} out of range", id)))?;
        let revision = u64::try_from(revision).map_err(|_| {
            AppError::internal_error(format!(
                "Channel {channel_id} has an invalid desired-state revision"
            ))
        })?;
        if revision == 0 {
            return Err(AppError::internal_error(format!(
                "Channel {channel_id} has an invalid desired-state revision"
            )));
        }

        let description = extract_description_from_config(config_str.as_deref(), channel_id)?;

        // Get runtime status if channel is running
        let (connected, last_update) = match manager.get_channel(channel_id) {
            Some(entry) => {
                let status = entry.get_status().await;
                (
                    status.is_connected,
                    DateTime::<Utc>::from_timestamp(status.last_update, 0).unwrap_or_else(Utc::now),
                )
            },
            _ => (false, Utc::now()),
        };

        let channel_response = ChannelStatusResponse {
            id: channel_id,
            revision,
            name,
            description,
            protocol: protocol.clone(),
            enabled,
            connected,
            last_update,
        };

        // Apply filters
        let matches = query.protocol.as_ref().is_none_or(|p| &protocol == p)
            && query.enabled.is_none_or(|e| enabled == e)
            && query.connected.is_none_or(|c| connected == c);

        if matches {
            all_channels.push(channel_response);
        }
    }

    // Use shared pagination utility
    let paginated_response =
        PaginatedResponse::from_slice(all_channels, query.page, query.page_size);

    Ok(Json(SuccessResponse::new(paginated_response)))
}

/// Current channel runtime status (lightweight).
///
/// Returns only `is_connected` and `last_update` timestamp without querying channel
/// configuration or points — intended for frontend status indicator polling. For full
/// information use `/api/channels/{id}`. Note: `is_connected` checks both TCP state
/// and data freshness; a channel that is TCP-connected but has not received data for
/// 90 s will return `false`.
#[utoipa::path(
    get,
    path = "/api/channels/{id}/status",
    params(
        ("id" = String, Path, description = "Channel identifier")
    ),
    responses(
        (status = 200, description = "Channel status", body = crate::dto::ChannelStatusDto,
            example = json!({
                "success": true,
                "data": {
                    "id": 1,
                    "name": "PLC#1",
                    "protocol": "modbus_tcp",
                    "connected": true,
                    "running": true,
                    "last_update": "2025-10-15T10:30:15Z",
                    "statistics": {
                        "total_reads": 15234,
                        "successful_reads": 15230,
                        "failed_reads": 4,
                        "total_writes": 128,
                        "successful_writes": 128,
                        "failed_writes": 0,
                        "uptime_seconds": 86400,
                        "avg_response_time_ms": 12.5
                    }
                }
            })
        )
    ),
    tag = "io"
)]
pub async fn get_channel_status(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<SuccessResponse<ChannelStatusDto>>, AppError> {
    let id_u16 = id
        .parse::<u32>()
        .map_err(|_| AppError::bad_request(format!("Invalid channel ID format: {}", id)))?;
    // Direct access without RwLock (lock-free)
    let manager = &state.channel_manager;

    match manager.get_channel(id_u16) {
        Some(entry) => {
            let (name, protocol) = manager
                .get_channel_metadata(id_u16)
                .unwrap_or_else(|| (format!("Channel {id_u16}"), "Unknown".to_string()));

            let channel_status = entry.get_status().await;
            let is_running = entry.is_connected();
            let diagnostics = entry.get_diagnostics(id_u16);

            let status = ChannelStatusDto {
                id: id_u16,
                name,
                protocol,
                connected: channel_status.is_connected,
                running: is_running,
                last_update: DateTime::<Utc>::from_timestamp(channel_status.last_update, 0)
                    .unwrap_or_else(Utc::now),
                statistics: diagnostics
                    .as_object()
                    .map(|obj| obj.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
                    .unwrap_or_default(),
            };
            Ok(Json(SuccessResponse::new(status)))
        },
        _ => Err(AppError::not_found(format!("Channel {} not found", id_u16))),
    }
}

/// Full channel details: configuration + runtime state + cumulative statistics.
///
/// Returns all operations-facing information for a channel in a single response:
/// protocol configuration (driver, address, poll interval, timeouts), current connection
/// state, cumulative read/write counts and error counts, the latest diagnostic snapshot,
/// and registered point counts. Use this for the channel detail page in the frontend.
/// The response is large — use `/channels` (paginated) for list summaries and call this
/// only for the detail page.
#[utoipa::path(
    get,
    path = "/api/channels/{id}",
    params(
        ("id" = String, Path, description = "Channel identifier")
    ),
    responses(
        (status = 200, description = "Channel details, including the desired-state revision used by x-aether-expected-revision", body = common::SuccessResponse<crate::dto::ChannelDetail>,
            example = json!({
                "success": true,
                "data": {
                    "id": 1,
                    "revision": 3,
                    "name": "PLC#1",
                    "description": "Packaging Line Controller #1",
                    "protocol": "modbus_tcp",
                    "enabled": true,
                    "parameters": {
                        "host": "192.168.1.10",
                        "port": 502,
                        "read_timeout_ms": 3000
                    },
                    "runtime_status": {
                        "connected": true,
                        "running": true,
                        "last_update": "2025-10-15T10:30:15Z",
                        "statistics": {
                            "total_reads": 15234,
                            "successful_reads": 15230,
                            "failed_reads": 4,
                            "uptime_seconds": 86400
                        }
                    },
                    "point_counts": {
                        "telemetry": 45,
                        "signal": 12,
                        "control": 8,
                        "adjustment": 6
                    }
                }
            })
        )
    ),
    tag = "io"
)]
pub async fn get_channel_detail_handler(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<SuccessResponse<ChannelDetail>>, AppError> {
    let id_u16 = id
        .parse::<u32>()
        .map_err(|_| AppError::bad_request(format!("Invalid channel ID format: {}", id)))?;

    let row = sqlx::query_as::<_, (String, String, bool, Option<String>, i64)>(
        "SELECT name, protocol, enabled, config, revision FROM channels WHERE channel_id = ?",
    )
    .bind(id_u16 as i64)
    .fetch_optional(&state.sqlite_pool)
    .await
    .map_err(|e| {
        tracing::error!("Load channel {}: {}", id_u16, e);
        AppError::internal_error("Database operation failed")
    })?;

    let Some((name, protocol, enabled, config_str, revision)) = row else {
        return Err(AppError::not_found(format!("Channel {} not found", id_u16)));
    };
    let revision = u64::try_from(revision).map_err(|_| {
        AppError::internal_error(format!(
            "Channel {id_u16} has an invalid desired-state revision"
        ))
    })?;
    if revision == 0 {
        return Err(AppError::internal_error(format!(
            "Channel {id_u16} has an invalid desired-state revision"
        )));
    }

    let mut obj = match config_str {
        None => serde_json::Map::new(),
        Some(s) => {
            let v: serde_json::Value = serde_json::from_str(&s).map_err(|e| {
                tracing::error!("Ch{} invalid config JSON: {}", id_u16, e);
                AppError::internal_error(format!(
                    "Invalid channel config JSON for {}: {}",
                    id_u16, e
                ))
            })?;
            // Use match to move the Map out of Value (avoid clone)
            match v {
                serde_json::Value::Object(map) => map,
                _ => {
                    tracing::error!("Ch{} config must be a JSON object", id_u16);
                    return Err(AppError::internal_error(format!(
                        "Invalid channel config for {}: expected JSON object",
                        id_u16
                    )));
                },
            }
        },
    };

    // Extract description
    let description = match obj.remove("description") {
        None => None,
        Some(d) => Some(
            d.as_str()
                .ok_or_else(|| {
                    tracing::error!("Ch{} config field 'description' must be a string", id_u16);
                    AppError::internal_error(format!(
                        "Invalid channel config for {}: 'description' must be a string",
                        id_u16
                    ))
                })?
                .to_string(),
        ),
    };

    // Extract logging config
    let logging_config = match obj.remove("logging") {
        None => crate::core::config::ChannelLoggingConfig::default(),
        Some(l) => {
            serde_json::from_value::<crate::core::config::ChannelLoggingConfig>(l).map_err(|e| {
                tracing::error!("Ch{} invalid logging config: {}", id_u16, e);
                AppError::internal_error(format!(
                    "Invalid channel logging config for {}: {}",
                    id_u16, e
                ))
            })?
        },
    };

    // Extract parameters (the actual protocol parameters)
    // Use match to move the Map out of Value (avoid clone)
    let parameters = match obj.remove("parameters") {
        None => std::collections::HashMap::new(),
        Some(serde_json::Value::Object(map)) => map.into_iter().collect(),
        Some(_) => {
            tracing::error!("Ch{} config field 'parameters' must be an object", id_u16);
            return Err(AppError::internal_error(format!(
                "Invalid channel config for {}: 'parameters' must be an object",
                id_u16
            )));
        },
    };

    // Direct access without RwLock (lock-free)
    let manager = &state.channel_manager;
    let (connected, last_update, statistics) = match manager.get_channel(id_u16) {
        Some(entry) => {
            let status = entry.get_status().await;
            let diag = entry.get_diagnostics(id_u16);
            (
                status.is_connected,
                DateTime::<Utc>::from_timestamp(status.last_update, 0).unwrap_or_else(Utc::now),
                diag.as_object()
                    .map(|obj| obj.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
                    .unwrap_or_default(),
            )
        },
        _ => (false, Utc::now(), std::collections::HashMap::new()),
    };

    let config = ChannelConfig {
        core: crate::core::config::ChannelCore {
            id: id_u16,
            name,
            description,
            protocol,
            enabled,
        },
        parameters,
        logging: logging_config,
    };

    // Query actual point counts by type for this channel
    let point_tables = [
        "telemetry_points",
        "signal_points",
        "control_points",
        "adjustment_points",
    ];
    let mut counts = [0usize; 4];
    for (i, table) in point_tables.iter().enumerate() {
        let sql = format!("SELECT COUNT(*) FROM {} WHERE channel_id = ?", table);
        counts[i] = sqlx::query_scalar::<_, i64>(&sql)
            .bind(id_u16 as i64)
            .fetch_one(&state.sqlite_pool)
            .await
            .unwrap_or(0) as usize;
    }

    let detail = ChannelDetail {
        config,
        revision,
        runtime_status: ChannelRuntimeStatus {
            connected,
            running: connected,
            last_update,
            statistics,
        },
        point_counts: PointCounts {
            telemetry: counts[0],
            signal: counts[1],
            control: counts[2],
            adjustment: counts[3],
        },
    };

    Ok(Json(SuccessResponse::new(detail)))
}
