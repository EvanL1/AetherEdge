//! Health Check and Service Status Handlers
//!
//! Provides endpoints for monitoring service health and operational status.

use axum::{extract::State, response::Json};
use chrono::Utc;
use common::system_metrics::SystemMetrics;
use common::{ComponentHealth, ServiceStatus as HealthServiceStatus};
use std::collections::HashMap;
use std::time::Instant;

use crate::api::routes::{AppState, get_service_start_time};
use crate::dto::{AppError, HealthStatus, ServiceStatus, SuccessResponse};

/// io runtime summary: total channels, active channels, uptime, and version.
///
/// Does not perform dependency checks (no SHM / SQLite probe) — reads only the
/// in-memory channel manager state. Use this to display "how long io has been
/// running / how many channels it manages" on the dashboard. For actual health checks
/// use `/health`, which returns 503 on failure.
#[utoipa::path(
    get,
    path = "/api/status",
    responses(
        (status = 200, description = "Service status retrieved", body = crate::dto::ServiceStatus)
    ),
    tag = "io"
)]
pub async fn get_service_status(
    State(state): State<AppState>,
) -> Result<Json<SuccessResponse<ServiceStatus>>, AppError> {
    // Direct access without RwLock (lock-free)
    let manager = &state.channel_manager;
    let total_channels = manager.channel_count();
    let active_channels = manager.running_channel_count().await;

    // Get actual service start time and calculate uptime
    let start_time = get_service_start_time();
    let uptime_duration = Utc::now() - start_time;
    let uptime_seconds = uptime_duration.num_seconds().max(0) as u64;

    let status = ServiceStatus {
        name: "Communication Service".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        uptime: uptime_seconds,
        start_time,
        channels: u32::try_from(total_channels).unwrap_or(u32::MAX),
        active_channels: u32::try_from(active_channels).unwrap_or(u32::MAX),
    };

    Ok(Json(SuccessResponse::new(status)))
}

