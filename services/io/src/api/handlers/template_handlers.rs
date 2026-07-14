#![allow(clippy::disallowed_methods)]

//! Channel template handlers
//!
//! Provides CRUD + apply operations for protocol point-table templates.
//! Templates capture a channel's complete point definitions and protocol mappings
//! as JSON snapshots, enabling "save once → apply many" workflows.

use crate::api::routes::AppState;
use crate::dto::{
    AppError, ApplyTemplateReq, CreateTemplateFromChannelReq, CreateTemplateReq, PointCounts,
    SuccessResponse, TemplateDetail, TemplateListItem, TemplateListQuery, UpdateTemplateReq,
};
use crate::point_topology::{
    PointDefinitionMutation, PointKind, PointTopologyMutation, PointTopologyMutationResult,
};
use axum::{
    Extension,
    extract::{Path, Query, State},
    http::HeaderMap,
    response::Json,
};
use serde_json::json;

// ============================================================================
// Helpers
// ============================================================================

/// Validate template name: non-empty, trimmed, max 255 chars
fn validate_template_name(name: &str) -> Result<String, AppError> {
    let trimmed = name.trim().to_string();
    if trimmed.is_empty() {
        return Err(AppError::bad_request("Template name cannot be empty"));
    }
    if trimmed.len() > 255 {
        return Err(AppError::bad_request(
            "Template name too long (max 255 chars)",
        ));
    }
    Ok(trimmed)
}

/// Validate that a snapshot JSON has the expected GroupedPoints/GroupedMappings structure
fn validate_snapshot_structure(snapshot: &serde_json::Value) -> Result<(), AppError> {
    let obj = snapshot
        .as_object()
        .ok_or_else(|| AppError::bad_request("Snapshot must be a JSON object"))?;
    let valid_keys = ["telemetry", "signal", "control", "adjustment"];
    for key in obj.keys() {
        if !valid_keys.contains(&key.as_str()) {
            return Err(AppError::bad_request(format!(
                "Unknown key in snapshot: '{}'. Valid keys: {:?}",
                key, valid_keys
            )));
        }
    }
    for key in &valid_keys {
        if let Some(val) = obj.get(*key)
            && !val.is_array()
        {
            return Err(AppError::bad_request(format!(
                "Snapshot key '{}' must be an array",
                key
            )));
        }
    }
    Ok(())
}

/// Count points in a GroupedPoints-style JSON snapshot
fn count_points_from_snapshot(snapshot: &serde_json::Value) -> PointCounts {
    let count_array = |key: &str| -> usize {
        snapshot
            .get(key)
            .and_then(|v| v.as_array())
            .map_or(0, Vec::len)
    };
    PointCounts {
        telemetry: count_array("telemetry"),
        signal: count_array("signal"),
        control: count_array("control"),
        adjustment: count_array("adjustment"),
    }
}

