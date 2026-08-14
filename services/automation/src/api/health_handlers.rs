//! Health Check API Handlers
//!
//! Provides health check endpoint for automation service monitoring.

#![allow(clippy::disallowed_methods)] // json! macro used in multiple functions

use axum::{extract::State, response::Json};
use common::system_metrics::SystemMetrics;
use common::{AppError, SuccessResponse};
use serde_json::json;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::Semaphore;

const SQLITE_HEALTH_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(500);
const IO_TOPOLOGY_HEALTH_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(500);

use crate::app_state::AppState;

enum BlockingProbe<T> {
    Completed(Result<T, tokio::task::JoinError>),
    Busy,
    TimedOut,
}

async fn run_singleflight_blocking_probe<T, F>(
    gate: Arc<Semaphore>,
    deadline: std::time::Duration,
    probe: F,
) -> BlockingProbe<T>
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    let Ok(permit) = gate.try_acquire_owned() else {
        return BlockingProbe::Busy;
    };
    let mut task = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        probe()
    });
    match tokio::time::timeout(deadline, &mut task).await {
        Ok(result) => BlockingProbe::Completed(result),
        Err(_) => BlockingProbe::TimedOut,
    }
}

fn command_dispatch_health(
    writer_available: bool,
    notifier_configured: bool,
    notifier_connected: bool,
) -> (&'static str, Option<&'static str>) {
    if writer_available && notifier_connected {
        ("ready", None)
    } else if writer_available && notifier_configured {
        ("writer_only", Some("command notifier disconnected"))
    } else if writer_available {
        ("writer_only", Some("command notifier not configured"))
    } else {
        (
            "degraded",
            Some("writer unavailable (io may have restarted)"),
        )
    }
}

