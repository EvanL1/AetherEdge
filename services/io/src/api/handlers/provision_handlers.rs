#![allow(clippy::disallowed_methods)]

//! Channel auto-provision handlers.
//!
//! Generic entry point for protocol-specific point table generation.
//! Currently supports SunSpec (`sunspec_tcp` / `sunspec_rtu`).

use crate::api::handlers::point_handlers::{
    trigger_channel_reload_if_needed, validate_channel_exists,
};
use crate::api::routes::AppState;
use crate::dto::{AppError, AutoReloadQuery, SuccessResponse};
use aether_model::sunspec::{ExpandConfig, ExpandFilter, expand_model, load_model, model_exists};
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::Json,
};
use common::ErrorInfo;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

#[cfg(feature = "modbus")]
use crate::protocols::adapters::modbus_config::ModbusChannelParamsConfig;
#[cfg(feature = "modbus")]
use crate::protocols::sunspec::{connect_modbus, discover_models};
#[cfg(feature = "modbus")]
use crate::utils::{is_modbus_family, normalize_protocol_name};

#[cfg(not(feature = "modbus"))]
use crate::utils::normalize_protocol_name;

// ============================================================================
// Request / Response DTOs
// ============================================================================

fn default_slave_id() -> u8 {
    1
}

fn default_function_code() -> u8 {
    3
}

fn default_replace_existing() -> bool {
    true
}

/// Channel provision request (protocol-specific options).
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct ProvisionRequest {
    /// Provisioning strategy. Defaults to `"sunspec"` for SunSpec channels.
    #[serde(default)]
    pub strategy: Option<String>,
    /// Modbus unit/slave ID used for discovery and mappings.
    #[serde(default = "default_slave_id")]
    pub slave_id: u8,
    /// Modbus function code for discovery reads (3=holding, 4=input).
    #[serde(default = "default_function_code")]
    pub function_code: u8,
    /// Explicit SunSpec base register; auto-detect across 0/40000/50000 if omitted.
    pub base_address: Option<u16>,
    /// Limit expansion to specific model IDs; all discovered models if omitted.
    pub model_ids: Option<Vec<u16>>,
    /// Include scale-factor registers (`sunssf` type).
    #[serde(default)]
    pub include_scale_factors: bool,
    /// Include static/nameplate points (`static: "S"`).
    #[serde(default)]
    pub include_static: bool,
    /// Include optional points (`mandatory: "O"`).
    #[serde(default)]
    pub include_optional: bool,
    /// Delete existing channel points before inserting provisioned points.
    #[serde(default = "default_replace_existing")]
    pub replace_existing: bool,
    /// First point_id to assign; auto-allocated from max existing + 1 if omitted.
    pub point_id_start: Option<u32>,
}

/// Summary of a discovered SunSpec model block.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct DiscoveredModelInfo {
    pub model_id: u16,
    pub length: u16,
    pub start_register: u16,
}

/// Provision operation result.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ProvisionResult {
    pub channel_id: u32,
    pub strategy: String,
    pub base_address: u16,
    pub discovered_models: Vec<DiscoveredModelInfo>,
    pub models_expanded: Vec<u16>,
    pub points_created: usize,
    pub point_id_start: u32,
    pub point_id_end: u32,
    pub message: String,
}

// ============================================================================
// Handler
// ============================================================================