/// Query all points for a channel, returning GroupedPoints-style JSON.
///
/// Note: signal_points has an extra `normal_state` column that other point tables lack.
/// This column is captured in the snapshot so it can be faithfully restored on apply.
async fn snapshot_channel_points(
    pool: &sqlx::SqlitePool,
    channel_id: u32,
) -> Result<serde_json::Value, AppError> {
    let mut result = json!({});

    // Telemetry and Control share the same column set.
    // Table names are from a compile-time constant array, not user input
    let standard_tables = [
        ("telemetry", "telemetry_points"),
        ("control", "control_points"),
    ];

    for (key, table) in standard_tables {
        let query = format!(
            "SELECT point_id, signal_name, scale, offset, unit, data_type, reverse, description \
             FROM {} WHERE channel_id = ? ORDER BY point_id",
            table
        );

        #[allow(clippy::type_complexity)]
        let rows: Vec<(i64, String, f64, f64, String, String, bool, String)> =
            sqlx::query_as(&query)
                .bind(channel_id as i64)
                .fetch_all(pool)
                .await
                .map_err(|e| {
                    tracing::error!("Snapshot points {}: {}", table, e);
                    AppError::internal_error("Database operation failed")
                })?;

        let points: Vec<serde_json::Value> = rows
            .into_iter()
            .map(
                |(point_id, signal_name, scale, offset, unit, data_type, reverse, description)| {
                    json!({
                        "point_id": point_id,
                        "signal_name": signal_name,
                        "scale": scale,
                        "offset": offset,
                        "unit": unit,
                        "data_type": data_type,
                        "reverse": reverse,
                        "description": description,
                    })
                },
            )
            .collect();

        result[key] = json!(points);
    }

    // Signal points: extra `normal_state` column
    {
        let query = "SELECT point_id, signal_name, scale, offset, unit, data_type, reverse, \
                     normal_state, description \
                     FROM signal_points WHERE channel_id = ? ORDER BY point_id";

        #[allow(clippy::type_complexity)]
        let rows: Vec<(i64, String, f64, f64, String, String, bool, i64, String)> =
            sqlx::query_as(query)
                .bind(channel_id as i64)
                .fetch_all(pool)
                .await
                .map_err(|e| {
                    tracing::error!("Snapshot points signal_points: {}", e);
                    AppError::internal_error("Database operation failed")
                })?;

        let points: Vec<serde_json::Value> = rows
            .into_iter()
            .map(
                |(
                    point_id,
                    signal_name,
                    scale,
                    offset,
                    unit,
                    data_type,
                    reverse,
                    normal_state,
                    description,
                )| {
                    json!({
                        "point_id": point_id,
                        "signal_name": signal_name,
                        "scale": scale,
                        "offset": offset,
                        "unit": unit,
                        "data_type": data_type,
                        "reverse": reverse,
                        "normal_state": normal_state,
                        "description": description,
                    })
                },
            )
            .collect();

        result["signal"] = json!(points);
    }

    // Adjustment points carry command constraints which must survive a
    // template round-trip.
    {
        let query = "SELECT point_id, signal_name, scale, offset, unit, data_type, reverse, \
                     description, min_value, max_value, step \
                     FROM adjustment_points WHERE channel_id = ? ORDER BY point_id";
        #[allow(clippy::type_complexity)]
        let rows: Vec<(
            i64,
            String,
            f64,
            f64,
            String,
            String,
            bool,
            String,
            Option<f64>,
            Option<f64>,
            f64,
        )> = sqlx::query_as(query)
            .bind(i64::from(channel_id))
            .fetch_all(pool)
            .await
            .map_err(|error| {
                tracing::error!("Snapshot points adjustment_points: {}", error);
                AppError::internal_error("Database operation failed")
            })?;
        result["adjustment"] = serde_json::Value::Array(
            rows.into_iter()
                .map(
                    |(
                        point_id,
                        signal_name,
                        scale,
                        offset,
                        unit,
                        data_type,
                        reverse,
                        description,
                        minimum,
                        maximum,
                        step,
                    )| {
                        json!({
                            "point_id": point_id,
                            "signal_name": signal_name,
                            "scale": scale,
                            "offset": offset,
                            "unit": unit,
                            "data_type": data_type,
                            "reverse": reverse,
                            "description": description,
                            "min_value": minimum,
                            "max_value": maximum,
                            "step": step,
                        })
                    },
                )
                .collect(),
        );
    }

    Ok(result)
}

/// Query all protocol mappings for a channel, returning GroupedMappings-style JSON
async fn snapshot_channel_mappings(
    pool: &sqlx::SqlitePool,
    channel_id: u32,
) -> Result<serde_json::Value, AppError> {
    let tables = [
        ("telemetry", "telemetry_points"),
        ("signal", "signal_points"),
        ("control", "control_points"),
        ("adjustment", "adjustment_points"),
    ];

    let mut result = json!({});

    for (key, table) in tables {
        // Table names are from a compile-time constant array, not user input
        let query = format!(
            "SELECT point_id, signal_name, protocol_mappings \
             FROM {} WHERE channel_id = ? ORDER BY point_id",
            table
        );

        let rows: Vec<(i64, String, Option<String>)> = sqlx::query_as(&query)
            .bind(channel_id as i64)
            .fetch_all(pool)
            .await
            .map_err(|e| {
                tracing::error!("Snapshot mappings {}: {}", table, e);
                AppError::internal_error("Database operation failed")
            })?;

        let mappings: Vec<serde_json::Value> = rows
            .into_iter()
            .map(|(point_id, signal_name, pm_json)| {
                let protocol_data = pm_json
                    .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
                    .unwrap_or(json!({}));
                json!({
                    "point_id": point_id,
                    "signal_name": signal_name,
                    "protocol_data": protocol_data,
                })
            })
            .collect();

        result[key] = json!(mappings);
    }

    Ok(result)
}

