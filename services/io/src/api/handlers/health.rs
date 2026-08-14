//! Health Check and Service Status Handlers
//!
//! Provides endpoints for monitoring service health and operational status.

use axum::{extract::State, response::Json};
use chrono::Utc;
use common::system_metrics::SystemMetrics;
use common::{ComponentHealth, ServiceStatus as HealthServiceStatus};
use std::collections::{BTreeSet, HashMap};
use std::time::{Duration, Instant};

use crate::api::dto::{
    AppError, CommandLedgerStatus, CommandListenerStatus, DataEventIngressStatus, HealthStatus,
    ServiceStatus, SuccessResponse,
};
use crate::api::routes::{AppState, get_service_start_time};
use crate::core::channels::ChannelManager;
use crate::core::channels::command_ledger::CommandLedgerStats;
use crate::core::channels::command_outcome::CommandOutcomeStats;

const HEALTH_SQLITE_TIMEOUT: Duration = Duration::from_millis(500);
const SHM_WRITER_STALE_AFTER_MS: u64 = 30_000;

fn shm_writer_liveness(
    point_heartbeat: Option<u64>,
    health_heartbeat: Option<u64>,
    now_ms: u64,
) -> ComponentHealth {
    let mut failures = Vec::new();
    for (label, heartbeat) in [
        ("point", point_heartbeat),
        ("channel_health", health_heartbeat),
    ] {
        match heartbeat {
            None => failures.push(format!("{label} writer unavailable")),
            Some(0) => failures.push(format!("{label} heartbeat is zero")),
            Some(value) if value > now_ms => {
                failures.push(format!("{label} heartbeat is in the future"));
            },
            Some(value) if now_ms - value > SHM_WRITER_STALE_AFTER_MS => {
                failures.push(format!("{label} heartbeat is {}ms old", now_ms - value))
            },
            Some(_) => {},
        }
    }

    ComponentHealth {
        status: if failures.is_empty() {
            HealthServiceStatus::Healthy
        } else {
            HealthServiceStatus::Unhealthy
        },
        message: Some(if failures.is_empty() {
            format!(
                "point_heartbeat_ms={}, channel_health_heartbeat_ms={}",
                point_heartbeat.unwrap_or_default(),
                health_heartbeat.unwrap_or_default()
            )
        } else {
            failures.join(", ")
        }),
        duration_ms: None,
    }
}

fn command_component_health(command_stats: CommandOutcomeStats) -> ComponentHealth {
    ComponentHealth {
        status: HealthServiceStatus::Healthy,
        message: Some(format!(
            "queued={}, dropped={}, expired={}, device_succeeded={}, device_failed={}, tracked={}",
            command_stats.queued,
            command_stats.dropped,
            command_stats.expired,
            command_stats.device_succeeded,
            command_stats.device_failed,
            command_stats.tracked
        )),
        duration_ms: None,
    }
}

fn command_ledger_status_from_stats(stats: CommandLedgerStats) -> CommandLedgerStatus {
    CommandLedgerStatus {
        configured: true,
        available: true,
        total: stats.total,
        capacity: stats.capacity,
        received: stats.received,
        queued: stats.queued,
        dispatching: stats.dispatching,
        succeeded: stats.succeeded,
        failed: stats.failed,
        expired: stats.expired,
        possibly_applied: stats.possibly_applied,
        oldest_nonterminal_updated_at_ms: stats.oldest_nonterminal_updated_at_ms,
        oldest_terminal_updated_at_ms: stats.oldest_terminal_updated_at_ms,
        capacity_rejections: stats.capacity_rejections,
        cleanup_failures: stats.cleanup_failures,
        outcome_persistence_failures: stats.outcome_persistence_failures,
        outcome_persistence_pending: stats.outcome_persistence_pending,
        error: None,
    }
}

fn command_listener_status(
    stats: Option<crate::core::channels::shm_listener::ShmListenerStats>,
) -> CommandListenerStatus {
    let Some(stats) = stats else {
        return CommandListenerStatus::default();
    };
    CommandListenerStatus {
        configured: true,
        running: stats.running,
        active_connections: u64::try_from(stats.active_connections).unwrap_or(u64::MAX),
        connection_capacity: u64::try_from(stats.connection_capacity).unwrap_or(u64::MAX),
        rejected_connections: stats.rejected_connections,
        idle_timeouts: stats.idle_timeouts,
        frames_total: stats.frames_total,
        last_frame_at_ms: stats.last_frame_at_ms,
    }
}

