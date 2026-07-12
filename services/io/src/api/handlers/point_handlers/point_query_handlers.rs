#![allow(clippy::disallowed_methods)]

//! Query handlers for point information, configuration, and unmapped points

use crate::api::routes::AppState;
use crate::dto::{AppError, SuccessResponse};
use aether_domain::PointKind;
use aether_model::PointType;
use axum::{
    extract::{Path, Query, State},
    response::Json,
};

use super::point_helpers::{
    fetch_grouped_points, parse_protocol_mapping_json, point_type_to_table, validate_channel_exists,
};

/// Read the real-time value of a single point (value + timestamp + raw).
///
/// Reads the authoritative SHM slot and returns its engineering value,
/// timestamp, and raw protocol value. An unwritten NaN slot is represented as
/// `null` rather than being mistaken for a real zero.
#[utoipa::path(
    get,
    path = "/api/channels/{channel_id}/{telemetry_type}/{point_id}",
    params(
        ("channel_id" = u32, Path, description = "Channel identifier"),
        ("telemetry_type" = String, Path, description = "Point type: T, S, C, or A"),
        ("point_id" = u32, Path, description = "Point identifier")
    ),
    responses(
        (status = 200, description = "Point information", body = serde_json::Value,
            example = json!({
                "success": true,
                "data": {
                    "channel_id": 1,
                    "telemetry_type": "T",
                    "point_id": 101,
                    "value": "650.5",
                    "timestamp": "1729000815",
                    "raw": "6505"
                }
            })
        )
    ),
    tag = "io"
)]
pub async fn get_point_info_handler(
    State(state): State<AppState>,
    Path((channel_id, telemetry_type, point_id)): Path<(u32, String, u32)>,
) -> Result<Json<SuccessResponse<serde_json::Value>>, AppError> {
    let point_type = PointType::from_str(&telemetry_type).ok_or_else(|| {
        AppError::bad_request(format!(
            "Invalid telemetry type '{}'. Must be T, S, C, or A",
            telemetry_type
        ))
    })?;

    let layout = state
        .channel_manager
        .shm_handle()
        .generation()
        .ok_or_else(|| AppError::service_unavailable("authoritative SHM is unavailable"))?;
    let kind = match point_type {
        PointType::Telemetry => PointKind::Telemetry,
        PointType::Signal => PointKind::Status,
        PointType::Control => PointKind::Command,
        PointType::Adjustment => PointKind::Action,
    };
    let sample = layout
        .manifest()
        .slot(channel_id, kind, point_id)
        .and_then(|slot| layout.read_slot(slot));
    let value = sample
        .filter(|sample| sample.value.is_finite())
        .map(|sample| sample.value.to_string());
    let timestamp = sample.map(|sample| sample.timestamp_ms.to_string());
    let raw_value = sample
        .filter(|sample| sample.raw.is_finite())
        .map(|sample| sample.raw.to_string());

    Ok(Json(SuccessResponse::new(serde_json::json!({
        "channel_id": channel_id,
        "telemetry_type": point_type.as_str(),
        "point_id": point_id,
        "value": value,
        "timestamp": timestamp,
        "raw": raw_value,
        "source": "shm"
    }))))
}

/// Get list of points for a channel, optionally filtered by type
///
/// Returns all point definitions for the specified channel.
/// Supports filtering by point type (T, S, C, A).
#[utoipa::path(
    get,
    path = "/api/channels/{id}/points",
    params(
        ("id" = u32, Path, description = "Channel identifier"),
        ("type" = Option<String>, Query, description = "Point type filter: T (telemetry), S (signal), C (control), A (adjustment)")
    ),
    responses(
        (status = 200, description = "Points retrieved (grouped)", body = crate::dto::GroupedPoints,
            example = json!({
                "success": true,
                "data": {
                    "telemetry": [
                        {
                            "point_id": 101,
                            "signal_name": "DC_Voltage",
                            "scale": 0.1,
                            "offset": 0.0,
                            "unit": "V",
                            "data_type": "uint16",
                            "reverse": false,
                            "description": "DC bus voltage",
                            "protocol_mapping": {
                                "slave_id": 1,
                                "function_code": 3,
                                "register_address": 100,
                                "data_type": "float32",
                                "byte_order": "ABCD",
                                "bit_position": 0
                            }
                        }
                    ],
                    "signal": [],
                    "control": [],
                    "adjustment": []
                }
            })
        )
    ),
    tag = "io"
)]
pub async fn get_channel_points_handler(
    Path(channel_id): Path<u32>,
    Query(params): Query<std::collections::HashMap<String, String>>,
    State(state): State<AppState>,
) -> Result<Json<SuccessResponse<crate::dto::GroupedPoints>>, AppError> {
    validate_channel_exists(&state.sqlite_pool, channel_id).await?;
    let type_filter = params.get("type").map(|s| s.as_str());
    let grouped = fetch_grouped_points(&state.sqlite_pool, channel_id, type_filter, false).await?;
    Ok(Json(SuccessResponse::new(grouped)))
}

