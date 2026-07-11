//! Control and command handlers for channel operations
//!
//! This module contains handlers for:
//! - Channel control operations (start, stop, restart)
//! - Point-level control commands
//! - Point-level adjustment commands
//! - Batch control and adjustment operations

#![allow(clippy::disallowed_methods)] // json! macro used in multiple functions

use crate::api::routes::AppState;
use crate::dto::{AppError, ChannelOperation, SuccessResponse, WritePointRequest, WriteResponse};
use aether_model::PointType;
use axum::{
    extract::{Path, State},
    response::Json,
};

/// Connect / disconnect / restart a channel's protocol runtime.
///
/// `operation` accepts `start` (connect to device, begin polling),
/// `stop` (disconnect cleanly, stop polling), or `restart` (cycle the
/// connection without rewriting routing/SHM). Hot operations: SHM
/// layout and routing cache are unaffected — only the protocol
/// adapter's TCP / serial / CAN socket gets opened or closed.
///
/// 404 when the channel id doesn't exist in `channels` table; 500
/// when the protocol-level connect fails (wrong address, timeout,
/// permission denied on serial port, etc.). Use this for operator-
/// driven device cycling; channels normally auto-reconnect via the
/// reconnect helper with exponential backoff.
#[utoipa::path(
    post,
    path = "/api/channels/{id}/control",
    params(
        ("id" = String, Path, description = "Channel identifier")
    ),
    request_body = crate::dto::ChannelOperation,
    responses(
        (status = 200, description = "Channel operation accepted", body = String,
            example = json!({
                "success": true,
                "data": "Channel 1 connected successfully"
            })
        )
    ),
    tag = "io"
)]
pub async fn control_channel(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(operation): Json<ChannelOperation>,
) -> Result<Json<SuccessResponse<String>>, AppError> {
    let channel_id = id
        .parse::<u32>()
        .map_err(|_| AppError::bad_request(format!("Invalid channel ID format: {}", id)))?;
    // Direct access without RwLock (lock-free)
    let manager = &state.channel_manager;

    // Check if channel exists and get the channel entry
    let Some(entry) = manager.get_channel(channel_id) else {
        return Err(AppError::not_found(format!(
            "Channel {} not found",
            channel_id
        )));
    };

    // Execute operation based on type using ChannelEntry's methods
    match operation.operation.as_str() {
        "start" => {
            if let Err(e) = entry.connect().await {
                tracing::error!("Ch{} connect: {}", channel_id, e);
                return Err(AppError::internal_error(format!(
                    "Failed to connect channel {}: {}",
                    channel_id, e
                )));
            }
            Ok(Json(SuccessResponse::new(format!(
                "Channel {channel_id} connected successfully"
            ))))
        },
        "stop" => {
            if let Err(e) = entry.disconnect().await {
                tracing::error!("Ch{} disconnect: {}", channel_id, e);
                return Err(AppError::internal_error(format!(
                    "Failed to disconnect channel {}: {}",
                    channel_id, e
                )));
            }
            Ok(Json(SuccessResponse::new(format!(
                "Channel {channel_id} disconnected successfully"
            ))))
        },
        "restart" => {
            // First stop the channel
            if let Err(e) = entry.disconnect().await {
                tracing::error!("Ch{} stop: {}", channel_id, e);
                return Err(AppError::internal_error(format!(
                    "Failed to stop channel {}: {}",
                    channel_id, e
                )));
            }

            // Wait a moment before starting
            tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

            // Then start it again
            if let Err(e) = entry.connect().await {
                tracing::error!("Ch{} restart: {}", channel_id, e);
                return Err(AppError::internal_error(format!(
                    "Failed to restart channel {}: {}",
                    channel_id, e
                )));
            }
            Ok(Json(SuccessResponse::new(format!(
                "Channel {channel_id} restarted successfully"
            ))))
        },
        _ => Err(AppError::bad_request(format!(
            "Invalid operation: {}",
            operation.operation
        ))),
    }
}