async fn load_command_ledger_status(manager: &ChannelManager) -> CommandLedgerStatus {
    match tokio::time::timeout(HEALTH_SQLITE_TIMEOUT, manager.command_ledger_stats()).await {
        Ok(Ok(Some(stats))) => command_ledger_status_from_stats(stats),
        Ok(Ok(None)) => CommandLedgerStatus {
            error: Some("durable command ledger is not configured".to_string()),
            ..CommandLedgerStatus::default()
        },
        Ok(Err(error)) => CommandLedgerStatus {
            configured: true,
            error: Some(error.to_string()),
            ..CommandLedgerStatus::default()
        },
        Err(_) => CommandLedgerStatus {
            configured: true,
            error: Some(format!(
                "ledger query timed out after {}ms",
                HEALTH_SQLITE_TIMEOUT.as_millis()
            )),
            ..CommandLedgerStatus::default()
        },
    }
}

fn command_ledger_component_health(status: &CommandLedgerStatus) -> ComponentHealth {
    let capacity_available = status.total < status.capacity;
    let healthy = status.configured
        && status.available
        && capacity_available
        && status.outcome_persistence_pending == 0;
    ComponentHealth {
        status: if healthy {
            HealthServiceStatus::Healthy
        } else {
            HealthServiceStatus::Unhealthy
        },
        message: Some(if let Some(error) = status.error.as_deref() {
            error.to_string()
        } else {
            format!(
                "total={}/{}, received={}, queued={}, dispatching={}, succeeded={}, failed={}, expired={}, possibly_applied={}, oldest_nonterminal_updated_at_ms={:?}, oldest_terminal_updated_at_ms={:?}, capacity_rejections={}, cleanup_failures={}, outcome_persistence_failures={}, outcome_persistence_pending={}",
                status.total,
                status.capacity,
                status.received,
                status.queued,
                status.dispatching,
                status.succeeded,
                status.failed,
                status.expired,
                status.possibly_applied,
                status.oldest_nonterminal_updated_at_ms,
                status.oldest_terminal_updated_at_ms,
                status.capacity_rejections,
                status.cleanup_failures,
                status.outcome_persistence_failures,
                status.outcome_persistence_pending,
            )
        }),
        duration_ms: None,
    }
}

fn file_log_component_health(
    stats: Option<crate::protocols::core::file_logging::FileLogStats>,
) -> ComponentHealth {
    match stats {
        Some(stats) => ComponentHealth {
            status: if stats.worker_running && stats.io_healthy && !stats.io_stalled {
                HealthServiceStatus::Healthy
            } else {
                HealthServiceStatus::Unhealthy
            },
            message: Some(format!(
                "accepted={}, dropped={}, oversized={}, write_failures={}, flush_failures={}, shutdown_timeouts={}, pending={}, worker_running={}, io_healthy={}, io_stalled={}",
                stats.accepted,
                stats.dropped,
                stats.oversized,
                stats.write_failures,
                stats.flush_failures,
                stats.shutdown_timeouts,
                stats.pending,
                stats.worker_running,
                stats.io_healthy,
                stats.io_stalled
            )),
            duration_ms: None,
        },
        None => ComponentHealth {
            status: HealthServiceStatus::Healthy,
            message: Some("disabled (no channel has file logging enabled)".to_string()),
            duration_ms: None,
        },
    }
}

fn command_listener_component_health(
    stats: Option<crate::core::channels::shm_listener::ShmListenerStats>,
) -> ComponentHealth {
    match stats {
        Some(stats) => {
            let available = stats.running && stats.active_connections < stats.connection_capacity;
            ComponentHealth {
                status: if available {
                    HealthServiceStatus::Healthy
                } else {
                    HealthServiceStatus::Unhealthy
                },
                message: Some(format!(
                    "running={}, active_connections={}/{}, rejected_connections={}, idle_timeouts={}, frames_total={}, last_frame_at_ms={:?}",
                    stats.running,
                    stats.active_connections,
                    stats.connection_capacity,
                    stats.rejected_connections,
                    stats.idle_timeouts,
                    stats.frames_total,
                    stats.last_frame_at_ms,
                )),
                duration_ms: None,
            }
        },
        // Library/test compositions may intentionally omit UDS. The production
        // composition root refuses to start without it.
        None => ComponentHealth {
            status: HealthServiceStatus::Healthy,
            message: Some("not configured in this composition".to_string()),
            duration_ms: None,
        },
    }
}