/// Get mapping for a specific point with explicit four-remote type
///
/// Unique identifier: (channel_id, four_remote_type, point_id)
#[utoipa::path(
    get,
    path = "/api/channels/{channel_id}/{type}/points/{point_id}/mapping",
    params(
        ("channel_id" = u32, Path, description = "Channel identifier"),
        ("type" = String, Path, description = "Four-remote type: T(Telemetry), S(Signal), C(Control), A(Adjustment)"),
        ("point_id" = u32, Path, description = "Point identifier")
    ),
    responses(
        (status = 200, description = "Mapping retrieved successfully", body = crate::dto::PointMappingDetail),
        (status = 400, description = "Invalid four-remote type (must be T, S, C, or A)"),
        (status = 404, description = "Channel or point not found in specified type")
    ),
    tag = "io"
)]
pub async fn get_point_mapping_with_type_handler(
    Path((channel_id, point_type, point_id)): Path<(u32, String, u32)>,
    State(state): State<AppState>,
) -> Result<Json<SuccessResponse<crate::dto::PointMappingDetail>>, AppError> {
    let table = point_type_to_table(&point_type)?;
    validate_channel_exists(&state.sqlite_pool, channel_id).await?;

    let query = format!(
        "SELECT signal_name, protocol_mappings FROM {} WHERE channel_id = ? AND point_id = ?",
        table
    );

    let result: Option<(String, Option<String>)> = sqlx::query_as(&query)
        .bind(channel_id as i64)
        .bind(point_id as i64)
        .fetch_optional(&state.sqlite_pool)
        .await
        .map_err(|e| {
            tracing::error!("Query {}: {}", table, e);
            AppError::internal_error("Database operation failed")
        })?;

    let (signal_name, protocol_mappings_json) = result.ok_or_else(|| {
        AppError::not_found(format!(
            "Point {} (type {}) not found in channel {}",
            point_id,
            point_type.to_uppercase(),
            channel_id
        ))
    })?;

    // For mapping endpoints, default to empty object instead of None
    let protocol_data = parse_protocol_mapping_json(protocol_mappings_json.as_deref())
        .unwrap_or_else(|| serde_json::Value::Object(serde_json::Map::new()));

    Ok(Json(SuccessResponse::new(crate::dto::PointMappingDetail {
        point_id,
        signal_name,
        protocol_data,
    })))
}

// ----------------------------------------------------------------------------
// Get Point Configuration Handler
// ----------------------------------------------------------------------------