/// Simulation write endpoint for acquisition-owned T/S points.
///
/// Real C/A device commands are deliberately rejected here. They must enter
/// through automation's authenticated, confirmed, and audited application API,
/// then reach io through the SHM/UDS command plane.
///
/// ## Supported Point Types
/// - **T** / **Telemetry**: For testing/simulation (normally read-only)
/// - **S** / **Signal**: For testing/simulation (normally read-only)
#[utoipa::path(
    post,
    path = "/api/channels/{channel_id}/write",
    params(
        ("channel_id" = u16, Path, description = "Channel identifier", example = 1001)
    ),
    request_body = WritePointRequest,
    responses(
        (status = 200, description = "Write operation completed (single or batch)",
            body = WriteResponse),
        (status = 400, description = "Invalid point type or parameters", body = String),
        (status = 500, description = "Write operation failed", body = String)
    ),
    tag = "io"
)]
pub async fn write_channel_point(
    State(state): State<AppState>,
    Path(channel_id): Path<u32>,
    Json(request): Json<WritePointRequest>,
) -> Result<Json<SuccessResponse<crate::dto::WriteResponse>>, AppError> {
    use crate::dto::{BatchCommandError, BatchCommandResult, WritePointData, WriteResponse};

    let point_type = normalize_point_type(&request.r#type)?;
    if !point_type.is_measurement() {
        return Err(AppError::bad_request(
            "Direct C/A writes are disabled; use an instance action through aether-automation",
        ));
    }
    if !state.allow_simulation_writes {
        return Err(AppError::new(
            axum::http::StatusCode::FORBIDDEN,
            common::ErrorInfo::new(
                "Simulation writes are disabled; set AETHER_ALLOW_SIMULATION_WRITES=true only in an isolated development environment",
            )
            .with_code(403),
        ));
    }

    match &request.data {
        WritePointData::Single { id, value } => {
            let point_id = id
                .parse::<u32>()
                .map_err(|_| AppError::bad_request(format!("Invalid point ID: {}", id)))?;

            let timestamp_ms = crate::core::channels::channel_manager::unix_timestamp_ms();
            use crate::protocols::core::data::{DataBatch, DataPoint};
            let point = match point_type {
                PointType::Telemetry => DataPoint::telemetry(point_id, *value),
                PointType::Signal => DataPoint::signal(point_id, *value),
                _ => unreachable!(),
            };
            state
                .channel_manager
                .data_store()
                .write_batch(channel_id, DataBatch::from_points(vec![point]))
                .await
                .map_err(|error| {
                    AppError::internal_error(format!("Failed to write SHM point: {error}"))
                })?;

            tracing::debug!(
                "Write Ch{}:{:?}:{} = {} @{}",
                channel_id,
                point_type,
                id,
                value,
                timestamp_ms
            );

            let response = crate::dto::WritePointResponse {
                channel_id,
                point_type: point_type.as_str().to_string(),
                point_id,
                value: *value,
                timestamp_ms,
            };

            Ok(Json(SuccessResponse::new(WriteResponse::Single(response))))
        },
        WritePointData::Batch { points } => {
            let mut errors = Vec::new();
            let total = points.len();
            // Parse all IDs up front; invalid IDs go to errors and skip.
            let mut parsed: Vec<(u32, f64)> = Vec::with_capacity(total);
            for point in points {
                match point.id.parse::<u32>() {
                    Ok(id) => parsed.push((id, point.value)),
                    Err(_) => {
                        tracing::warn!("Invalid ID: Ch{}:{}:{}", channel_id, point_type, point.id);
                        errors.push(BatchCommandError {
                            point_id: 0,
                            error: format!("Invalid point ID: {}", point.id),
                        });
                    },
                }
            }

            if !parsed.is_empty() {
                use crate::protocols::core::data::{DataBatch, DataPoint};
                let points = parsed
                    .iter()
                    .map(|(point_id, value)| match point_type {
                        PointType::Telemetry => DataPoint::telemetry(*point_id, *value),
                        PointType::Signal => DataPoint::signal(*point_id, *value),
                        _ => unreachable!(),
                    })
                    .collect();
                state
                    .channel_manager
                    .data_store()
                    .write_batch(channel_id, DataBatch::from_points(points))
                    .await
                    .map_err(|error| {
                        AppError::internal_error(format!("Failed to write SHM batch: {error}"))
                    })?;
            }
            let succeeded = parsed.len();

            tracing::debug!(
                "Batch Ch{}:{:?}: {}/{} ok",
                channel_id,
                point_type,
                succeeded,
                total
            );

            let result = BatchCommandResult {
                total,
                succeeded,
                failed: total - succeeded,
                errors,
            };

            Ok(Json(SuccessResponse::new(WriteResponse::Batch(result))))
        },
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

/// Normalize point type from full name or short name to single letter
fn normalize_point_type(type_str: &str) -> Result<PointType, AppError> {
    match type_str {
        "T" | "t" | "Telemetry" | "telemetry" | "TELEMETRY" => Ok(PointType::Telemetry),
        "S" | "s" | "Signal" | "signal" | "SIGNAL" => Ok(PointType::Signal),
        "C" | "c" | "Control" | "control" | "CONTROL" => Ok(PointType::Control),
        "A" | "a" | "Adjustment" | "adjustment" | "ADJUSTMENT" => Ok(PointType::Adjustment),
        _ => Err(AppError::bad_request(format!(
            "Invalid point type '{}'. Must be one of: T/Telemetry, S/Signal, C/Control, A/Adjustment",
            type_str
        ))),
    }
}
