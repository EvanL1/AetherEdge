//! Authenticated durable command-outcome query.

use aether_application::READ_COMMAND_OUTCOME_CAPABILITY;
use aether_domain::{CommandId, TimestampMs};
use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode, header},
    response::Json,
};
use common::ErrorInfo;

use crate::api::dto::{AppError, CommandOutcomeResponse, SuccessResponse};
use crate::api::routes::AppState;

/// Query one retained command outcome by its hexadecimal or canonical UUID ID.
#[utoipa::path(
    get,
    path = "/api/commands/{command_id}/outcome",
    params(("command_id" = String, Path, description = "32 lowercase hexadecimal characters or canonical UUID")),
    responses(
        (status = 200, description = "Retained durable command outcome", body = crate::api::dto::CommandOutcomeResponse),
        (status = 400, description = "Malformed command ID"),
        (status = 403, description = "Missing/invalid Bearer token or device.read permission"),
        (status = 404, description = "Command identity is not retained"),
        (status = 503, description = "Durable command ledger is unavailable")
    ),
    security(("bearer_auth" = [])),
    tag = "io"
)]
pub async fn get_command_outcome(
    State(state): State<AppState>,
    Path(command_id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<SuccessResponse<CommandOutcomeResponse>>, AppError> {
    let command_id = parse_command_id(&command_id)?;
    let authenticator = state.access_authenticator.as_ref().ok_or_else(|| {
        AppError::service_unavailable("command outcome authentication is not configured")
    })?;
    let authorization = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok());
    let invocation = authenticator.invocation(
        authorization,
        None,
        false,
        TimestampMs::new(chrono::Utc::now().timestamp_millis().max(0) as u64),
    );
    if !invocation
        .context()
        .actor()
        .has_permission(READ_COMMAND_OUTCOME_CAPABILITY.required_permission())
    {
        return Err(AppError::new(
            StatusCode::FORBIDDEN,
            ErrorInfo::new(format!(
                "{} permission is required",
                READ_COMMAND_OUTCOME_CAPABILITY.required_permission()
            ))
            .with_code(403),
        ));
    }
    if !state.channel_manager.has_command_ledger() {
        return Err(AppError::service_unavailable(
            "durable command outcome ledger is not configured",
        ));
    }
    let record = state
        .channel_manager
        .command_ledger_record(command_id)
        .await
        .map_err(|error| AppError::service_unavailable(error.to_string()))?
        .ok_or_else(|| AppError::not_found("command outcome is not retained"))?;

    Ok(Json(SuccessResponse::new(CommandOutcomeResponse {
        command_id: format!("{:032x}", record.command_id().get()),
        channel_id: record.channel_id(),
        state: record.state().as_str().to_string(),
        received_at_ms: record.received_at().get(),
        accepted_at_ms: record.accepted_at().map(TimestampMs::get),
        updated_at_ms: record.updated_at().get(),
        expires_at_ms: record.expires_at().get(),
        diagnostic: record.diagnostic().map(str::to_string),
    })))
}

fn parse_command_id(value: &str) -> Result<CommandId, AppError> {
    if value.len() == 32
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        let value = u128::from_str_radix(value, 16)
            .map_err(|_| AppError::bad_request("command_id is invalid"))?;
        return Ok(CommandId::new(value));
    }
    let parsed = uuid::Uuid::parse_str(value).map_err(|_| {
        AppError::bad_request("command_id must be 32 lowercase hex or a canonical UUID")
    })?;
    if parsed.to_string() != value {
        return Err(AppError::bad_request(
            "command_id UUID must use canonical lowercase form",
        ));
    }
    Ok(CommandId::new(parsed.as_u128()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_id_parser_is_strict_and_lowercase() {
        assert!(parse_command_id("00000000000000000000000000000001").is_ok());
        assert!(parse_command_id("018f0000-0000-7000-8000-000000000001").is_ok());
        assert!(parse_command_id("0000000000000000000000000000000A").is_err());
        assert!(parse_command_id("018F0000-0000-7000-8000-000000000001").is_err());
        assert!(parse_command_id("1").is_err());
        assert!(parse_command_id("gggggggggggggggggggggggggggggggg").is_err());
    }
}
