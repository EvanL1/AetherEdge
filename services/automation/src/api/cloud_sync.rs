//! Cloud Sync API Handlers
//!
//! Endpoints for cloud-edge synchronization:
//! - GET /api/instances/export - Export instance topology to cloud

#![allow(clippy::disallowed_methods)] // json! macro used in multiple functions

use axum::{extract::State, response::Json};
use common::SuccessResponse;
use serde::Serialize;
use serde_json::json;
use std::sync::Arc;

use crate::app_state::AppState;
use crate::error::AutomationError;

/// Instance export item (edge → cloud sync)
#[derive(Debug, Serialize)]
pub struct InstanceExport {
    pub id: u32,
    pub name: String,
    pub product: String,
    pub parent_id: Option<u32>,
    pub properties: serde_json::Value,
}

/// Instance topology export response
#[derive(Debug, Serialize)]
pub struct InstanceTopology {
    pub version: String,
    pub instances: Vec<InstanceExport>,
}

/// Export instance topology for cloud sync
///
/// Returns all instances with their topology (parent_id) and properties.
/// Used for edge → cloud synchronization.
///
#[cfg_attr(feature = "openapi", utoipa::path(
    get,
    path = "/api/instances/export",
    tag = "instances",
    responses(
        (status = 200, description = "Instance topology exported",
            body = inline(Object),
            example = json!({
                "success": true,
                "data": {
                    "version": "1.0.0",
                    "instances": [
                        {"id": 1, "name": "pump_001", "product": "pump", "parent_id": null, "properties": {}}
                    ]
                }
            })
        ),
        (status = 500, description = "Database error")
    )
))]
pub async fn export_instances(
    State(state): State<Arc<AppState>>,
) -> Result<Json<SuccessResponse<InstanceTopology>>, AutomationError> {
    let commissioned =
        state.instance_manager.list_instances().await.map_err(|e| {
            AutomationError::InternalError(format!("Failed to query instances: {}", e))
        })?;

    let instances = commissioned
        .into_iter()
        .map(|instance| InstanceExport {
            id: instance.core.instance_id,
            name: instance.core.instance_name,
            product: instance.core.product_name,
            parent_id: instance.core.parent_id,
            properties: json!(instance.core.properties),
        })
        .collect();

    // Use a fixed version for edge export
    let version = "1.0.0".to_string();

    Ok(Json(SuccessResponse::new(InstanceTopology {
        version,
        instances,
    })))
}
