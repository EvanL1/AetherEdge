//! Channel lifecycle handlers (enable/disable and delete)

use super::helpers::*;
use crate::api::routes::AppState;
use crate::core::config::ChannelCore;
use crate::dto::{AppError, SuccessResponse};
use axum::{
    extract::{Path, State},
    response::Json,
};
use std::sync::Arc;

/// Set channel enabled state
///
/// Enable or disable a channel, controlling its runtime lifecycle.
/// This is a higher-level operation than update - it manages whether the channel should run.
#[utoipa::path(
    put,
    path = "/api/channels/{id}/enabled",
    params(
        ("id" = u32, Path, description = "Channel identifier")
    ),
    request_body = crate::dto::ChannelEnabledRequest,
    responses(
        (status = 200, description = "Channel enabled state updated", body = crate::dto::ChannelCrudResult)
    ),
    tag = "io"
)]
pub async fn set_channel_enabled_handler(
    Path(id): Path<u32>,
    State(state): State<AppState>,
    Json(req): Json<crate::dto::ChannelEnabledRequest>,
) -> Result<Json<SuccessResponse<crate::dto::ChannelCrudResult>>, AppError> {
    use crate::core::config::ChannelConfig;

    tracing::debug!("Ch{} enabled={}", id, req.enabled);

    // 1. Load current configuration from database
    let (name, protocol, current_enabled, config_str) =
        load_channel_from_db(&state.sqlite_pool, id).await?;

    // 2. Parse config for runtime (before early return so we can populate description correctly)
    let (description, parameters, logging) = parse_channel_config(id, config_str)?;

    // 3. Check if state actually changed
    if current_enabled == req.enabled {
        // State unchanged - enabled is a configuration state independent of connection
        return Ok(Json(SuccessResponse::new(crate::dto::ChannelCrudResult {
            core: ChannelCore {
                id,
                name,
                description, // propagate existing description
                protocol,
                enabled: req.enabled,
            },
            runtime_status: if req.enabled {
                "enabled".to_string()
            } else {
                "disabled".to_string()
            },
            message: Some(format!(
                "Channel already {}",
                if req.enabled { "enabled" } else { "disabled" }
            )),
        })));
    }

    // 4. Execute enable or disable
    let runtime_status = if req.enabled {
        // Enable: create and start channel
        let config = ChannelConfig {
            core: ChannelCore {
                id,
                name: name.clone(),
                description: description.clone(),
                protocol: protocol.clone(),
                enabled: true,
            },
            parameters,
            logging,
        };

        // Direct access without RwLock (lock-free)
        let manager = &state.channel_manager;
        match manager.create_channel(Arc::new(config)).await {
            Ok(entry) => {
                // Trigger asynchronous connection in background
                // Fire-and-forget: connection is best-effort, errors are logged
                let channel_id_for_log = id;
                tokio::spawn(async move {
                    match entry.connect().await {
                        Ok(_) => tracing::debug!("Ch{} connected", channel_id_for_log),
                        Err(e) => tracing::warn!("Ch{} connect: {}", channel_id_for_log, e),
                    }
                });

                // Update database
                if let Err(e) = sqlx::query("UPDATE channels SET enabled = ? WHERE channel_id = ?")
                    .bind(true)
                    .bind(id as i64)
                    .execute(&state.sqlite_pool)
                    .await
                {
                    tracing::error!("Ch{} DB update: {}", id, e);
                    // DB update failed - remove the runtime channel
                    // Direct access without RwLock (lock-free)
                    let manager = &state.channel_manager;
                    let _ = manager.remove_channel(id).await;
                    return Err(AppError::internal_error(format!(
                        "Database update failed: {}",
                        e
                    )));
                }

                tracing::debug!("Ch{} enabled (bg connect)", id);
                "connecting".to_string()
            },
            Err(e) => {
                tracing::warn!("Ch{} runtime create: {}", id, e);

                // Update database to enabled even if runtime creation failed
                if let Err(e) = sqlx::query("UPDATE channels SET enabled = ? WHERE channel_id = ?")
                    .bind(true)
                    .bind(id as i64)
                    .execute(&state.sqlite_pool)
                    .await
                {
                    tracing::error!("Ch{} DB update: {}", id, e);
                    return Err(AppError::internal_error(format!(
                        "Database update failed: {}",
                        e
                    )));
                }

                tracing::debug!("Ch{} enabled (no runtime)", id);
                "enabled".to_string()
            },
        }
    } else {
        // Disable: stop and remove channel
        // Direct access without RwLock (lock-free)
        let manager = &state.channel_manager;
        if let Err(e) = manager.remove_channel(id).await {
            tracing::warn!("Ch{} remove: {}", id, e);
        }

        // Update database
        if let Err(e) = sqlx::query("UPDATE channels SET enabled = ? WHERE channel_id = ?")
            .bind(false)
            .bind(id as i64)
            .execute(&state.sqlite_pool)
            .await
        {
            tracing::error!("Ch{} DB update: {}", id, e);
            return Err(AppError::internal_error(format!(
                "Database update failed: {}",
                e
            )));
        }

        tracing::debug!("Ch{} disabled", id);
        "stopped".to_string()
    };

    let result = crate::dto::ChannelCrudResult {
        core: ChannelCore {
            id,
            name,
            description, // propagate existing description
            protocol,
            enabled: req.enabled,
        },
        runtime_status,
        message: Some(format!(
            "Channel {} successfully",
            if req.enabled { "enabled" } else { "disabled" }
        )),
    };

    Ok(Json(SuccessResponse::new(result)))
}