/// Read the **configuration** of a point (not its runtime value).
///
/// Reads the point definition from SQLite: register address, byte order, scale factor,
/// unit, alarm limits, etc. Does not query live SHM — returns static configuration
/// only. Use this to pre-populate the "edit point" dialog in the frontend. For the
/// real-time value use `/api/channels/{channel_id}/{type}/{point_id}`.
async fn get_point_config_handler_inner(
    channel_id: u32,
    point_type: &str,
    point_id: u32,
    state: AppState,
) -> Result<Json<SuccessResponse<crate::dto::PointDefinition>>, AppError> {
    let table = point_type_to_table(point_type)?;
    validate_channel_exists(&state.sqlite_pool, channel_id).await?;

    let query = format!(
        "SELECT point_id, signal_name, scale, offset, unit, data_type, reverse, \
         description, protocol_mappings \
         FROM {} WHERE channel_id = ? AND point_id = ?",
        table
    );

    #[allow(clippy::type_complexity)]
    let result: Option<(
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
        .bind(point_id as i64)
        .fetch_optional(&state.sqlite_pool)
        .await
        .map_err(|e| {
            tracing::error!("Query point config: {}", e);
            AppError::internal_error("Database operation failed")
        })?;

    let (pt_id, signal_name, scale, offset, unit, data_type, reverse, description, pm_json) =
        result.ok_or_else(|| {
            AppError::not_found(format!(
                "Point {} (type {}) not found in channel {}",
                point_id,
                point_type.to_ascii_uppercase(),
                channel_id
            ))
        })?;

    Ok(Json(SuccessResponse::new(crate::dto::PointDefinition {
        point_id: pt_id,
        signal_name,
        scale,
        offset,
        unit,
        data_type,
        reverse,
        description,
        protocol_mapping: parse_protocol_mapping_json(pm_json.as_deref()),
    })))
}

// ============================================================================
// Unmapped Points Query Handler
// ============================================================================

/// Get unmapped points for a channel (points without protocol_mappings)
///
/// **Unmapped Definition**: Points where `protocol_mappings IS NULL OR '' OR '{}' OR 'null'`
#[utoipa::path(
    get,
    path = "/api/channels/{id}/unmapped-points",
    params(
        ("id" = u32, Path, description = "Channel identifier"),
        ("type" = Option<String>, Query, description = "Point type filter: T (telemetry), S (signal), C (control), A (adjustment)")
    ),
    responses(
        (status = 200, description = "Unmapped points retrieved (grouped by type)", body = crate::dto::GroupedPoints,
            example = json!({
                "success": true,
                "data": {
                    "telemetry": [
                        {
                            "point_id": 101,
                            "signal_name": "DC_Voltage",
                            "scale": 0.1,
                            "offset": 0.0,
                            "unit": "V",
                            "data_type": "uint16",
                            "reverse": false,
                            "description": "DC bus voltage",
                            "protocol_mapping": null
                        }
                    ],
                    "signal": [],
                    "control": [],
                    "adjustment": []
                }
            })
        )
    ),
    tag = "io"
)]
pub async fn get_unmapped_points_handler(
    Path(channel_id): Path<u32>,
    Query(params): Query<std::collections::HashMap<String, String>>,
    State(state): State<AppState>,
) -> Result<Json<SuccessResponse<crate::dto::GroupedPoints>>, AppError> {
    validate_channel_exists(&state.sqlite_pool, channel_id).await?;
    let type_filter = params.get("type").map(|s| s.as_str());
    let grouped = fetch_grouped_points(&state.sqlite_pool, channel_id, type_filter, true).await?;
    Ok(Json(SuccessResponse::new(grouped)))
}

// ============================================================================
// Type-specific GET wrapper handlers (delegate to *_inner functions)
// ============================================================================

/// Get telemetry point configuration
#[utoipa::path(
    get,
    path = "/api/channels/{channel_id}/T/points/{point_id}",
    params(
        ("channel_id" = u32, Path, description = "Channel identifier"),
        ("point_id" = u32, Path, description = "Telemetry point identifier")
    ),
    responses(
        (status = 200, description = "Telemetry point configuration", body = crate::dto::PointDefinition),
        (status = 404, description = "Channel or telemetry point not found")
    ),
    tag = "io"
)]
pub async fn get_telemetry_point_config_handler(
    Path((channel_id, point_id)): Path<(u32, u32)>,
    State(state): State<AppState>,
) -> Result<Json<SuccessResponse<crate::dto::PointDefinition>>, AppError> {
    get_point_config_handler_inner(channel_id, "T", point_id, state).await
}

/// Get signal point configuration
#[utoipa::path(
    get,
    path = "/api/channels/{channel_id}/S/points/{point_id}",
    params(
        ("channel_id" = u32, Path, description = "Channel identifier"),
        ("point_id" = u32, Path, description = "Signal point identifier")
    ),
    responses(
        (status = 200, description = "Signal point configuration", body = crate::dto::PointDefinition),
        (status = 404, description = "Channel or signal point not found")
    ),
    tag = "io"
)]
pub async fn get_signal_point_config_handler(
    Path((channel_id, point_id)): Path<(u32, u32)>,
    State(state): State<AppState>,
) -> Result<Json<SuccessResponse<crate::dto::PointDefinition>>, AppError> {
    get_point_config_handler_inner(channel_id, "S", point_id, state).await
}