fn template_point_mutations(
    points_snapshot: &serde_json::Value,
    mappings_snapshot: &serde_json::Value,
    slave_id_override: Option<u8>,
) -> Result<Vec<(PointKind, PointDefinitionMutation)>, AppError> {
    let groups = [
        ("telemetry", PointKind::Telemetry),
        ("signal", PointKind::Signal),
        ("control", PointKind::Control),
        ("adjustment", PointKind::Adjustment),
    ];
    let mut result = Vec::new();

    for (group, kind) in groups {
        let mut mappings = std::collections::HashMap::new();
        for mapping in mappings_snapshot
            .get(group)
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
        {
            let point_id = template_point_id(mapping, group)?;
            mappings.insert(
                point_id,
                mapping
                    .get("protocol_data")
                    .cloned()
                    .unwrap_or_else(|| json!({})),
            );
        }

        for point in points_snapshot
            .get(group)
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
        {
            let point_id = template_point_id(point, group)?;
            let signal_name = point
                .get("signal_name")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| {
                    AppError::bad_request(format!(
                        "Template {group} point {point_id} is missing signal_name"
                    ))
                })?
                .to_string();
            let mut protocol_data = mappings.remove(&point_id).unwrap_or_else(|| json!({}));
            if let Some(slave_id) = slave_id_override
                && let Some(values) = protocol_data.as_object_mut()
                && values.contains_key("slave_id")
            {
                values.insert("slave_id".to_string(), serde_json::Value::from(slave_id));
            }
            let protocol_mapping = if protocol_data
                .as_object()
                .is_some_and(serde_json::Map::is_empty)
                || protocol_data.is_null()
            {
                None
            } else {
                Some(serde_json::to_string(&protocol_data).map_err(|error| {
                    AppError::bad_request(format!(
                        "Template {group} point {point_id} has invalid protocol mapping: {error}"
                    ))
                })?)
            };
            result.push((
                kind,
                PointDefinitionMutation {
                    point_id,
                    signal_name,
                    scale: point
                        .get("scale")
                        .and_then(serde_json::Value::as_f64)
                        .unwrap_or(1.0),
                    offset: point
                        .get("offset")
                        .and_then(serde_json::Value::as_f64)
                        .unwrap_or(0.0),
                    unit: point
                        .get("unit")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    reverse: point
                        .get("reverse")
                        .and_then(serde_json::Value::as_bool)
                        .unwrap_or(false),
                    data_type: point
                        .get("data_type")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    description: point
                        .get("description")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    normal_state: point
                        .get("normal_state")
                        .and_then(serde_json::Value::as_i64)
                        .unwrap_or(0),
                    minimum: point.get("min_value").and_then(serde_json::Value::as_f64),
                    maximum: point.get("max_value").and_then(serde_json::Value::as_f64),
                    step: point
                        .get("step")
                        .and_then(serde_json::Value::as_f64)
                        .unwrap_or(1.0),
                    protocol_mapping: Some(protocol_mapping),
                },
            ));
        }
    }
    Ok(result)
}

fn template_point_id(value: &serde_json::Value, group: &str) -> Result<u32, AppError> {
    let raw = value
        .get("point_id")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| AppError::bad_request(format!("Template {group} point_id is invalid")))?;
    u32::try_from(raw)
        .map_err(|_| AppError::bad_request(format!("Template {group} point_id exceeds u32")))
}