fn channels_are_healthy(
    desired: Option<&BTreeSet<u32>>,
    projected: &BTreeSet<u32>,
    running_channels: usize,
) -> bool {
    desired.is_some_and(|desired| {
        desired == projected
            && (desired.is_empty() || running_channels.saturating_mul(2) >= desired.len())
    })
}

fn data_event_ingress_status(
    channel_stats: &[crate::core::channels::ChannelStats],
) -> DataEventIngressStatus {
    let mut summary = DataEventIngressStatus::default();
    for stats in channel_stats
        .iter()
        .filter_map(|channel| channel.data_event_ingress)
    {
        summary.channels = summary.channels.saturating_add(1);
        summary.accepted = summary.accepted.saturating_add(stats.accepted);
        summary.coalesced = summary.coalesced.saturating_add(stats.coalesced);
        summary.dropped_full = summary.dropped_full.saturating_add(stats.dropped_full);
        summary.dropped_closed = summary.dropped_closed.saturating_add(stats.dropped_closed);
        summary.dropped_contended = summary
            .dropped_contended
            .saturating_add(stats.dropped_contended);
        summary.oversized = summary.oversized.saturating_add(stats.oversized);
        summary.discarded_on_close = summary
            .discarded_on_close
            .saturating_add(stats.discarded_on_close);
        summary.pending = summary.pending.saturating_add(stats.pending);
        summary.high_watermark = summary.high_watermark.saturating_add(stats.high_watermark);
        summary.capacity = summary.capacity.saturating_add(stats.capacity);
        if !stats.receiver_open {
            summary.closed_channels = summary.closed_channels.saturating_add(1);
        }
        if stats.saturated {
            summary.saturated_channels = summary.saturated_channels.saturating_add(1);
        }
        if !stats.receiver_open || stats.saturated {
            summary.unhealthy_channels = summary.unhealthy_channels.saturating_add(1);
        }
    }
    summary
}