/// Delete a channel and hot-stop it immediately (no io restart required).
///
/// Sequence: disconnect the protocol adapter (closes TCP / releases serial port),
/// deregister from the channel manager, then delete the channel, its associated points,
/// and routing entries from SQLite. **SHM layout shrinks**, triggering a `routing_hash`
/// change and an SHM rebuild on the automation side.
/// **Destructive**: all routing entries that reference this channel are cascade-deleted.
#[utoipa::path(
    delete,
    path = "/api/channels/{id}",
    params(
        ("id" = u32, Path, description = "Channel identifier")
    ),
    responses(
        (status = 200, description = "Channel deleted", body = String)
    ),
    tag = "io"
)]
pub async fn delete_channel_handler(
    Path(id): Path<u32>,
    State(state): State<AppState>,
) -> Result<Json<SuccessResponse<String>>, AppError> {
    tracing::debug!("Deleting Ch{}", id);

    // 1. Begin transaction for atomic deletion
    let mut tx = state.sqlite_pool.begin().await.map_err(|e| {
        tracing::error!("Ch{} delete tx begin: {}", id, e);
        AppError::internal_error(format!("Failed to begin transaction: {}", e))
    })?;

    // 2. Delete related records from dependent tables (foreign key constraints)
    for table in &RELATED_TABLES {
        let query = format!("DELETE FROM {} WHERE channel_id = ?", table);
        let result = sqlx::query(&query).bind(id as i64).execute(&mut *tx).await;

        match result {
            Ok(r) => {
                if r.rows_affected() > 0 {
                    tracing::debug!("Ch{} deleted {} rows from {}", id, r.rows_affected(), table);
                }
            },
            Err(e) => {
                // Table may not exist, log and continue
                tracing::debug!("Table {} delete skipped: {}", table, e);
            },
        }
    }

    // 3. Delete channel from database
    let result = sqlx::query("DELETE FROM channels WHERE channel_id = ?")
        .bind(id as i64)
        .execute(&mut *tx)
        .await
        .map_err(|e| {
            tracing::error!("Ch{} delete: {}", id, e);
            AppError::internal_error(format!("Failed to delete channel from database: {}", e))
        })?;

    if result.rows_affected() == 0 {
        if let Err(rb_err) = tx.rollback().await {
            tracing::error!("Ch{} delete rollback failed: {}", id, rb_err);
        }
        return Err(AppError::not_found(format!(
            "Channel {} not found in database",
            id
        )));
    }

    // 4. Commit transaction BEFORE removing runtime channel
    // This ensures database consistency is preserved
    tx.commit().await.map_err(|e| {
        tracing::error!("Ch{} delete tx commit: {}", id, e);
        AppError::internal_error(format!("Failed to commit transaction: {}", e))
    })?;

    // 5. Remove from runtime (best effort - doesn't affect data consistency)
    // Even if this fails, the channel is gone from database which is the source of truth
    {
        // Direct access without RwLock (lock-free)
        let manager = &state.channel_manager;
        if let Err(e) = manager.remove_channel(id).await {
            tracing::warn!("Ch{} runtime remove: {}", id, e);
        }
    }

    tracing::info!("Ch{} deleted", id);
    Ok(Json(SuccessResponse::new(format!(
        "Channel {} deleted successfully",
        id
    ))))
}