// ============================================================================
// Handlers
// ============================================================================

/// List saved device templates (metadata only).
///
/// A template is a reusable snapshot of a channel's point definitions and protocol
/// mappings — importing a template on a new deployment avoids manually configuring
/// 1000+ points. This endpoint returns only metadata (name, description, created_at);
/// the full snapshot is returned by `/templates/{id}`.
#[utoipa::path(
    get,
    path = "/api/templates",
    params(
        ("protocol" = Option<String>, Query, description = "Filter by protocol type (e.g. modbus_tcp)")
    ),
    responses(
        (status = 200, description = "List of templates", body = Vec<TemplateListItem>,
            example = json!({
                "success": true,
                "data": [{
                    "template_id": 1,
                    "name": "Packaging PLC Modbus Template",
                    "description": "Standard packaging-controller point definitions",
                    "protocol": "modbus_tcp",
                    "point_counts": {"telemetry": 30, "signal": 10, "control": 5, "adjustment": 5},
                    "created_at": "2025-10-15T10:30:00Z"
                }]
            })
        )
    ),
    tag = "templates"
)]
pub async fn list_templates(
    State(state): State<AppState>,
    Query(query): Query<TemplateListQuery>,
) -> Result<Json<SuccessResponse<Vec<TemplateListItem>>>, AppError> {
    let (sql, has_filter) = match &query.protocol {
        Some(_) => (
            "SELECT template_id, name, description, protocol, points_snapshot, created_at \
             FROM channel_templates WHERE protocol = ? ORDER BY created_at DESC",
            true,
        ),
        None => (
            "SELECT template_id, name, description, protocol, points_snapshot, created_at \
             FROM channel_templates ORDER BY created_at DESC",
            false,
        ),
    };

    let rows: Vec<(i64, String, Option<String>, String, String, String)> = if has_filter {
        sqlx::query_as(sql)
            .bind(query.protocol.as_deref().unwrap_or_default())
            .fetch_all(&state.sqlite_pool)
            .await
    } else {
        sqlx::query_as(sql).fetch_all(&state.sqlite_pool).await
    }
    .map_err(|e| {
        tracing::error!("List templates: {}", e);
        AppError::internal_error("Database operation failed")
    })?;

    let items: Vec<TemplateListItem> = rows
        .into_iter()
        .map(
            |(template_id, name, description, protocol, points_json, created_at)| {
                let snapshot: serde_json::Value =
                    serde_json::from_str(&points_json).unwrap_or(json!({}));
                let point_counts = count_points_from_snapshot(&snapshot);
                TemplateListItem {
                    template_id,
                    name,
                    description,
                    protocol,
                    point_counts,
                    created_at,
                }
            },
        )
        .collect();

    Ok(Json(SuccessResponse::new(items)))
}

/// Full template content (metadata + point snapshot + protocol mapping snapshot).
///
/// A single template may carry thousands of point definitions; the response body can
/// exceed 1 MB. Call this only when preparing to apply a template or when an
/// administrator needs to inspect the content — do not poll. 404 indicates the
/// `template_id` does not exist.
#[utoipa::path(
    get,
    path = "/api/templates/{id}",
    params(
        ("id" = i64, Path, description = "Template identifier")
    ),
    responses(
        (status = 200, description = "Template detail with full snapshots", body = TemplateDetail),
        (status = 404, description = "Template not found")
    ),
    tag = "templates"
)]
pub async fn get_template(
    Path(template_id): Path<i64>,
    State(state): State<AppState>,
) -> Result<Json<SuccessResponse<TemplateDetail>>, AppError> {
    #[allow(clippy::type_complexity)]
    let row: Option<(
        i64,
        String,
        Option<String>,
        String,
        String,
        String,
        Option<i64>,
        String,
        String,
    )> = sqlx::query_as(
        "SELECT template_id, name, description, protocol, points_snapshot, mappings_snapshot, \
         source_channel_id, created_at, updated_at \
         FROM channel_templates WHERE template_id = ?",
    )
    .bind(template_id)
    .fetch_optional(&state.sqlite_pool)
    .await
    .map_err(|e| {
        tracing::error!("Get template: {}", e);
        AppError::internal_error("Database operation failed")
    })?;

    let Some((
        id,
        name,
        description,
        protocol,
        points_json,
        mappings_json,
        source_channel_id,
        created_at,
        updated_at,
    )) = row
    else {
        return Err(AppError::not_found(format!(
            "Template {} not found",
            template_id
        )));
    };

    let points_snapshot: serde_json::Value = serde_json::from_str(&points_json).map_err(|e| {
        tracing::error!("Corrupt points_snapshot for template {}: {}", id, e);
        AppError::internal_error("Template data is corrupted")
    })?;
    let mappings_snapshot: serde_json::Value =
        serde_json::from_str(&mappings_json).map_err(|e| {
            tracing::error!("Corrupt mappings_snapshot for template {}: {}", id, e);
            AppError::internal_error("Template data is corrupted")
        })?;

    Ok(Json(SuccessResponse::new(TemplateDetail {
        template_id: id,
        name,
        description,
        protocol,
        source_channel_id,
        points_snapshot,
        mappings_snapshot,
        created_at,
        updated_at,
    })))
}