/// Auto-provision channel points from device discovery.
///
/// For SunSpec channels: connects via Modbus, discovers the model chain,
/// expands matching model JSON definitions into telemetry points + mappings,
/// writes to SQLite, and optionally reloads the channel.
///
/// @route POST /api/channels/{channel_id}/provision
#[utoipa::path(
    post,
    path = "/api/channels/{channel_id}/provision",
    params(
        ("channel_id" = u32, Path, description = "Channel identifier"),
        ("auto_reload" = bool, Query, description = "Reconcile the channel through the governed application boundary after provision (default: false)")
    ),
    request_body(
        content = ProvisionRequest,
        description = "Provision options. Strategy defaults from channel protocol.",
        examples(
            ("SunSpec defaults" = (
                summary = "Auto-discover and provision SunSpec models",
                value = json!({
                    "slave_id": 1,
                    "function_code": 3,
                    "replace_existing": true
                })
            )),
            ("SunSpec explicit base" = (
                summary = "Provision with known base address",
                value = json!({
                    "strategy": "sunspec",
                    "base_address": 40000,
                    "model_ids": [1, 701],
                    "include_scale_factors": false
                })
            ))
        )
    ),
    responses(
        (status = 200, description = "Channel provisioned", body = ProvisionResult),
        (status = 400, description = "Invalid request or unsupported strategy"),
        (status = 404, description = "Channel not found"),
        (status = 502, description = "Device discovery failed")
    ),
    tag = "io"
)]
pub async fn provision_channel_handler(
    Path(channel_id): Path<u32>,
    Query(reload_query): Query<AutoReloadQuery>,
    State(state): State<AppState>,
    Json(req): Json<ProvisionRequest>,
) -> Result<Json<SuccessResponse<ProvisionResult>>, AppError> {
    validate_channel_exists(&state.sqlite_pool, channel_id).await?;

    let row: Option<(String, String)> =
        sqlx::query_as("SELECT protocol, config FROM channels WHERE channel_id = ?")
            .bind(channel_id as i64)
            .fetch_optional(&state.sqlite_pool)
            .await
            .map_err(|e| {
                tracing::error!("Load channel for provision: {}", e);
                AppError::internal_error("Database operation failed")
            })?;

    let Some((protocol, config_json)) = row else {
        return Err(AppError::not_found(format!(
            "Channel {channel_id} not found"
        )));
    };

    let strategy = resolve_strategy(&protocol, req.strategy.as_deref())?;

    let result = match strategy.as_str() {
        "sunspec" => {
            #[cfg(feature = "modbus")]
            {
                provision_sunspec(
                    channel_id,
                    &protocol,
                    &config_json,
                    &state,
                    &req,
                    reload_query.auto_reload,
                )
                .await?
            }
            #[cfg(not(feature = "modbus"))]
            {
                let _ = (channel_id, protocol, config_json, state, req);
                return Err(AppError::bad_request(
                    "SunSpec provision requires io built with modbus feature",
                ));
            }
        },
        other => {
            return Err(AppError::bad_request(format!(
                "Unsupported provision strategy '{other}'"
            )));
        },
    };

    Ok(Json(SuccessResponse::new(result)))
}

fn resolve_strategy(protocol: &str, requested: Option<&str>) -> Result<String, AppError> {
    if let Some(s) = requested {
        return Ok(s.to_ascii_lowercase());
    }

    let normalized = normalize_protocol_name(protocol);
    if normalized == "sunspec_tcp" || normalized == "sunspec_rtu" {
        return Ok("sunspec".to_string());
    }

    Err(AppError::bad_request(format!(
        "Channel protocol '{protocol}' has no default provision strategy; specify 'strategy' in request body"
    )))
}

