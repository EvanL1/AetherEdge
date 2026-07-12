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
#[cfg_attr(feature = "swagger-ui", utoipa::path(
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
    let pool = &state.instance_manager.pool;

    // Query all instances with parent_id
    #[allow(clippy::type_complexity)]
    let rows: Vec<(u32, String, String, Option<u32>, Option<String>)> = sqlx::query_as(
        r#"
        SELECT instance_id, instance_name, product_name, parent_id, properties
        FROM instances
        ORDER BY instance_id
        "#,
    )
    .fetch_all(pool)
    .await
    .map_err(|e| AutomationError::InternalError(format!("Failed to query instances: {}", e)))?;

    let instances: Vec<InstanceExport> = rows
        .into_iter()
        .map(|(id, name, product, parent_id, props_json)| {
            let properties = props_json
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or(json!({}));

            InstanceExport {
                id,
                name,
                product,
                parent_id,
                properties,
            }
        })
        .collect();

    // Use a fixed version for edge export
    let version = "1.0.0".to_string();

    Ok(Json(SuccessResponse::new(InstanceTopology {
        version,
        instances,
    })))
}