/// Create template from an existing channel
///
/// Snapshots the channel's current point definitions and protocol mappings.
#[utoipa::path(
    post,
    path = "/api/templates/from-channel/{channel_id}",
    params(
        ("channel_id" = u32, Path, description = "Source channel identifier to snapshot")
    ),
    request_body(
        content = CreateTemplateFromChannelReq,
        description = "Template name and optional description"
    ),
    responses(
        (status = 200, description = "Template created from channel snapshot", body = TemplateDetail),
        (status = 404, description = "Channel not found"),
        (status = 409, description = "Template name already exists")
    ),
    tag = "templates"
)]
pub async fn create_template_from_channel(
    Path(channel_id): Path<u32>,
    State(state): State<AppState>,
    Json(req): Json<CreateTemplateFromChannelReq>,
) -> Result<Json<SuccessResponse<TemplateDetail>>, AppError> {
    let name = validate_template_name(&req.name)?;

    // Validate channel exists and get protocol
    let channel_info: Option<(String,)> =
        sqlx::query_as("SELECT protocol FROM channels WHERE channel_id = ?")
            .bind(channel_id as i64)
            .fetch_optional(&state.sqlite_pool)
            .await
            .map_err(|e| {
                tracing::error!("Ch check: {}", e);
                AppError::internal_error("Database operation failed")
            })?;

    let Some((protocol,)) = channel_info else {
        return Err(AppError::not_found(format!(
            "Channel {} not found",
            channel_id
        )));
    };

    // Snapshot points and mappings
    let points_snapshot = snapshot_channel_points(&state.sqlite_pool, channel_id).await?;
    let mappings_snapshot = snapshot_channel_mappings(&state.sqlite_pool, channel_id).await?;

    let points_json = serde_json::to_string(&points_snapshot)
        .map_err(|e| AppError::internal_error(format!("JSON serialize: {}", e)))?;
    let mappings_json = serde_json::to_string(&mappings_snapshot)
        .map_err(|e| AppError::internal_error(format!("JSON serialize: {}", e)))?;

    // Insert template
    let row: (i64, String, String) = sqlx::query_as(
        "INSERT INTO channel_templates (name, description, protocol, points_snapshot, mappings_snapshot, source_channel_id) \
         VALUES (?, ?, ?, ?, ?, ?) \
         RETURNING template_id, created_at, updated_at",
    )
    .bind(&name)
    .bind(&req.description)
    .bind(&protocol)
    .bind(&points_json)
    .bind(&mappings_json)
    .bind(channel_id as i64)
    .fetch_one(&state.sqlite_pool)
    .await
    .map_err(|e| {
        if e.to_string().contains("UNIQUE") {
            AppError::conflict(format!("Template name '{}' already exists", name))
        } else {
            tracing::error!("Insert template: {}", e);
            AppError::internal_error("Database operation failed")
        }
    })?;

    tracing::info!(
        "Template '{}' created from channel {} (id={})",
        name,
        channel_id,
        row.0
    );

    Ok(Json(SuccessResponse::new(TemplateDetail {
        template_id: row.0,
        name,
        description: req.description,
        protocol,
        source_channel_id: Some(channel_id as i64),
        points_snapshot,
        mappings_snapshot,
        created_at: row.1,
        updated_at: row.2,
    })))
}

