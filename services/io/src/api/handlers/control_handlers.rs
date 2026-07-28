//! Per-channel protocol diagnostics control.

use crate::api::routes::AppState;
use crate::dto::{AppError, SuccessResponse};
use axum::{
    extract::{Path, State},
    response::Json,
};

/// Change a channel's log verbosity at runtime, no restart needed.
///
/// Per-channel knob (overrides global `RUST_LOG`) for trace-level
/// debugging without flooding every channel's logs. Accepted levels:
/// `debug` / `verbose` (full protocol frames), `info` / `standard`
/// (default), `error` (only failures). Applies both to the protocol
/// adapter's internal logging config and the per-channel diagnostic file.
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
    let Some(entry) = state.channel_manager.get_channel(id) else {
        return Err(AppError::not_found(format!("Channel {} not found", id)));
    };

    entry
        .set_log_level(&req.level)
        .await
        .map_err(|error| AppError::bad_request(error.to_string()))?;

    Ok(Json(SuccessResponse::new(format!(
        "Channel {} log level set to {}",
        id, req.level
    ))))
}