/// Health check endpoint
///
/// Performs actual connectivity checks on dependencies.
/// Returns 503 if any critical dependency is unhealthy.
///
#[cfg_attr(feature = "openapi", utoipa::path(
    get,
    path = "/health",
    responses(
        (status = 200, description = "Automation service and critical dependencies are healthy", body = serde_json::Value),
        (status = 503, description = "A critical dependency is unavailable", body = serde_json::Value)
    ),
    tag = "automation"
))]
pub async fn health_check(
    State(state): State<Arc<AppState>>,
) -> Result<Json<SuccessResponse<serde_json::Value>>, AppError> {
    let mut checks = serde_json::Map::new();
    let mut overall_healthy = true;
    let mut errors = Vec::new();

    // One bounded instance query proves both SQLite connectivity and the
    // commissioned-instance read path without exposing the pool to HTTP.
    let sqlite_start = Instant::now();
    let sqlite_probe = tokio::time::timeout(
        SQLITE_HEALTH_TIMEOUT,
        state.instance_manager.list_instances_paginated(None, 1, 1),
    )
    .await;
    let sqlite_status = match sqlite_probe {
        Err(_) => {
            overall_healthy = false;
            let err_msg = format!(
                "Instance query exceeded {} ms",
                SQLITE_HEALTH_TIMEOUT.as_millis()
            );
            errors.push(format!("sqlite: {err_msg}"));
            checks.insert(
                "sqlite".to_string(),
                json!({
                    "status": "unhealthy",
                    "message": err_msg,
                    "duration_ms": sqlite_start.elapsed().as_millis()
                }),
            );
            "timeout"
        },
        Ok(Ok((count, _))) => {
            let duration_ms = sqlite_start.elapsed().as_millis();
            checks.insert(
                "sqlite".to_string(),
                json!({
                    "status": "healthy",
                    "message": "Connected",
                    "duration_ms": duration_ms
                }),
            );
            checks.insert(
                "instances".to_string(),
                json!({
                    "status": "healthy",
                    "count": count,
                    "duration_ms": duration_ms
                }),
            );
            "connected"
        },
        Ok(Err(e)) => {
            overall_healthy = false;
            let err_msg = format!("Instance query failed: {}", e);
            errors.push(format!("sqlite: {}", err_msg));
            let duration_ms = sqlite_start.elapsed().as_millis();
            checks.insert(
                "sqlite".to_string(),
                json!({
                    "status": "unhealthy",
                    "message": err_msg,
                    "duration_ms": duration_ms
                }),
            );
            checks.insert(
                "instances".to_string(),
                json!({
                    "status": "unhealthy",
                    "message": "Commissioned instances are unavailable",
                    "duration_ms": duration_ms
                }),
            );
            "error"
        },
    };

    // Report the validated active Pack/site product-library size.
    let product_start = Instant::now();
    let product_count = state.instance_manager.product_loader().product_count();
    checks.insert(
        "products".to_string(),
        json!({
            "status": "healthy",
            "count": product_count,
            "duration_ms": product_start.elapsed().as_millis()
        }),
    );

    // Check SHM dispatch status
    // Both the SHM writer and connected UDS notifier are required for the
    // service's device-control contract. A configured but disconnected socket
    // must never be advertised as ready.
    let shm_writer_available = state.shm_dispatch.is_writer_available();
    let notifier = state.shm_dispatch.notifier_status();
    let uds_notifier_configured = notifier.configured();
    let uds_notifier_connected = notifier.connected();
    let (shm_status, shm_failure) = command_dispatch_health(
        shm_writer_available,
        uds_notifier_configured,
        uds_notifier_connected,
    );
    if let Some(failure) = shm_failure {
        overall_healthy = false;
        errors.push(format!("shm_dispatch: {failure}"));
    }
    let shm_value = serde_json::json!({
        "status": shm_status,
        "writer_available": shm_writer_available,
        "uds_notifier_configured": uds_notifier_configured,
        "uds_notifier_connected": uds_notifier_connected,
        "uds_last_failure_at_ms": notifier.last_failure_at_ms()
    });

    checks.insert("shm_dispatch".to_string(), shm_value);

    let topology = state.instance_manager.runtime_topology().load();
    let topology_probe = run_singleflight_blocking_probe(
        Arc::clone(&state.io_topology_probe_gate),
        IO_TOPOLOGY_HEALTH_TIMEOUT,
        move || topology.validate_io_freshness(),
    )
    .await;
    match topology_probe {
        BlockingProbe::Completed(Ok(Ok(()))) => {
            checks.insert(
                "io_topology".to_string(),
                json!({"status": "healthy", "writer_heartbeats": "fresh"}),
            );
        },
        BlockingProbe::Completed(Ok(Err(error))) => {
            overall_healthy = false;
            errors.push(format!("io_topology: {error}"));
            checks.insert(
                "io_topology".to_string(),
                json!({"status": "unhealthy", "message": error.to_string()}),
            );
        },
        BlockingProbe::Completed(Err(error)) => {
            overall_healthy = false;
            errors.push(format!("io_topology: probe task failed: {error}"));
            checks.insert(
                "io_topology".to_string(),
                json!({"status": "unhealthy", "message": error.to_string()}),
            );
        },
        BlockingProbe::Busy => {
            overall_healthy = false;
            errors.push("io_topology: freshness probe is already in progress".to_string());
            checks.insert(
                "io_topology".to_string(),
                json!({
                    "status": "unhealthy",
                    "message": "freshness probe is already in progress"
                }),
            );
        },
        BlockingProbe::TimedOut => {
            overall_healthy = false;
            errors.push("io_topology: freshness probe timed out".to_string());
            checks.insert(
                "io_topology".to_string(),
                json!({
                    "status": "unhealthy",
                    "message": "freshness probe timed out",
                    "timeout_ms": IO_TOPOLOGY_HEALTH_TIMEOUT.as_millis()
                }),
            );
        },
    }

    if let Some(scheduler) = state.rule_scheduler.get() {
        let scheduler_status = scheduler.status().await;
        // Historical transient write/flush failures stay observable in stats,
        // but do not make readiness permanently sticky after the worker has
        // recovered. SQLite remains the mandatory durable audit sink.
        let rule_log_healthy = scheduler_status.rule_log.worker_running
            && scheduler_status.rule_log.worker_start_failures == 0
            && scheduler_status.rule_log.shutdown_timeouts == 0;
        let scheduler_healthy = scheduler_status.running
            && scheduler_status.invalid_enabled_rules == 0
            && rule_log_healthy;
        if !scheduler_healthy {
            overall_healthy = false;
            if !scheduler_status.running {
                errors.push("scheduler: rule loop is stopped".to_string());
            }
            if scheduler_status.invalid_enabled_rules > 0 {
                errors.push(format!(
                    "scheduler: {} enabled rule(s) have invalid trigger configuration",
                    scheduler_status.invalid_enabled_rules
                ));
            }
            if !rule_log_healthy {
                errors.push("scheduler: rule audit logger is degraded".to_string());
            }
        }
        checks.insert(
            "rule_scheduler".to_string(),
            json!({
                "status": if scheduler_healthy { "healthy" } else { "unhealthy" },
                "running": scheduler_status.running,
                "total_rules": scheduler_status.total_rules,
                "enabled_rules": scheduler_status.enabled_rules,
                "invalid_enabled_rules": scheduler_status.invalid_enabled_rules,
                "point_watch_configured": scheduler_status.point_watch_configured,
                "point_watch_subscriptions": scheduler_status.point_watch_subscriptions,
                "point_watch_dropped_events": scheduler_status.point_watch_dropped_events,
                "rule_log": scheduler_status.rule_log
            }),
        );
    } else {
        overall_healthy = false;
        errors.push("scheduler: rule runtime is not installed".to_string());
        checks.insert(
            "rule_scheduler".to_string(),
            json!({"status": "unhealthy", "message": "not installed"}),
        );
    }

    // Collect system metrics (CPU, memory)
    let metrics = SystemMetrics::collect();

    let status = if overall_healthy {
        "healthy"
    } else {
        "unhealthy"
    };

    let response = json!({
        "status": status,
        "service": "aether-automation",
        "architecture": "product-instance",
        "sqlite": sqlite_status,
        "checks": checks,
        "system": {
            "cpu_count": metrics.cpu_count,
            "process_cpu_percent": metrics.process_cpu_percent,
            "process_memory_mb": metrics.process_memory_mb,
            "memory_total_mb": metrics.memory_total_mb
        },
        "timestamp": chrono::Utc::now().to_rfc3339()
    });

    // Return 503 if unhealthy
    if !overall_healthy {
        return Err(AppError::service_unavailable(format!(
            "Service dependencies are unhealthy: {}",
            errors.join(", ")
        )));
    }

    Ok(Json(SuccessResponse::new(response)))
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, mpsc};
    use std::time::Duration;

    use tokio::sync::Semaphore;

    use super::{BlockingProbe, command_dispatch_health, run_singleflight_blocking_probe};

    #[test]
    fn disconnected_notifier_is_unhealthy_even_with_a_writer() {
        assert_eq!(
            command_dispatch_health(true, true, false),
            ("writer_only", Some("command notifier disconnected"))
        );
        assert_eq!(command_dispatch_health(true, true, true), ("ready", None));
    }

    #[tokio::test]
    async fn timed_out_probe_remains_singleflight_until_its_worker_exits() {
        let gate = Arc::new(Semaphore::new(1));
        let starts = Arc::new(AtomicUsize::new(0));
        let (release_tx, release_rx) = mpsc::channel();
        let first_starts = Arc::clone(&starts);

        let first = run_singleflight_blocking_probe(
            Arc::clone(&gate),
            Duration::from_millis(10),
            move || {
                first_starts.fetch_add(1, Ordering::SeqCst);
                release_rx.recv().expect("release timed-out probe worker");
            },
        )
        .await;
        assert!(matches!(first, BlockingProbe::TimedOut));

        for _ in 0..3 {
            let subsequent_starts = Arc::clone(&starts);
            let result = run_singleflight_blocking_probe(
                Arc::clone(&gate),
                Duration::from_millis(10),
                move || {
                    subsequent_starts.fetch_add(1, Ordering::SeqCst);
                },
            )
            .await;
            assert!(matches!(result, BlockingProbe::Busy));
        }
        assert_eq!(starts.load(Ordering::SeqCst), 1);

        release_tx.send(()).expect("release probe worker");
        tokio::time::timeout(Duration::from_secs(1), async {
            while gate.available_permits() == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("probe permit must be released by its worker");

        let final_starts = Arc::clone(&starts);
        let final_result =
            run_singleflight_blocking_probe(gate, Duration::from_secs(1), move || {
                final_starts.fetch_add(1, Ordering::SeqCst);
            })
            .await;
        assert!(matches!(final_result, BlockingProbe::Completed(Ok(()))));
        assert_eq!(starts.load(Ordering::SeqCst), 2);
    }
}