/// Upload a complete template as JSON directly (without snapshotting an existing channel).
///
/// The counterpart to "create template from channel": the caller supplies the full point
/// definitions and protocol mapping arrays. Use this to import a previously exported
/// template JSON or to create a template programmatically. Returns 400 on schema
/// validation failure, 409 on duplicate name.
#[utoipa::path(
    post,
    path = "/api/templates",
    request_body(
        content = CreateTemplateReq,
        description = "Template with name, protocol, and point/mapping snapshots"
    ),
    responses(
        (status = 200, description = "Template created", body = TemplateDetail),
        (status = 409, description = "Template name already exists")
    ),
    tag = "templates"
)]
pub async fn create_template(
    State(state): State<AppState>,
    Json(req): Json<CreateTemplateReq>,
) -> Result<Json<SuccessResponse<TemplateDetail>>, AppError> {
    let name = validate_template_name(&req.name)?;
    validate_snapshot_structure(&req.points_snapshot)?;
    validate_snapshot_structure(&req.mappings_snapshot)?;

    let points_json = serde_json::to_string(&req.points_snapshot)
        .map_err(|e| AppError::internal_error(format!("JSON serialize: {}", e)))?;
    let mappings_json = serde_json::to_string(&req.mappings_snapshot)
        .map_err(|e| AppError::internal_error(format!("JSON serialize: {}", e)))?;

    let row: (i64, String, String) = sqlx::query_as(
        "INSERT INTO channel_templates (name, description, protocol, points_snapshot, mappings_snapshot) \
         VALUES (?, ?, ?, ?, ?) \
         RETURNING template_id, created_at, updated_at",
    )
    .bind(&name)
    .bind(&req.description)
    .bind(&req.protocol)
    .bind(&points_json)
    .bind(&mappings_json)
    .fetch_one(&state.sqlite_pool)
    .await
    .map_err(|e| {
        if e.to_string().contains("UNIQUE") {
            AppError::conflict(format!("Template name '{}' already exists", name))
        } else {
            tracing::error!("Insert template: {}", e);
            AppError::internal_error("Database operation failed")
        }
    })?;

    tracing::info!("Template '{}' created manually (id={})", name, row.0);

    Ok(Json(SuccessResponse::new(TemplateDetail {
        template_id: row.0,
        name,
        description: req.description,
        protocol: req.protocol,
        source_channel_id: None,
        points_snapshot: req.points_snapshot,
        mappings_snapshot: req.mappings_snapshot,
        created_at: row.1,
        updated_at: row.2,
    })))
}

