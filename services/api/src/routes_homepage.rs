use std::sync::Arc;

use axum::{
    Json,
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
};
use serde::Deserialize;
use serde_json::json;
use tracing::error;

use crate::{
    db,
    models::{CalculatedPoint, CalculatedPointUpdate},
    state::AppState,
};

#[derive(Debug, Deserialize, utoipa::IntoParams)]
pub struct PointsQuery {
    page: Option<i64>,
    limit: Option<i64>,
    name: Option<String>,
}

// ── GET /api/v1/homepage ──────────────────────────────────────────────────────

/// List homepage panel points (paginated).
///
/// "Homepage points" are the key metrics displayed on the operator's main
/// dashboard (e.g. total power, total SOC, grid frequency). They are typically
/// calculated points derived from instance measurements. Each point definition
/// is stored in SQLite and can be filtered by the `name` keyword.
#[utoipa::path(get, path = "/api/v1/homepage", tag = "Homepage",
    security(("bearer_auth" = [])),
    params(PointsQuery),
    responses((status = 200, description = "Calculated point list")))]
pub async fn list_points(
    State(state): State<Arc<AppState>>,
    Query(q): Query<PointsQuery>,
) -> impl IntoResponse {
    let page = q.page.unwrap_or(1).max(1);
    let limit = q.limit.unwrap_or(20).clamp(1, 100);
    let offset = (page - 1) * limit;

    match db::get_all_calculated_points(&state.db, offset, limit, q.name.as_deref()).await {
        Ok((items, total)) => {
            let pages = (total + limit - 1) / limit;
            Json(json!({
                "success": true,
                "message": "Calculated points retrieved",
                "data": {
                    "items": items,
                    "total": total,
                    "page": page,
                    "limit": limit,
                    "pages": pages,
                }
            }))
            .into_response()
        },
        Err(e) => {
            error!("List points error: {}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"success": false, "message": "Internal server error"})),
            )
                .into_response()
        },
    }
}

// ── GET /api/v1/homepage/:id ──────────────────────────────────────────────────

/// Retrieve the full definition of a single homepage point.
///
/// Includes display name, formula/source, unit, and threshold alarm settings.
/// Used to pre-populate the "edit point" dialog. Returns 404 if the point ID
/// does not exist.
#[utoipa::path(get, path = "/api/v1/homepage/{id}", tag = "Homepage",
    security(("bearer_auth" = [])),
    params(("id" = i64, Path, description = "Point ID")),
    responses((status = 200, description = "Point definition", body = CalculatedPoint), (status = 404, description = "Not found")))]
pub async fn get_point(
    State(state): State<Arc<AppState>>,
    Path(point_id): Path<i64>,
) -> impl IntoResponse {
    match db::get_calculated_point_by_id(&state.db, point_id).await {
        Ok(Some(point)) => Json(json!({
            "success": true,
            "message": "Calculated point retrieved",
            "data": point,
        }))
        .into_response(),
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(json!({"success": false, "message": format!("Calculated point ID {} not found", point_id)})),
        )
            .into_response(),
        Err(e) => {
            error!("Get point error: {}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"success": false, "message": "Internal server error"})),
            )
                .into_response()
        },
    }
}

// ── PUT /api/v1/homepage/:id ──────────────────────────────────────────────────

/// Update a single homepage point (partial update).
///
/// All fields are optional; omitted fields retain their current values. Used
/// for drag-and-drop layout changes, formula edits, and threshold updates.
/// Changes take effect immediately — the next frontend poll or WebSocket push
/// will use the new definition.
#[utoipa::path(put, path = "/api/v1/homepage/{id}", tag = "Homepage",
    security(("bearer_auth" = [])),
    params(("id" = i64, Path, description = "Point ID")),
    request_body = CalculatedPointUpdate,
    responses((status = 200, description = "Point updated", body = CalculatedPoint), (status = 404, description = "Not found")))]
pub async fn update_point(
    State(state): State<Arc<AppState>>,
    Path(point_id): Path<i64>,
    Json(body): Json<CalculatedPointUpdate>,
) -> impl IntoResponse {
    // Check existence
    match db::get_calculated_point_by_id(&state.db, point_id).await {
        Ok(None) => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({"success": false, "message": format!("Calculated point ID {} not found", point_id)})),
            )
                .into_response();
        },
        Err(e) => {
            error!("DB error: {}", e);
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"success": false, "message": "Internal server error"})),
            )
                .into_response();
        },
        _ => {},
    }

    match db::update_calculated_point(
        &state.db,
        point_id,
        body.name.as_deref(),
        body.formula.as_deref(),
        body.unit.as_deref(),
        body.imgurl.as_deref(),
        body.description.as_deref(),
    )
    .await
    {
        Ok(_) => match db::get_calculated_point_by_id(&state.db, point_id).await {
            Ok(Some(updated)) => Json(json!({
                "success": true,
                "message": "Calculated point updated",
                "data": updated,
            }))
            .into_response(),
            _ => (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"success": false, "message": "Internal server error"})),
            )
                .into_response(),
        },
        Err(e) => {
            error!("Update point error: {}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"success": false, "message": "Internal server error"})),
            )
                .into_response()
        },
    }
}

// ── POST /api/v1/homepage/reset ───────────────────────────────────────────────

/// Reset homepage points to factory defaults.
///
/// Clears all current point definitions and re-inserts the built-in defaults
/// (total plant power, SOC, temperature, grid-tie status, etc.). Use when the
/// configuration has been corrupted or after an upgrade to pull in new default
/// points. **Destructive operation** — all user-customised point definitions
/// are overwritten and cannot be recovered.
#[utoipa::path(post, path = "/api/v1/homepage/reset", tag = "Homepage",
    security(("bearer_auth" = [])),
    responses((status = 200, description = "Default points restored")))]
pub async fn reset_points(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    match db::reset_calculated_points(&state.db).await {
        Ok(count) => Json(json!({
            "success": true,
            "message": "Default settings restored",
            "data": {
                "imported_count": count,
                "note": "所有自定义点位已被删除，已导入默认点位数据",
            }
        }))
        .into_response(),
        Err(e) => {
            error!("Reset points error: {}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"success": false, "message": "Internal server error"})),
            )
                .into_response()
        },
    }
}