/// Health check endpoint
///
/// Performs actual availability checks on SHM and SQLite dependencies.
/// Returns 503 if any critical dependency is unhealthy.
#[utoipa::path(
    get,
    path = "/health",
    responses(
        (status = 200, description = "Service is healthy"),
        (status = 503, description = "Service is unhealthy")
    ),
    tag = "io"
)]
pub async fn health_check(
    State(state): State<AppState>,
) -> Result<Json<SuccessResponse<HealthStatus>>, AppError> {
    // Get actual uptime from service start time
    let start_time = get_service_start_time();
    let uptime_duration = Utc::now() - start_time;
    let uptime_seconds: u64 = uptime_duration.num_seconds().max(0).try_into().unwrap_or(0);

    let mut checks = HashMap::new();
    let mut overall_healthy = true;

    // Check SQLite connectivity
    let sqlite_start = Instant::now();
    let sqlite_result: Result<(i32,), _> = sqlx::query_as("SELECT 1")
        .fetch_one(&state.sqlite_pool)
        .await;
    let sqlite_duration = sqlite_start.elapsed().as_millis() as u64;

    match sqlite_result {
        Ok(_) => {
            checks.insert(
                "sqlite".to_string(),
                ComponentHealth {
                    status: HealthServiceStatus::Healthy,
                    message: Some("Connected".to_string()),
                    duration_ms: Some(sqlite_duration),
                },
            );
        },
        Err(e) => {
            overall_healthy = false;
            checks.insert(
                "sqlite".to_string(),
                ComponentHealth {
                    status: HealthServiceStatus::Unhealthy,
                    message: Some(format!("Query failed: {}", e)),
                    duration_ms: Some(sqlite_duration),
                },
            );
        },
    }

    // Get channel manager stats
    // Direct access without RwLock (lock-free)
    let manager = &state.channel_manager;
    let total_channels = manager.channel_count();
    let running_channels = manager.running_channel_count().await;

    // Fix: channels check should report Unhealthy when < 50% are running
    let channels_healthy = total_channels == 0 || running_channels * 2 >= total_channels;
    if !channels_healthy {
        overall_healthy = false;
    }

    checks.insert(
        "channels".to_string(),
        ComponentHealth {
            status: if channels_healthy {
                HealthServiceStatus::Healthy
            } else {
                HealthServiceStatus::Unhealthy
            },
            message: Some(format!("{}/{} running", running_channels, total_channels)),
            duration_ms: None,
        },
    );

    // Watchdog check: report failed and stuck channel counts
    let all_stats = manager.get_all_channel_stats().await;
    let now_ms = crate::core::channels::channel_entry::unix_timestamp_ms();
    let stuck_timeout_ms: i64 = 120 * 1000;

    let failed_count = all_stats.iter().filter(|s| s.reconnect_failed).count();
    let stuck_count = all_stats
        .iter()
        .filter(|s| {
            s.watchdog_heartbeat_ms > 0 && (now_ms - s.watchdog_heartbeat_ms) > stuck_timeout_ms
        })
        .count();
    let total_reconnects: u64 = all_stats.iter().map(|s| s.reconnect_total_attempts).sum();

    let watchdog_healthy = stuck_count == 0;
    if !watchdog_healthy {
        overall_healthy = false;
    }

    checks.insert(
        "watchdog".to_string(),
        ComponentHealth {
            status: if watchdog_healthy {
                HealthServiceStatus::Healthy
            } else {
                HealthServiceStatus::Unhealthy
            },
            message: Some(format!(
                "failed={}, stuck={}, total_reconnects={}",
                failed_count, stuck_count, total_reconnects
            )),
            duration_ms: None,
        },
    );

    // SHM stats: slot occupancy + writer heartbeat. Always emit a "shm"
    // entry so operators / Docker healthcheck can distinguish "SHM
    // healthy" from "SHM never initialized" — the old `if let Some(handle)`
    // path silently dropped the entry when the handle was None and
    // returned 200 alongside SQLite health, masking a degraded SHM.
    match manager.shm_handle().layout() {
        Some(layout_guard) => {
            let mut parts = Vec::new();

            if let Some(layout) = layout_guard.as_ref() {
                parts.push(format!("total={}", layout.writer.slot_count()));
                parts.push(format!("heartbeat_ms={}", layout.writer.writer_heartbeat()));
            }

            if let Some(stats) = manager.slot_bitmap_stats() {
                parts.push(format!("allocated={}", stats.allocated_slots));
                parts.push(format!("free={}", stats.free_slots));
            }

            checks.insert(
                "shm".to_string(),
                ComponentHealth {
                    status: HealthServiceStatus::Healthy,
                    message: Some(if parts.is_empty() {
                        "unavailable".to_string()
                    } else {
                        parts.join(", ")
                    }),
                    duration_ms: None,
                },
            );
        },
        None => {
            // SHM never initialized — the writer thread either hasn't
            // started yet or failed to create the mmap. Report Degraded
            // AND flip overall_healthy so the response gets 503 instead
            // of 200; otherwise Docker readiness probes (which only see
            // the HTTP status code) would treat a node running without
            // its hot data path as fully healthy.
            checks.insert(
                "shm".to_string(),
                ComponentHealth {
                    status: HealthServiceStatus::Degraded,
                    message: Some(
                        "not initialized (writer not started or mmap failed)".to_string(),
                    ),
                    duration_ms: None,
                },
            );
            overall_healthy = false;
        },
    }

    // Collect system metrics (CPU, memory)
    let metrics = SystemMetrics::collect();

    let overall_status = if overall_healthy {
        HealthServiceStatus::Healthy
    } else {
        HealthServiceStatus::Unhealthy
    };

    // Build error message before moving checks into health struct.
    // Include both Unhealthy and Degraded components so the 503 body
    // explains the SHM-missing case rather than being empty.
    let error_msg = if !overall_healthy {
        Some(format!(
            "Service dependencies are unhealthy: {}",
            checks
                .iter()
                .filter(|(_, c)| matches!(
                    c.status,
                    HealthServiceStatus::Unhealthy | HealthServiceStatus::Degraded
                ))
                .map(|(k, c)| format!("{}: {}", k, c.message.as_deref().unwrap_or("unknown")))
                .collect::<Vec<_>>()
                .join(", ")
        ))
    } else {
        None
    };

    let health = HealthStatus {
        status: overall_status,
        service: "aether-io".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        uptime_seconds,
        timestamp: Utc::now(),
        checks,
        system: Some(serde_json::to_value(&metrics).unwrap_or_default()),
    };

    // Return 503 if unhealthy
    if let Some(msg) = error_msg {
        return Err(AppError::service_unavailable(msg));
    }

    Ok(Json(SuccessResponse::new(health)))
}