/// Update template name and/or description (snapshot is not modified).
///
/// There is no endpoint to replace a snapshot — to change the content, DELETE the
/// template and re-POST it. This is intentional: modifying a snapshot while multiple
/// channels reference it would break consistency.
#[utoipa::path(
    put,
    path = "/api/templates/{id}",
    params(
        ("id" = i64, Path, description = "Template identifier")
    ),
    request_body(
        content = UpdateTemplateReq,
        description = "Fields to update (name and/or description)"
    ),
    responses(
        (status = 200, description = "Template updated", body = serde_json::Value),
        (status = 404, description = "Template not found"),
        (status = 409, description = "Template name already exists")
    ),
    tag = "templates"
)]
pub async fn update_template(
    Path(template_id): Path<i64>,
    State(state): State<AppState>,
    Json(req): Json<UpdateTemplateReq>,
) -> Result<Json<SuccessResponse<serde_json::Value>>, AppError> {
    // Build dynamic SET clause
    let mut sets = Vec::new();
    let mut binds: Vec<String> = Vec::new();

    if let Some(ref name) = req.name {
        let validated = validate_template_name(name)?;
        sets.push("name = ?");
        binds.push(validated);
    }
    if let Some(ref desc) = req.description {
        sets.push("description = ?");
        binds.push(desc.clone());
    }

    if sets.is_empty() {
        return Err(AppError::bad_request(
            "At least one field (name, description) must be provided",
        ));
    }

    sets.push("updated_at = CURRENT_TIMESTAMP");

    let sql = format!(
        "UPDATE channel_templates SET {} WHERE template_id = ?",
        sets.join(", ")
    );

    let mut query = sqlx::query(&sql);
    for bind in &binds {
        query = query.bind(bind);
    }
    query = query.bind(template_id);

    let result = query.execute(&state.sqlite_pool).await.map_err(|e| {
        if e.to_string().contains("UNIQUE") {
            AppError::conflict("Template name already exists")
        } else {
            tracing::error!("Update template: {}", e);
            AppError::internal_error("Database operation failed")
        }
    })?;

    if result.rows_affected() == 0 {
        return Err(AppError::not_found(format!(
            "Template {} not found",
            template_id
        )));
    }

    Ok(Json(SuccessResponse::new(
        json!({"template_id": template_id, "message": "Template updated"}),
    )))
}

/// Delete a template.
///
/// Channels already created from this template are **not affected** — the relationship
/// between a template and a channel is a one-time copy-on-apply, not a symlink. Deleting
/// a template simply removes the row from the `templates` table; it can no longer be
/// selected for future apply operations.
#[utoipa::path(
    delete,
    path = "/api/templates/{id}",
    params(
        ("id" = i64, Path, description = "Template identifier")
    ),
    responses(
        (status = 200, description = "Template deleted", body = serde_json::Value),
        (status = 404, description = "Template not found")
    ),
    tag = "templates"
)]
pub async fn delete_template(
    Path(template_id): Path<i64>,
    State(state): State<AppState>,
) -> Result<Json<SuccessResponse<serde_json::Value>>, AppError> {
    let result = sqlx::query("DELETE FROM channel_templates WHERE template_id = ?")
        .bind(template_id)
        .execute(&state.sqlite_pool)
        .await
        .map_err(|e| {
            tracing::error!("Delete template: {}", e);
            AppError::internal_error("Database operation failed")
        })?;

    if result.rows_affected() == 0 {
        return Err(AppError::not_found(format!(
            "Template {} not found",
            template_id
        )));
    }

    tracing::info!("Template {} deleted", template_id);

    Ok(Json(SuccessResponse::new(
        json!({"template_id": template_id, "message": "Template deleted"}),
    )))
}