/// Get control point configuration
#[utoipa::path(
    get,
    path = "/api/channels/{channel_id}/C/points/{point_id}",
    params(
        ("channel_id" = u32, Path, description = "Channel identifier"),
        ("point_id" = u32, Path, description = "Control point identifier")
    ),
    responses(
        (status = 200, description = "Control point configuration", body = crate::dto::PointDefinition),
        (status = 404, description = "Channel or control point not found")
    ),
    tag = "io"
)]
pub async fn get_control_point_config_handler(
    Path((channel_id, point_id)): Path<(u32, u32)>,
    State(state): State<AppState>,
) -> Result<Json<SuccessResponse<crate::dto::PointDefinition>>, AppError> {
    get_point_config_handler_inner(channel_id, "C", point_id, state).await
}

/// Get adjustment point configuration
#[utoipa::path(
    get,
    path = "/api/channels/{channel_id}/A/points/{point_id}",
    params(
        ("channel_id" = u32, Path, description = "Channel identifier"),
        ("point_id" = u32, Path, description = "Adjustment point identifier")
    ),
    responses(
        (status = 200, description = "Adjustment point configuration", body = crate::dto::PointDefinition),
        (status = 404, description = "Channel or adjustment point not found")
    ),
    tag = "io"
)]
pub async fn get_adjustment_point_config_handler(
    Path((channel_id, point_id)): Path<(u32, u32)>,
    State(state): State<AppState>,
) -> Result<Json<SuccessResponse<crate::dto::PointDefinition>>, AppError> {
    get_point_config_handler_inner(channel_id, "A", point_id, state).await
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod cache_tests {
    use super::*;
    use axum::extract::{Path, State};
    use std::collections::BTreeMap;
    use std::sync::Arc;

    use crate::api::routes::AppState;
    use crate::core::channels::ChannelManager;
    use aether_routing::RoutingCache;

    async fn create_test_sqlite_pool() -> sqlx::SqlitePool {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();

        common::test_utils::schema::init_io_schema(&pool)
            .await
            .unwrap();

        pool
    }

    async fn create_test_state() -> AppState {
        let sqlite_pool = create_test_sqlite_pool().await;
        let routing_cache = Arc::new(RoutingCache::default());
        let shm_handle = crate::test_utils::create_test_shm_handle_with_points(BTreeMap::from([(
            1,
            [103, 0, 0, 0],
        )]));
        let channel_manager = Arc::new(
            ChannelManager::new(shm_handle, routing_cache)
                .expect("create SHM-backed channel manager"),
        );
        let command_tx_cache = Arc::new(crate::api::command_cache::CommandTxCache::new());

        AppState {
            channel_manager,
            sqlite_pool,
            command_tx_cache,
            allow_simulation_writes: false,
            channel_reconciliation: None,
        }
    }

    #[tokio::test]
    async fn test_get_point_info_from_shm() {
        let channel_id: u32 = 1;
        let point_id: u32 = 102;
        let state = create_test_state().await;
        let layout = state
            .channel_manager
            .shm_handle()
            .generation()
            .expect("test SHM layout");
        let address = aether_domain::ChannelPointAddress::new(
            aether_domain::ChannelId::new(channel_id),
            PointKind::Telemetry,
            aether_domain::PointId::new(point_id),
        )
        .expect("telemetry address");
        let sample = aether_domain::AcquiredPointSample::new(
            address,
            750.0,
            7500.0,
            aether_domain::TimestampMs::new(1_729_001_000),
            aether_domain::PointQuality::Good,
        )
        .expect("finite sample");
        layout
            .acquisition_writer()
            .commit_batch(&[sample])
            .expect("seed telemetry through the acquisition port");

        let result =
            get_point_info_handler(State(state), Path((channel_id, "T".to_string(), point_id)))
                .await;

        let response = result.expect("Handler should succeed");
        let data = &response.0.data;
        assert_eq!(data["source"], "shm");
        assert_eq!(data["value"], "750");
        assert_eq!(data["raw"], "7500");
        assert_eq!(data["timestamp"], "1729001000");
    }

    #[tokio::test]
    async fn test_get_point_info_invalid_type() {
        let state = create_test_state().await;

        let result = get_point_info_handler(
            State(state),
            Path((1, "X".to_string(), 100)), // "X" is invalid
        )
        .await;

        let err = result.expect_err("Should return error for invalid type");
        assert!(
            format!("{:?}", err).contains("Invalid telemetry type"),
            "Error should mention invalid type"
        );
    }
}