#[cfg(feature = "modbus")]
async fn provision_sunspec(
    channel_id: u32,
    protocol: &str,
    config_json: &str,
    state: &AppState,
    req: &ProvisionRequest,
    auto_reload: bool,
) -> Result<ProvisionResult, AppError> {
    if !is_modbus_family(protocol) {
        return Err(AppError::bad_request(format!(
            "SunSpec provision requires modbus-family protocol, got '{protocol}'"
        )));
    }

    if ![3, 4].contains(&req.function_code) {
        return Err(AppError::bad_request(
            "function_code must be 3 (holding) or 4 (input) for SunSpec discovery",
        ));
    }

    let config: serde_json::Value = serde_json::from_str(config_json)
        .map_err(|e| AppError::bad_request(format!("Invalid channel config JSON: {e}")))?;

    let params_value = config
        .get("parameters")
        .ok_or_else(|| AppError::bad_request("Channel config missing 'parameters' object"))?;

    let params: ModbusChannelParamsConfig = serde_json::from_value(params_value.clone())
        .map_err(|e| AppError::bad_request(format!("Invalid Modbus parameters: {e}")))?;

    let mut client = connect_modbus(&params, protocol)
        .await
        .map_err(|e| device_error(format!("Modbus connect failed: {e}")))?;

    let (base_address, discovered) = discover_models(
        &mut client,
        req.slave_id,
        req.function_code,
        req.base_address,
    )
    .await
    .map_err(|e| device_error(format!("SunSpec discovery failed: {e}")))?;

    let _ = client.close().await;

    let models_to_expand: Vec<_> = discovered
        .iter()
        .filter(|m| {
            req.model_ids
                .as_ref()
                .is_none_or(|ids| ids.contains(&m.model_id))
        })
        .filter(|m| model_exists(m.model_id))
        .collect();

    if models_to_expand.is_empty() {
        let ids: Vec<u16> = discovered.iter().map(|m| m.model_id).collect();
        return Err(AppError::bad_request(format!(
            "No expandable models: discovered [{ids:?}], none have embedded JSON definitions"
        )));
    }

    let filter = ExpandFilter {
        include_static: req.include_static,
        include_scale_factors: req.include_scale_factors,
        include_optional: req.include_optional,
    };

    let mut expanded = Vec::new();
    let mut models_expanded = Vec::new();

    for block in models_to_expand {
        let model = load_model(block.model_id)
            .map_err(|e| AppError::internal_error(format!("Load model {}: {e}", block.model_id)))?;

        let points = expand_model(
            &model,
            &ExpandConfig {
                model_id: block.model_id,
                start_register: block.start_register,
                slave_id: req.slave_id,
                function_code: req.function_code,
                filter,
            },
        );

        if !points.is_empty() {
            models_expanded.push(block.model_id);
            expanded.extend(points);
        }
    }

    if expanded.is_empty() {
        return Err(AppError::bad_request(
            "Discovery succeeded but filter produced zero mappable points",
        ));
    }

    let point_id_start = match req.point_id_start {
        Some(id) => id,
        None => next_point_id(&state.sqlite_pool, channel_id).await?,
    };

    let mut tx = state
        .sqlite_pool
        .begin()
        .await
        .map_err(|e| AppError::internal_error(format!("Transaction start: {e}")))?;

    if req.replace_existing {
        for table in [
            "telemetry_points",
            "signal_points",
            "control_points",
            "adjustment_points",
        ] {
            sqlx::query(&format!("DELETE FROM {table} WHERE channel_id = ?"))
                .bind(channel_id as i64)
                .execute(&mut *tx)
                .await
                .map_err(|e| AppError::internal_error(format!("Clear {table}: {e}")))?;
        }
    }

    let mut point_id = point_id_start;
    for point in &expanded {
        sqlx::query(
            "INSERT INTO telemetry_points \
             (channel_id, point_id, signal_name, scale, offset, unit, data_type, reverse, description, protocol_mappings) \
             VALUES (?, ?, ?, ?, ?, ?, ?, 0, ?, ?)",
        )
        .bind(channel_id as i64)
        .bind(point_id as i64)
        .bind(&point.signal_name)
        .bind(point.scale)
        .bind(point.offset)
        .bind(&point.unit)
        .bind(&point.data_type)
        .bind(&point.description)
        .bind(&point.protocol_mappings)
        .execute(&mut *tx)
        .await
        .map_err(|e| AppError::internal_error(format!("Insert point {point_id}: {e}")))?;

        point_id = point_id.saturating_add(1);
    }

    tx.commit()
        .await
        .map_err(|e| AppError::internal_error(format!("Transaction commit: {e}")))?;

    let point_id_end = point_id.saturating_sub(1);
    let points_created = expanded.len();

    tracing::info!(
        "Ch{} provisioned {} SunSpec points (base={}, models={:?})",
        channel_id,
        points_created,
        base_address,
        models_expanded
    );

    trigger_channel_reload_if_needed(channel_id, state, auto_reload).await;

    let discovered_models = discovered
        .into_iter()
        .map(|m| DiscoveredModelInfo {
            model_id: m.model_id,
            length: m.length,
            start_register: m.start_register,
        })
        .collect();

    Ok(ProvisionResult {
        channel_id,
        strategy: "sunspec".to_string(),
        base_address,
        discovered_models,
        models_expanded,
        points_created,
        point_id_start,
        point_id_end,
        message: format!("Provisioned {points_created} telemetry points from SunSpec discovery"),
    })
}

#[cfg(feature = "modbus")]
async fn next_point_id(pool: &sqlx::SqlitePool, channel_id: u32) -> Result<u32, AppError> {
    let row: (i64,) = sqlx::query_as(
        "SELECT COALESCE(MAX(point_id), 0) FROM ( \
            SELECT point_id FROM telemetry_points WHERE channel_id = ? \
            UNION ALL SELECT point_id FROM signal_points WHERE channel_id = ? \
            UNION ALL SELECT point_id FROM control_points WHERE channel_id = ? \
            UNION ALL SELECT point_id FROM adjustment_points WHERE channel_id = ? \
         )",
    )
    .bind(channel_id as i64)
    .bind(channel_id as i64)
    .bind(channel_id as i64)
    .bind(channel_id as i64)
    .fetch_one(pool)
    .await
    .map_err(|e| AppError::internal_error(format!("Query max point_id: {e}")))?;

    Ok((row.0 as u32).saturating_add(1))
}

fn device_error(message: impl Into<String>) -> AppError {
    AppError::new(
        StatusCode::BAD_GATEWAY,
        ErrorInfo::new(message).with_code(502),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_strategy_sunspec_default() {
        assert_eq!(resolve_strategy("sunspec_tcp", None).unwrap(), "sunspec");
        assert_eq!(resolve_strategy("SUNSPEC_RTU", None).unwrap(), "sunspec");
    }

    #[test]
    fn resolve_strategy_explicit() {
        assert_eq!(
            resolve_strategy("modbus_tcp", Some("sunspec")).unwrap(),
            "sunspec"
        );
    }

    #[test]
    fn resolve_strategy_missing_for_modbus() {
        assert!(resolve_strategy("modbus_tcp", None).is_err());
    }
}
