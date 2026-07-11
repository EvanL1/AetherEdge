//! Product Management API Handlers (Read-only)
//!
//! Provides endpoints for querying product templates and definitions.
//! Products are compile-time constants and cannot be created via API.

#![allow(clippy::disallowed_methods)] // json! macro internally uses unwrap (safe for known valid JSON)

use axum::{
    extract::{Path, State},
    response::Json,
};
use common::SuccessResponse;
use serde_json::json;
use std::sync::Arc;

use crate::app_state::AppState;
use crate::error::AutomationError;

/// List all available product templates (lightweight)
///
/// Returns a lightweight list containing only product names and parent relationships.
/// This endpoint is optimized for frontend dropdown lists and product selection interfaces.
/// For detailed product information including measurements/actions/properties, use GET /api/products/{product_name}/points.
///
#[cfg_attr(feature = "swagger-ui", utoipa::path(
    get,
    path = "/api/products",
    tag = "products",
    responses(
        (status = 200, description = "Lightweight product list retrieved successfully",
            body = inline(Object),
            example = json!({
                "success": true,
                "data": {
                    "count": 9,
                    "products": [
                        {"product_name": "Station", "parent_name": null},
                        {"product_name": "ESS", "parent_name": "Station"},
                        {"product_name": "Battery", "parent_name": "ESS"}
                    ]
                }
            })
        )
    )
))]
pub async fn list_products(
    State(state): State<Arc<AppState>>,
) -> Result<Json<SuccessResponse<serde_json::Value>>, AutomationError> {
    // Products are compile-time constants, no async needed
    let product_names = state
        .instance_manager
        .product_loader()
        .get_all_product_names();

    let products: Vec<serde_json::Value> = product_names
        .into_iter()
        .map(|(product_name, parent_name)| {
            json!({
                "product_name": product_name,
                "parent_name": parent_name
            })
        })
        .collect();

    Ok(Json(SuccessResponse::new(json!({
        "count": products.len(),
        "products": products
    }))))
}

/// Get product definition with nested structure
///
/// Returns detailed product information including all measurement,
/// action, and property points.
///
#[cfg_attr(feature = "swagger-ui", utoipa::path(
    get,
    path = "/api/products/{product_name}/points",
    tag = "products",
    params(
        ("product_name" = String, Path, description = "Product identifier")
    ),
    responses(
        (status = 200, description = "Product details with all points retrieved successfully",
            body = inline(Object),
            example = json!({
                "success": true,
                "data": {
                    "product": {
                        "product_name": "Battery",
                        "parent_name": "ESS",
                        "measurements": [
                            {"measurement_id": 1, "name": "SOC", "unit": "%", "description": null}
                        ],
                        "actions": [
                            {"action_id": 1, "name": "Charge", "unit": null, "description": null}
                        ],
                        "properties": []
                    }
                }
            })
        ),
        (status = 404, description = "Product not found")
    )
))]
pub async fn get_product_points(
    State(state): State<Arc<AppState>>,
    Path(product_name): Path<String>,
) -> Result<Json<SuccessResponse<serde_json::Value>>, AutomationError> {
    // Products are compile-time constants, no async needed
    match state
        .instance_manager
        .product_loader()
        .get_product(&product_name)
    {
        Ok(product) => Ok(Json(SuccessResponse::new(json!({
            "product": product
        })))),
        Err(e) => Err(AutomationError::InternalError(format!(
            "Not found: Product '{}' not found ({})",
            product_name, e
        ))),
    }
}
