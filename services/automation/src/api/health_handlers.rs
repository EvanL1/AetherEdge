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

use crate::app_state::AppState;

/// Health check endpoint
///
/// Performs actual connectivity checks on dependencies.
/// Returns 503 if any critical dependency is unhealthy.
///
pub async fn health_check(
    State(state): State<Arc<AppState>>,
) -> Result<Json<SuccessResponse<serde_json::Value>>, AppError> {
    let mut checks = serde_json::Map::new();
    let mut overall_healthy = true;
    let mut errors = Vec::new();

    // Check SQLite connectivity using direct query
    let sqlite_start = Instant::now();
    let sqlite_status = match sqlx::query("SELECT 1")
        .fetch_optional(&state.instance_manager.pool)
        .await
    {
        Ok(_) => {
            checks.insert(
                "sqlite".to_string(),
                json!({
                    "status": "healthy",
                    "message": "Connected",
                    "duration_ms": sqlite_start.elapsed().as_millis()
                }),
            );
            "connected"
        },
        Err(e) => {
            overall_healthy = false;
            let err_msg = format!("Ping failed: {}", e);
            errors.push(format!("sqlite: {}", err_msg));
            checks.insert(
                "sqlite".to_string(),
                json!({
                    "status": "unhealthy",
                    "message": err_msg,
                    "duration_ms": sqlite_start.elapsed().as_millis()
                }),
            );
            "error"
        },
    };

    // Check instance manager
    let instance_start = Instant::now();
    match state
        .instance_manager
        .list_instances_paginated(None, 1, 1)
        .await
    {
        Ok((count, _)) => {
            checks.insert(
                "instances".to_string(),
                json!({
                    "status": "healthy",
                    "count": count,
                    "duration_ms": instance_start.elapsed().as_millis()
                }),
            );
        },
        Err(e) => {
            overall_healthy = false;
            let err_msg = format!("Failed to list instances: {}", e);
            errors.push(format!("instances: {}", err_msg));
            checks.insert(
                "instances".to_string(),
                json!({
                    "status": "unhealthy",
                    "message": err_msg,
                    "duration_ms": instance_start.elapsed().as_millis()
                }),
            );
        },
    }

    // Check product loader (products are compile-time constants, always healthy)
    let product_start = Instant::now();
    let products = state.instance_manager.product_loader().get_all_products();
    checks.insert(
        "products".to_string(),
        json!({
            "status": "healthy",
            "count": products.len(),
            "duration_ms": product_start.elapsed().as_millis()
        }),
    );

    // Check SHM dispatch status
    // degraded (no writer) → unhealthy (503): M2C actions will fail
    // writer_only (UDS down) → warning: SHM writes work but io may not process them
    let shm_writer_available = state.shm_dispatch.is_writer_available();
    let uds_notifier_configured = state.shm_dispatch.is_notifier_configured();
    let shm_status = if shm_writer_available && uds_notifier_configured {
        "ready"
    } else if shm_writer_available {
        "writer_only"
    } else {
        overall_healthy = false;
        errors.push("shm_dispatch: writer unavailable (io may have restarted)".to_string());
        "degraded"
    };
    let mut shm_value = serde_json::json!({
        "status": shm_status,
        "writer_available": shm_writer_available,
        "uds_notifier_configured": uds_notifier_configured
    });

    if let Some(stats) = state.instance_manager.slot_bitmap_stats()
        && let Some(obj) = shm_value.as_object_mut()
    {
        obj.insert("total_slots".to_string(), json!(stats.total_slots));
        obj.insert("allocated".to_string(), json!(stats.allocated_slots));
        obj.insert("free".to_string(), json!(stats.free_slots));
    }

    checks.insert("shm_dispatch".to_string(), shm_value);

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
