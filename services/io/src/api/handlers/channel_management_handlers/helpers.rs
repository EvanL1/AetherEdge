//! Shared helper functions for channel management handlers

use crate::api::routes::AppState;
use crate::core::channels::ChannelManager;
use crate::core::config::ChannelLoggingConfig;
use crate::dto::{AppError, ParameterChangeType};
use std::sync::Arc;

/// Tables related to a channel (used by migration and deletion)
pub(super) const RELATED_TABLES: [&str; 8] = [
    "telemetry_points",
    "signal_points",
    "control_points",
    "adjustment_points",
    "json_point_mappings",
    "point_mappings",
    "measurement_routing",
    "action_routing",
];

/// Parse channel config JSON into description, parameters, and logging
#[allow(clippy::type_complexity)]
pub(super) fn parse_channel_config(
    channel_id: u32,
    config_str: Option<String>,
) -> Result<
    (
        Option<String>,
        std::collections::HashMap<String, serde_json::Value>,
        ChannelLoggingConfig,
    ),
    AppError,
> {
    let config_obj = match config_str {
        None => serde_json::Map::new(),
        Some(s) => {
            let value: serde_json::Value = serde_json::from_str(&s).map_err(|e| {
                tracing::error!("Ch{} invalid config JSON: {}", channel_id, e);
                AppError::internal_error(format!(
                    "Invalid channel config JSON for {}: {}",
                    channel_id, e
                ))
            })?;
            value.as_object().cloned().ok_or_else(|| {
                tracing::error!("Ch{} config must be a JSON object", channel_id);
                AppError::internal_error(format!(
                    "Invalid channel config for {}: expected JSON object",
                    channel_id
                ))
            })?
        },
    };

    let description = match config_obj.get("description") {
        None => None,
        Some(d) => Some(
            d.as_str()
                .ok_or_else(|| {
                    tracing::error!(
                        "Ch{} config field 'description' must be a string",
                        channel_id
                    );
                    AppError::internal_error(format!(
                        "Invalid channel config for {}: 'description' must be a string",
                        channel_id
                    ))
                })?
                .to_string(),
        ),
    };

    let parameters = match config_obj.get("parameters") {
        None => std::collections::HashMap::new(),
        Some(p) => p
            .as_object()
            .ok_or_else(|| {
                tracing::error!(
                    "Ch{} config field 'parameters' must be an object",
                    channel_id
                );
                AppError::internal_error(format!(
                    "Invalid channel config for {}: 'parameters' must be an object",
                    channel_id
                ))
            })?
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect(),
    };

    let logging = match config_obj.get("logging") {
        None => ChannelLoggingConfig::default(),
        Some(l) => serde_json::from_value(l.clone()).map_err(|e| {
            tracing::error!("Ch{} invalid logging config: {}", channel_id, e);
            AppError::internal_error(format!(
                "Invalid channel logging config for {}: {}",
                channel_id, e
            ))
        })?,
    };

    Ok((description, parameters, logging))
}

/// Build channel config JSON from description, parameters, and logging
pub(super) fn build_channel_config_json(
    description: Option<&String>,
    parameters: &std::collections::HashMap<String, serde_json::Value>,
    logging: &ChannelLoggingConfig,
) -> Result<String, serde_json::Error> {
    let mut config_obj = serde_json::Map::new();

    if let Some(desc) = description {
        config_obj.insert(
            "description".to_string(),
            serde_json::Value::String(desc.clone()),
        );
    }

    let params_obj: serde_json::Map<String, serde_json::Value> = parameters
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    config_obj.insert(
        "parameters".to_string(),
        serde_json::Value::Object(params_obj),
    );

    let logging_json = serde_json::to_value(logging)?;
    config_obj.insert("logging".to_string(), logging_json);

    serde_json::to_string(&config_obj)
}

/// Load channel configuration from database, returns error if not found
pub(super) async fn load_channel_from_db(
    pool: &sqlx::SqlitePool,
    id: u32,
) -> Result<(String, String, bool, Option<String>), AppError> {
    sqlx::query_as("SELECT name, protocol, enabled, config FROM channels WHERE channel_id = ?")
        .bind(id as i64)
        .fetch_optional(pool)
        .await
        .map_err(|e| {
            tracing::error!("Load channel {}: {}", id, e);
            AppError::internal_error("Database operation failed")
        })?
        .ok_or_else(|| AppError::not_found(format!("Channel {} not found in database", id)))
}