fn data_event_ingress_component_health(summary: &DataEventIngressStatus) -> ComponentHealth {
    ComponentHealth {
        status: if summary.unhealthy_channels == 0 {
            HealthServiceStatus::Healthy
        } else {
            HealthServiceStatus::Unhealthy
        },
        message: Some(format!(
            "channels={}, unhealthy={}, closed={}, saturated={}, accepted={}, coalesced={}, dropped_full={}, dropped_closed={}, dropped_contended={}, oversized={}, discarded_on_close={}, pending={}/{}, high_watermark={}",
            summary.channels,
            summary.unhealthy_channels,
            summary.closed_channels,
            summary.saturated_channels,
            summary.accepted,
            summary.coalesced,
            summary.dropped_full,
            summary.dropped_closed,
            summary.dropped_contended,
            summary.oversized,
            summary.discarded_on_close,
            summary.pending,
            summary.capacity,
            summary.high_watermark,
        )),
        duration_ms: None,
    }
}

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
        (status = 200, description = "Service status retrieved", body = crate::api::dto::ServiceStatus)
    ),
    tag = "io"
)]
pub async fn get_service_status(
    State(state): State<AppState>,
) -> Result<Json<SuccessResponse<ServiceStatus>>, AppError> {
    // Direct access without RwLock (lock-free)
    let manager = &state.channel_manager;
    let total_channels = manager.channel_count();
    let active_channels = manager.running_channel_count();
    let data_event_ingress = data_event_ingress_status(&manager.get_all_channel_stats());
    let command_ledger = load_command_ledger_status(manager).await;
    let command_listener =
        command_listener_status(manager.shm_listener().map(|value| value.stats()));

    // Get actual service start time and calculate uptime
    let start_time = get_service_start_time();
    let uptime_duration = Utc::now() - start_time;
    let uptime_seconds = uptime_duration.num_seconds().max(0) as u64;

    let status = ServiceStatus {
        name: "Aether I/O Service".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        uptime: uptime_seconds,
        start_time,
        channels: u32::try_from(total_channels).unwrap_or(u32::MAX),
        active_channels: u32::try_from(active_channels).unwrap_or(u32::MAX),
        data_event_ingress,
        command_ledger,
        command_listener,
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
    let sqlite_result = tokio::time::timeout(
        HEALTH_SQLITE_TIMEOUT,
        sqlx::query_scalar::<_, i64>(
            "SELECT channel_id FROM channels WHERE enabled = 1 ORDER BY channel_id",
        )
        .fetch_all(&state.sqlite_pool),
    )
    .await;
    let sqlite_duration = sqlite_start.elapsed().as_millis() as u64;
    let mut desired_enabled_channels = None;

    match sqlite_result {
        Ok(Ok(enabled_ids)) => {
            match enabled_ids
                .into_iter()
                .map(u32::try_from)
                .collect::<Result<BTreeSet<_>, _>>()
            {
                Ok(enabled_ids) => {
                    let enabled_count = enabled_ids.len();
                    desired_enabled_channels = Some(enabled_ids);
                    checks.insert(
                        "sqlite".to_string(),
                        ComponentHealth {
                            status: HealthServiceStatus::Healthy,
                            message: Some(format!("Connected; enabled_channels={enabled_count}")),
                            duration_ms: Some(sqlite_duration),
                        },
                    );
                },
                Err(error) => {
                    overall_healthy = false;
                    checks.insert(
                        "sqlite".to_string(),
                        ComponentHealth {
                            status: HealthServiceStatus::Unhealthy,
                            message: Some(format!("Invalid enabled channel identity: {error}")),
                            duration_ms: Some(sqlite_duration),
                        },
                    );
                },
            }
        },
        Ok(Err(e)) => {
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
        Err(_) => {
            overall_healthy = false;
            checks.insert(
                "sqlite".to_string(),
                ComponentHealth {
                    status: HealthServiceStatus::Unhealthy,
                    message: Some(format!(
                        "Query timed out after {}ms",
                        HEALTH_SQLITE_TIMEOUT.as_millis()
                    )),
                    duration_ms: Some(sqlite_duration),
                },
            );
        },
    }

    // Get channel manager stats
    // Direct access without RwLock (lock-free)
    let manager = &state.channel_manager;
    let projected_channels: BTreeSet<_> = manager.get_channel_ids().into_iter().collect();
    let total_channels = projected_channels.len();
    let running_channels = manager.running_channel_count();

    // Desired state is SQLite, not the in-memory projection. Counting only
    // projected channels made 0/0 look healthy when every enabled channel had
    // failed activation. Projection identity must match authority; the
    // established availability threshold then applies to device connectivity.
    let channels_healthy = channels_are_healthy(
        desired_enabled_channels.as_ref(),
        &projected_channels,
        running_channels,
    );
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
            message: Some(format!(
                "running={running_channels}, projected={total_channels}, desired_enabled={}",
                desired_enabled_channels
                    .as_ref()
                    .map(|channels| channels.len().to_string())
                    .unwrap_or_else(|| "unavailable".to_string())
            )),
            duration_ms: None,
        },
    );

    // Watchdog check: report failed and stuck channel counts
    let all_stats = manager.get_all_channel_stats();
    let data_event_ingress = data_event_ingress_status(&all_stats);
    let data_event_ingress_health = data_event_ingress_component_health(&data_event_ingress);
    if matches!(
        data_event_ingress_health.status,
        HealthServiceStatus::Unhealthy
    ) {
        overall_healthy = false;
    }
    checks.insert("data_event_ingress".to_string(), data_event_ingress_health);
    let now_ms = crate::core::channels::channel_entry::monotonic_timestamp_ms();
    let stuck_timeout_ms: i64 = 120 * 1000;

    let failed_count = all_stats.iter().filter(|s| s.reconnect_failed).count();
    let stuck_count = all_stats
        .iter()
        .filter(|s| {
            s.watchdog_progress_tick_ms > 0
                && now_ms.saturating_sub(s.watchdog_progress_tick_ms) > stuck_timeout_ms
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

    // CommandReceipt acknowledges local producer acceptance. These counters
    // expose the subsequent in-process lifecycle without changing that wire
    // contract or introducing automatic retries for device commands. They are
    // cumulative process telemetry, not current liveness: one historical drop
    // must not leave readiness permanently degraded until restart.
    let command_stats = manager.command_outcome_stats();
    checks.insert(
        "commands".to_string(),
        command_component_health(command_stats),
    );

    let command_ledger_status = load_command_ledger_status(manager).await;
    let command_ledger = command_ledger_component_health(&command_ledger_status);
    if matches!(command_ledger.status, HealthServiceStatus::Unhealthy) {
        overall_healthy = false;
    }
    checks.insert("command_ledger".to_string(), command_ledger);

    let file_logging = file_log_component_health(manager.file_log_stats());
    if matches!(file_logging.status, HealthServiceStatus::Unhealthy) {
        overall_healthy = false;
    }
    checks.insert("file_logging".to_string(), file_logging);

    let command_listener =
        command_listener_component_health(manager.shm_listener().map(|listener| listener.stats()));
    if matches!(command_listener.status, HealthServiceStatus::Unhealthy) {
        overall_healthy = false;
    }
    checks.insert("command_listener".to_string(), command_listener);

    // SHM stats: slot occupancy + writer heartbeat. Always emit a "shm"
    // entry so operators / Docker healthcheck can distinguish "SHM
    // healthy" from "SHM never initialized" — the old `if let Some(handle)`
    // path silently dropped the entry when the handle was None and
    // returned 200 alongside SQLite health, masking a degraded SHM.
    let point_heartbeat = manager
        .shm_handle()
        .generation()
        .map(|layout| layout.acquisition_writer().writer_heartbeat());
    let health_heartbeat = manager
        .channel_health_writer()
        .and_then(|writer| writer.writer_heartbeat());
    let shm = shm_writer_liveness(
        point_heartbeat,
        health_heartbeat,
        aether_shm_bridge::timestamp_ms(),
    );
    if matches!(shm.status, HealthServiceStatus::Unhealthy) {
        overall_healthy = false;
    }
    checks.insert("shm".to_string(), shm);

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

#[cfg(test)]
mod tests {
    use super::*;

    fn channel_with_ingress(
        ingress: crate::protocols::core::DataEventIngressStats,
    ) -> crate::core::channels::ChannelStats {
        crate::core::channels::ChannelStats {
            channel_id: 7,
            is_connected: true,
            watchdog_progress_tick_ms: 1,
            reconnect_failed: false,
            reconnect_total_attempts: 0,
            data_event_ingress: Some(ingress),
        }
    }

    #[test]
    fn historical_ingress_drops_stay_visible_without_permanent_degradation() {
        let summary = data_event_ingress_status(&[channel_with_ingress(
            crate::protocols::core::DataEventIngressStats {
                accepted: 10,
                coalesced: 20,
                dropped_full: 3,
                dropped_closed: 2,
                oversized: 1,
                receiver_open: true,
                saturated: false,
                ..Default::default()
            },
        )]);
        let component = data_event_ingress_component_health(&summary);

        assert!(matches!(component.status, HealthServiceStatus::Healthy));
        assert!(component.message.as_deref().is_some_and(|message| {
            message.contains("coalesced=20")
                && message.contains("dropped_full=3")
                && message.contains("oversized=1")
        }));
    }

    #[test]
    fn current_closed_or_saturated_ingress_is_unhealthy() {
        for ingress in [
            crate::protocols::core::DataEventIngressStats {
                receiver_open: false,
                ..Default::default()
            },
            crate::protocols::core::DataEventIngressStats {
                receiver_open: true,
                saturated: true,
                ..Default::default()
            },
        ] {
            let summary = data_event_ingress_status(&[channel_with_ingress(ingress)]);
            assert!(matches!(
                data_event_ingress_component_health(&summary).status,
                HealthServiceStatus::Unhealthy
            ));
        }
    }

    #[test]
    fn cumulative_command_failures_do_not_permanently_degrade_readiness() {
        let component = command_component_health(CommandOutcomeStats {
            queued: 12,
            dropped: 3,
            expired: 2,
            device_succeeded: 5,
            device_failed: 2,
            tracked: 9,
        });

        assert!(matches!(component.status, HealthServiceStatus::Healthy));
        assert!(component.message.as_deref().is_some_and(
            |message| message.contains("dropped=3") && message.contains("device_failed=2")
        ));
    }

    #[test]
    fn historical_file_log_failures_remain_visible_without_permanent_degradation() {
        let component =
            file_log_component_health(Some(crate::protocols::core::file_logging::FileLogStats {
                accepted: 12,
                dropped: 3,
                oversized: 1,
                write_failures: 2,
                flush_failures: 1,
                pending: 4,
                worker_running: true,
                io_healthy: true,
                ..Default::default()
            }));

        assert!(matches!(component.status, HealthServiceStatus::Healthy));
        assert!(component.message.as_deref().is_some_and(|message| {
            message.contains("dropped=3")
                && message.contains("oversized=1")
                && message.contains("write_failures=2")
                && message.contains("pending=4")
        }));
    }

    #[test]
    fn current_file_log_failure_is_unhealthy_and_a_successful_flush_recovers() {
        let failed =
            file_log_component_health(Some(crate::protocols::core::file_logging::FileLogStats {
                worker_running: true,
                io_healthy: false,
                flush_failures: 1,
                ..Default::default()
            }));
        let recovered =
            file_log_component_health(Some(crate::protocols::core::file_logging::FileLogStats {
                worker_running: true,
                io_healthy: true,
                flush_failures: 1,
                ..Default::default()
            }));

        assert!(matches!(failed.status, HealthServiceStatus::Unhealthy));
        assert!(matches!(recovered.status, HealthServiceStatus::Healthy));
        assert!(
            recovered
                .message
                .as_deref()
                .is_some_and(|message| message.contains("flush_failures=1"))
        );

        let stalled =
            file_log_component_health(Some(crate::protocols::core::file_logging::FileLogStats {
                worker_running: true,
                io_healthy: true,
                io_stalled: true,
                ..Default::default()
            }));
        assert!(matches!(stalled.status, HealthServiceStatus::Unhealthy));
    }

    #[test]
    fn saturated_or_stopped_command_listener_is_unhealthy() {
        let saturated = command_listener_component_health(Some(
            crate::core::channels::shm_listener::ShmListenerStats {
                running: true,
                active_connections: 64,
                connection_capacity: 64,
                rejected_connections: 1,
                idle_timeouts: 0,
                frames_total: 0,
                last_frame_at_ms: None,
            },
        ));
        let stopped = command_listener_component_health(Some(
            crate::core::channels::shm_listener::ShmListenerStats {
                running: false,
                active_connections: 0,
                connection_capacity: 64,
                rejected_connections: 0,
                idle_timeouts: 0,
                frames_total: 0,
                last_frame_at_ms: None,
            },
        ));

        assert!(matches!(saturated.status, HealthServiceStatus::Unhealthy));
        assert!(matches!(stopped.status, HealthServiceStatus::Unhealthy));
    }

    #[test]
    fn equal_channel_counts_with_different_identities_are_unhealthy() {
        let desired = BTreeSet::from([7]);
        let projected = BTreeSet::from([8]);

        assert!(!channels_are_healthy(Some(&desired), &projected, 1));
    }

    #[test]
    fn stopped_file_log_worker_is_unhealthy() {
        let component =
            file_log_component_health(Some(crate::protocols::core::file_logging::FileLogStats {
                worker_running: false,
                ..Default::default()
            }));

        assert!(matches!(component.status, HealthServiceStatus::Unhealthy));
    }

    #[test]
    fn both_shm_writer_heartbeats_must_be_present_current_and_not_future() {
        let now_ms = 100_000;
        assert!(matches!(
            shm_writer_liveness(Some(now_ms - 1), Some(now_ms - 2), now_ms).status,
            HealthServiceStatus::Healthy
        ));

        for component in [
            shm_writer_liveness(None, Some(now_ms), now_ms),
            shm_writer_liveness(Some(0), Some(now_ms), now_ms),
            shm_writer_liveness(Some(now_ms + 1), Some(now_ms), now_ms),
            shm_writer_liveness(
                Some(now_ms - SHM_WRITER_STALE_AFTER_MS - 1),
                Some(now_ms),
                now_ms,
            ),
            shm_writer_liveness(Some(now_ms), None, now_ms),
            shm_writer_liveness(Some(now_ms), Some(0), now_ms),
        ] {
            assert!(matches!(component.status, HealthServiceStatus::Unhealthy));
        }
    }
}