/// Apply a template to a channel
///
/// Copies all point definitions and protocol mappings from the template to the target channel.
/// Optionally clears existing points and overrides slave_id.
///
/// Uses `ON CONFLICT DO UPDATE` (not `INSERT OR REPLACE`) to avoid triggering
/// `AFTER DELETE` cascade triggers that would remove routing table entries.
#[utoipa::path(
    post,
    path = "/api/templates/{id}/apply/{channel_id}",
    params(
        ("id" = i64, Path, description = "Template identifier"),
        ("channel_id" = u32, Path, description = "Target channel to apply template to")
    ),
    request_body(
        content = ApplyTemplateReq,
        description = "Apply options: clear existing points, override slave_id"
    ),
    responses(
        (status = 200, description = "Template applied to channel", body = serde_json::Value,
            example = json!({
                "success": true,
                "data": {
                    "template_id": 1,
                    "channel_id": 1001,
                    "points_inserted": 50,
                    "cleared_existing": true,
                    "slave_id_override": null,
                    "message": "Template applied: 50 points inserted"
                }
            })
        ),
        (status = 404, description = "Template or channel not found"),
        (status = 400, description = "Protocol mismatch between template and channel")
    ),
    tag = "templates"
)]
pub async fn apply_template(
    Path((template_id, channel_id)): Path<(i64, u32)>,
    State(state): State<AppState>,
    Extension(boundary): Extension<crate::api::handlers::point_handlers::PointTopologyHttpBoundary>,
    headers: HeaderMap,
    Json(req): Json<ApplyTemplateReq>,
) -> Result<Json<SuccessResponse<serde_json::Value>>, AppError> {
    // 1. Load template
    let tpl_row: Option<(String, String, String)> = sqlx::query_as(
        "SELECT protocol, points_snapshot, mappings_snapshot \
         FROM channel_templates WHERE template_id = ?",
    )
    .bind(template_id)
    .fetch_optional(&state.sqlite_pool)
    .await
    .map_err(|e| {
        tracing::error!("Load template: {}", e);
        AppError::internal_error("Database operation failed")
    })?;

    let Some((tpl_protocol, points_json, mappings_json)) = tpl_row else {
        return Err(AppError::not_found(format!(
            "Template {} not found",
            template_id
        )));
    };

    // 2. Verify channel exists and protocol matches
    let ch_row: Option<(String,)> =
        sqlx::query_as("SELECT protocol FROM channels WHERE channel_id = ?")
            .bind(channel_id as i64)
            .fetch_optional(&state.sqlite_pool)
            .await
            .map_err(|e| {
                tracing::error!("Ch check: {}", e);
                AppError::internal_error("Database operation failed")
            })?;

    let Some((ch_protocol,)) = ch_row else {
        return Err(AppError::not_found(format!(
            "Channel {} not found",
            channel_id
        )));
    };

    if !tpl_protocol.eq_ignore_ascii_case(&ch_protocol) {
        return Err(AppError::bad_request(format!(
            "Protocol mismatch: template is '{}' but channel is '{}'",
            tpl_protocol, ch_protocol
        )));
    }

    let points_snapshot: serde_json::Value = serde_json::from_str(&points_json).map_err(|e| {
        tracing::error!(
            "Corrupt points_snapshot for template {}: {}",
            template_id,
            e
        );
        AppError::internal_error("Template data is corrupted")
    })?;
    let mappings_snapshot: serde_json::Value =
        serde_json::from_str(&mappings_json).map_err(|e| {
            tracing::error!(
                "Corrupt mappings_snapshot for template {}: {}",
                template_id,
                e
            );
            AppError::internal_error("Template data is corrupted")
        })?;

    let points =
        template_point_mutations(&points_snapshot, &mappings_snapshot, req.slave_id_override)?;
    let point_count = points.len();
    if point_count == 0 {
        return Err(AppError::bad_request(
            "Template must contain at least one point",
        ));
    }
    let clear_existing = req.clear_existing;
    let slave_id_override = req.slave_id_override;
    let acceptance = boundary
        .mutate(
            &headers,
            PointTopologyMutation::Provision {
                channel_id,
                replace_existing: clear_existing,
                upsert_existing: !clear_existing,
                points,
            },
        )
        .await?;
    let request_id = acceptance.request_id().to_string();
    let resulting_revision = acceptance.resulting_revision().get();
    let completion_audit =
        crate::api::handlers::point_handlers::completion_audit(acceptance.completion_audit());
    let total_inserted = match acceptance.into_result() {
        PointTopologyMutationResult::Provisioned { point_count } => point_count,
        _ => {
            return Err(AppError::internal_error(
                "Point topology application returned an invalid template receipt",
            ));
        },
    };

    tracing::info!(
        "Template {} applied to channel {} ({} points)",
        template_id,
        channel_id,
        total_inserted
    );

    // 4. Trigger channel reload
    crate::api::handlers::point_handlers::trigger_channel_reload_if_needed(
        channel_id, &state, true,
    )
    .await;

    Ok(Json(SuccessResponse::new(json!({
        "template_id": template_id,
        "channel_id": channel_id,
        "points_inserted": total_inserted,
        "cleared_existing": clear_existing,
        "slave_id_override": slave_id_override,
        "request_id": request_id,
        "resulting_revision": resulting_revision,
        "completion_audit": completion_audit,
        "retryable": false,
        "message": format!("Template applied: {} points inserted", total_inserted),
    }))))
}