/// Check that a channel ID is available in both runtime and database
pub(super) async fn check_channel_id_available(
    id: u32,
    pool: &sqlx::SqlitePool,
    manager: &ChannelManager,
) -> Result<(), AppError> {
    if manager.get_channel(id).is_some() {
        return Err(AppError::conflict(format!(
            "Channel {} already exists in runtime",
            id
        )));
    }
    let db_exists: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM channels WHERE channel_id = ?)")
            .bind(id as i64)
            .fetch_one(pool)
            .await
            .map_err(|e| AppError::internal_error(format!("Database error: {}", e)))?;
    if db_exists {
        return Err(AppError::conflict(format!(
            "Channel {} already exists in database",
            id
        )));
    }
    Ok(())
}

/// Analyze parameter changes to determine if reload is needed
///
/// Returns the highest severity change type:
/// - MetadataOnly: name, description - no reload needed
/// - NonCritical: timeout, retry - may need reload
/// - Critical: host, port, slave_id - must reload
pub(super) fn analyze_parameter_changes(
    old_params: &std::collections::HashMap<String, serde_json::Value>,
    new_params: &std::collections::HashMap<String, serde_json::Value>,
    name_changed: bool,
    description_changed: bool,
    protocol_changed: bool,
) -> ParameterChangeType {
    use ParameterChangeType::*;

    // Protocol change is always critical
    if protocol_changed {
        return Critical;
    }

    // Check if only metadata changed
    if !name_changed && !description_changed && old_params == new_params {
        return MetadataOnly;
    }

    // Define critical parameters (connection-related)
    let critical_params: Vec<&str> = vec![
        "host",
        "ip",
        "address",
        "server",
        "port",
        "slave_id",
        "device_id",
        "unit_id",
        "node_id",
        "baud_rate",
        "data_bits",
        "stop_bits",
        "parity",
        "serial_port",
        "device",
        "tty",
    ];

    // Define non-critical parameters (performance tuning)
    let non_critical_params: Vec<&str> = vec![
        "timeout",
        "timeout_ms",
        "connect_timeout",
        "retry",
        "max_retries",
        "retry_count",
        "poll_interval",
        "poll_rate",
        "scan_rate",
        "keepalive",
        "heartbeat",
    ];

    // Check for critical parameter changes
    for key in critical_params.iter() {
        if old_params.get(*key) != new_params.get(*key) {
            tracing::debug!("Critical change: {}", key);
            return Critical;
        }
    }

    // Check for non-critical parameter changes
    for key in non_critical_params.iter() {
        if old_params.get(*key) != new_params.get(*key) {
            tracing::debug!("Param change: {}", key);
            return NonCritical;
        }
    }

    // Check for any other parameter changes (treat as non-critical)
    let all_keys: std::collections::HashSet<_> =
        old_params.keys().chain(new_params.keys()).collect();

    for key in all_keys {
        if old_params.get(key.as_str()) != new_params.get(key.as_str()) {
            tracing::debug!("Unknown param: {}", key);
            return NonCritical;
        }
    }

    // Only metadata changed
    MetadataOnly
}

/// Perform hot reload for a running channel (async, non-blocking)
///
/// Removes the old channel, creates a new one with updated config.
/// Connection is attempted in background (non-blocking).
/// Returns Ok("reloaded") immediately after channel creation.
pub(super) async fn perform_hot_reload(
    id: u32,
    state: &AppState,
    new_config: crate::core::config::ChannelConfig,
) -> Result<String, String> {
    // Direct access without RwLock (lock-free)
    let manager = &state.channel_manager;

    // 1. Remove old channel (allow failure)
    if let Err(e) = manager.remove_channel(id).await {
        tracing::warn!("Remove Ch{}: {}", id, e);
    }

    // 2. Create new channel
    let entry = manager
        .create_channel(Arc::new(new_config))
        .await
        .map_err(|e| format!("Failed to create channel: {}", e))?;

    // 3. Async connection (don't wait) using ChannelEntry's connect method
    // Fire-and-forget: connection is best-effort, errors are logged
    tokio::spawn(async move {
        match entry.connect().await {
            Ok(_) => tracing::debug!("Ch{} connected", id),
            Err(e) => tracing::warn!("Ch{} connect: {}", id, e),
        }
    });

    tracing::debug!("Ch{} reloaded", id);
    Ok("reloaded".to_string())
}
