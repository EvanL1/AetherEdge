//! HTTP route handlers for the alarm service

use std::collections::HashMap;
use std::sync::Arc;

use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, patch, post},
};
use chrono::TimeZone;
use serde_json::{Value, json};
use tracing::error;
use utoipa::OpenApi;
#[cfg(feature = "swagger-ui")]
use utoipa_swagger_ui::{Config, SwaggerUi};

use crate::db::{self};
use crate::models::{
    AlertEvent, AlertQueryParams, AlertRule, ApiResponse, CreateRuleRequest, EventQueryParams,
    MonitorStatus, RuleQueryParams, UpdateRuleRequest,
};
use crate::monitor;
use crate::state::AppState;

// ============================================================================
// Router
// ============================================================================

pub fn create_routes(state: Arc<AppState>) -> Router {
    let api = Router::new()
        // Service meta
        .route("/", get(service_info))
        .route("/health", get(health))
        // Rules
        .route("/alarmApi/rules", get(list_rules).post(create_rule))
        .route("/alarmApi/rules/channel/{channel_id}", get(rules_by_channel))
        .route(
            "/alarmApi/rules/{id}",
            get(get_rule)
                .put(update_rule)
                .delete(delete_rule),
        )
        .route("/alarmApi/rules/{id}/enable", patch(enable_rule))
        .route("/alarmApi/rules/{id}/disable", patch(disable_rule))
        // Alerts
        .route("/alarmApi/alerts", get(list_alerts))
        .route("/alarmApi/alerts/{id}", get(get_alert))
        .route("/alarmApi/alerts/{id}/resolve", patch(resolve_alert))
        // Alert events
        .route("/alarmApi/alert-events", get(list_events))
        .route("/alarmApi/alert-events/export", get(export_events_csv))
        // Statistics & monitor
        .route("/alarmApi/alert-statistics", get(alert_statistics))
        .route("/alarmApi/monitor/status", get(monitor_status))
        .route("/alarmApi/monitor/check-rule/{id}", post(manual_check_rule))
        .route("/alarmApi/call-data", post(call_data))
        // Admin API (shared endpoints from common lib)
        .route("/api/admin/logs/level", get(common::admin_api::get_log_level).post(common::admin_api::set_log_level))
        .route("/api/admin/logs/files", get(common::admin_api::list_log_files))
        .route("/api/admin/logs/view", get(common::admin_api::view_log_file))
        .with_state(state);

    #[cfg(feature = "swagger-ui")]
    let api = api.merge(
        SwaggerUi::new("/docs")
            .url("/openapi.json", ApiDoc::openapi())
            .config(
                Config::default()
                    .default_model_rendering("model")
                    .default_models_expand_depth(1),
            ),
    );

    api
}

// ============================================================================
// OpenAPI document (only consumed when swagger-ui feature is enabled)
// ============================================================================

#[cfg_attr(not(feature = "swagger-ui"), allow(dead_code))]
#[derive(OpenApi)]
#[openapi(
    paths(
        service_info,
        health,
        list_rules,
        create_rule,
        get_rule,
        update_rule,
        delete_rule,
        enable_rule,
        disable_rule,
        rules_by_channel,
        list_alerts,
        get_alert,
        resolve_alert,
        list_events,
        export_events_csv,
        alert_statistics,
        monitor_status,
        manual_check_rule,
        call_data,
    ),
    components(schemas(
        AlertRule,
        crate::models::Alert,
        AlertEvent,
        CreateRuleRequest,
        UpdateRuleRequest,
        MonitorStatus,
    )),
    tags(
        (name = "Rules",   description = "Alarm rule CRUD"),
        (name = "Alerts",  description = "Active alert query and resolution"),
        (name = "Events",  description = "Alert event history and export"),
        (name = "Monitor", description = "Monitor status and manual trigger"),
        (name = "Meta",    description = "Service info"),
    ),
    info(title = "AetherEMS Alarm Service", version = "1.0.0",
         description = "Alarm rule management, active alert monitoring, event history query")
)]
pub struct ApiDoc;

// ============================================================================
// Service meta
// ============================================================================

/// Service banner with name and version.
///
/// Returns the alarm build metadata. Used by deployment scripts and the
/// gateway's service-discovery UI to confirm the binary is reachable and to
/// surface the running version.
#[utoipa::path(get, path = "/", tag = "Meta",
    responses((status = 200, description = "Service basic info")))]
async fn service_info() -> Json<Value> {
    Json(json!({
        "success": true,
        "message": "Service is running",
        "data": {
            "name": "aether-alarm",
            "version": env!("CARGO_PKG_VERSION"),
            "description": "AetherEMS alarm service (Rust)",
        }
    }))
}

/// Liveness probe.
///
/// Always returns 200 if the HTTP server is up. Does **not** verify SQLite,
/// SHM, or the monitor loop — use `/alarmApi/monitor/status` for the latter.
#[utoipa::path(get, path = "/health", tag = "Meta",
    responses((status = 200, description = "Health check")))]
async fn health() -> Json<Value> {
    Json(json!({ "success": true, "message": "Service is running" }))
}

// ============================================================================
// Alert rules
// ============================================================================

/// List alarm rules (paged, filterable).
///
/// Returns the full rule definition (threshold, operator, target point,
/// warning level, enabled flag). Supports keyword search, filter by
/// `service_type` / `channel_id` / `data_type` / `enabled` / `warning_level`,
/// and either page/page_size or skip/limit pagination.
///
/// Channel-online sentinel rules (`service_type=io, data_type=online`)
/// are listed alongside regular threshold rules; the consumer can tell them
/// apart by `data_type`.
#[utoipa::path(get, path = "/alarmApi/rules", tag = "Rules",
    params(RuleQueryParams),
    responses(
        (status = 200, description = "Rule list"),
    ))]
async fn list_rules(
    State(state): State<Arc<AppState>>,
    Query(params): Query<RuleQueryParams>,
) -> impl IntoResponse {
    match db::list_rules(&state.db, &params).await {
        Ok(paged) => {
            let msg = format!("Found {} rule(s)", paged.total);
            Json(ApiResponse::ok(msg, paged)).into_response()
        },
        Err(e) => {
            error!("list_rules: {}", e);
            server_error("Failed to query rules")
        },
    }
}

/// Create a new alarm rule.
///
/// Binds a logical `(service_type, channel_id, data_type, point_id)` target to
/// a threshold comparison. SQLite resolves the target to SHM; PointWatch
/// provides wake-up hints and the monitor loop periodically reconciles it.
///
/// **Sentinel shape for channel-online rules**: set
/// `service_type=io, data_type=online, point_id=0` and the rule pins to
/// the channel-health SHM entry for `channel_id`. A non-zero `point_id` is
/// rejected with 400 because that coordinate is unused for online rules.
///
/// Two conflict modes return 409 with a `conflict` field in the body:
/// * `duplicate_name`: rule_name already taken (case-insensitive)
/// * `duplicate_point`: another rule already monitors the same point
#[utoipa::path(post, path = "/alarmApi/rules", tag = "Rules",
    request_body = CreateRuleRequest,
    responses(
        (status = 200, description = "Rule created", body = AlertRule),
        (status = 400, description = "Invalid operator"),
        (status = 409, description = "Duplicate rule"),
    ))]
async fn create_rule(
    State(state): State<Arc<AppState>>,
    Json(req): Json<CreateRuleRequest>,
) -> impl IntoResponse {
    if !is_valid_operator(&req.operator) {
        return bad_request("Invalid operator. Allowed: >, <, >=, <=, ==, !=");
    }
    if let Err(msg) = validate_channel_online_shape(&req.service_type, &req.data_type, req.point_id)
    {
        return bad_request(&msg);
    }

    // Reject duplicate rule name (case-insensitive)
    match db::find_rule_by_name(&state.db, &req.rule_name).await {
        Ok(Some(existing)) => {
            return (
                StatusCode::CONFLICT,
                Json(json!({
                    "success": false,
                    "message": format!("A rule named '{}' already exists", req.rule_name),
                    "data": {
                        "conflict": "duplicate_name",
                        "existing_rule": {
                            "id": existing.id,
                            "rule_name": existing.rule_name,
                            "created_at": existing.created_at,
                        }
                    }
                })),
            )
                .into_response();
        },
        Ok(None) => {},
        Err(e) => {
            error!("create_rule name check: {}", e);
            return server_error("Failed to create rule");
        },
    }

    // Reject duplicate point binding: same (service_type, channel_id, data_type, point_id)
    match db::find_rule_by_point(
        &state.db,
        &req.service_type,
        req.channel_id,
        &req.data_type,
        req.point_id,
    )
    .await
    {
        Ok(Some(existing)) => {
            return (
                StatusCode::CONFLICT,
                Json(json!({
                    "success": false,
                    "message": format!(
                        "A rule already monitors this point (service:{}, channel:{}, type:{}, point:{})",
                        req.service_type, req.channel_id, req.data_type, req.point_id
                    ),
                    "data": {
                        "conflict": "duplicate_point",
                        "existing_rule": {
                            "id": existing.id,
                            "rule_name": existing.rule_name,
                            "created_at": existing.created_at,
                        },
                        "suggestion": format!("Update the existing rule (id:{}) or choose a different point", existing.id),
                    }
                })),
            )
                .into_response();
        },
        Ok(None) => {},
        Err(e) => {
            error!("create_rule point check: {}", e);
            return server_error("Failed to create rule");
        },
    }

    match db::insert_rule(
        &state.db,
        &req.service_type,
        req.channel_id,
        &req.data_type,
        req.point_id,
        &req.rule_name,
        req.warning_level,
        &req.operator,
        req.value,
        req.enabled,
        req.description.as_deref(),
    )
    .await
    {
        Ok(id) => {
            let rule = db::get_rule_by_id(&state.db, id).await.ok().flatten();
            let logical_key = rule.as_ref().map(AlertRule::logical_key);
            Json(ApiResponse::ok(
                format!("Rule '{}' created", req.rule_name),
                json!({
                    "rule_id": id,
                    "rule_name": req.rule_name,
                    "logical_key": logical_key,
                    "point_id": req.point_id,
                    "monitoring": req.enabled,
                    "rule": rule,
                }),
            ))
            .into_response()
        },
        Err(e) => {
            error!("create_rule: {}", e);
            server_error("Failed to create rule")
        },
    }
}

/// Get one alarm rule by its primary key.
///
/// Response wraps the rule in `{ total: 1, list: [rule] }` for
/// compatibility with the legacy Python-era frontend that consumed
/// `data.list[0]`. Use `list_rules` for multi-rule queries.
#[utoipa::path(get, path = "/alarmApi/rules/{id}", tag = "Rules",
    params(("id" = i64, Path, description = "Rule ID")),
    responses(
        (status = 200, description = "Rule detail", body = AlertRule),
        (status = 404, description = "Rule not found"),
    ))]
async fn get_rule(State(state): State<Arc<AppState>>, Path(id): Path<i64>) -> impl IntoResponse {
    match db::get_rule_by_id(&state.db, id).await {
        Ok(Some(rule)) => {
            // Return list format for compatibility with alarm-py (data.list[0])
            Json(ApiResponse::ok(
                "Rule retrieved",
                json!({ "total": 1, "list": [rule] }),
            ))
            .into_response()
        },
        Ok(None) => not_found("Rule not found"),
        Err(e) => {
            error!("get_rule: {}", e);
            server_error("Failed to get rule")
        },
    }
}

/// Update an alarm rule (partial patch).
///
/// All fields in `UpdateRuleRequest` are optional; only those supplied are
/// written. After a successful update, `monitor::on_rule_updated` runs — if
/// the rule's `enabled` flag flipped to false, its active alerts are
/// resolved with reason "rule disabled" (stored as the Chinese literal "规则被禁用" in alert records).
///
/// If the patch touches `service_type` / `data_type` / `point_id`, the
/// resulting tuple is re-validated against the channel-online sentinel
/// shape (see POST), so partial updates can't sneak a malformed rule
/// through.
#[utoipa::path(put, path = "/alarmApi/rules/{id}", tag = "Rules",
    params(("id" = i64, Path, description = "Rule ID")),
    request_body = UpdateRuleRequest,
    responses(
        (status = 200, description = "Rule updated", body = AlertRule),
        (status = 400, description = "Invalid operator"),
        (status = 404, description = "Rule not found"),
    ))]
async fn update_rule(
    State(state): State<Arc<AppState>>,
    Path(id): Path<i64>,
    Json(req): Json<UpdateRuleRequest>,
) -> impl IntoResponse {
    if let Some(ref op) = req.operator
        && !is_valid_operator(op)
    {
        return bad_request("Invalid operator. Allowed: >, <, >=, <=, ==, !=");
    }
    // If any of (service_type, data_type, point_id) is being updated we need
    // to re-validate the sentinel shape against the resulting tuple. Pull the
    // existing row and overlay the patch fields.
    if req.service_type.is_some() || req.data_type.is_some() || req.point_id.is_some() {
        match db::get_rule_by_id(&state.db, id).await {
            Ok(Some(existing)) => {
                let service_type = req
                    .service_type
                    .as_deref()
                    .unwrap_or(&existing.service_type);
                let data_type = req.data_type.as_deref().unwrap_or(&existing.data_type);
                let point_id = req.point_id.unwrap_or(existing.point_id);
                if let Err(msg) = validate_channel_online_shape(service_type, data_type, point_id) {
                    return bad_request(&msg);
                }
            },
            Ok(None) => return not_found("Rule not found"),
            Err(e) => {
                error!("update_rule fetch existing: {}", e);
                return server_error("Failed to update rule");
            },
        }
    }

    // If renaming, ensure the new name does not clash with another rule
    if let Some(ref new_name) = req.rule_name {
        match db::find_rule_by_name(&state.db, new_name).await {
            Ok(Some(existing)) if existing.id != id => {
                return (
                    StatusCode::CONFLICT,
                    Json(json!({
                        "success": false,
                        "message": format!("A rule named '{}' already exists", new_name),
                        "data": {
                            "conflict": "duplicate_name",
                            "existing_rule": { "id": existing.id, "rule_name": existing.rule_name }
                        }
                    })),
                )
                    .into_response();
            },
            Ok(_) => {},
            Err(e) => {
                error!("update_rule name check: {}", e);
                return server_error("Failed to update rule");
            },
        }
    }

    match db::update_rule(
        &state.db,
        id,
        req.service_type.as_deref(),
        req.channel_id,
        req.data_type.as_deref(),
        req.point_id,
        req.rule_name.as_deref(),
        req.warning_level,
        req.operator.as_deref(),
        req.value,
        req.enabled,
        req.description.as_deref().map(Some),
    )
    .await
    {
        Ok(true) => {
            monitor::on_rule_updated(&state, id).await;
            Json(ApiResponse::ok("Rule updated", json!({ "rule_id": id }))).into_response()
        },
        Ok(false) => not_found("Rule not found"),
        Err(e) => {
            error!("update_rule: {}", e);
            server_error("Failed to update rule")
        },
    }
}

/// Delete an alarm rule.
///
/// Cascade behavior: any active alerts produced by this rule are first
/// resolved with reason "rule deleted" (stored as the Chinese literal "规则被删除"; broadcast to the WebSocket so the UI
/// clears them), then the `alert_rule` row is removed. `alert_event` rows
/// (the historical event log) are kept — they reference the rule by id
/// only and survive deletion for audit.
#[utoipa::path(delete, path = "/alarmApi/rules/{id}", tag = "Rules",
    params(("id" = i64, Path, description = "Rule ID")),
    responses(
        (status = 200, description = "Rule deleted"),
        (status = 404, description = "Rule not found"),
    ))]
async fn delete_rule(State(state): State<Arc<AppState>>, Path(id): Path<i64>) -> impl IntoResponse {
    let rule = match db::get_rule_by_id(&state.db, id).await {
        Ok(Some(r)) => r,
        Ok(None) => return not_found("Rule not found"),
        Err(e) => {
            error!("delete_rule fetch: {}", e);
            return server_error("Failed to delete rule");
        },
    };

    monitor::on_rule_deleted(&state, &rule).await;

    match db::delete_rule(&state.db, id).await {
        Ok(true) => Json(ApiResponse::ok("Rule deleted", json!({ "rule_id": id }))).into_response(),
        Ok(false) => not_found("Rule not found"),
        Err(e) => {
            error!("delete_rule: {}", e);
            server_error("Failed to delete rule")
        },
    }
}

/// Enable a rule (set `enabled=true`).
///
/// The rule joins the polling loop on the next tick. Convenience shortcut
/// over PUT with `{"enabled": true}`.
#[utoipa::path(patch, path = "/alarmApi/rules/{id}/enable", tag = "Rules",
    params(("id" = i64, Path, description = "Rule ID")),
    responses(
        (status = 200, description = "Rule enabled"),
        (status = 404, description = "Rule not found"),
    ))]
async fn enable_rule(State(state): State<Arc<AppState>>, Path(id): Path<i64>) -> impl IntoResponse {
    match db::set_rule_enabled(&state.db, id, true).await {
        Ok(true) => Json(ApiResponse::ok("Rule enabled", json!({ "rule_id": id }))).into_response(),
        Ok(false) => not_found("Rule not found"),
        Err(e) => {
            error!("enable_rule: {}", e);
            server_error("Failed to enable rule")
        },
    }
}

/// Disable a rule (set `enabled=false`).
///
/// Stops the monitor from evaluating this rule on the next tick AND resolves
/// any currently-active alerts produced by it (reason "rule disabled", stored as "规则被禁用"), so the
/// UI clears stale alerts immediately rather than waiting for them to age
/// out. Convenience over PUT with `{"enabled": false}` plus the side effect.
#[utoipa::path(patch, path = "/alarmApi/rules/{id}/disable", tag = "Rules",
    params(("id" = i64, Path, description = "Rule ID")),
    responses(
        (status = 200, description = "Rule disabled"),
        (status = 404, description = "Rule not found"),
    ))]
async fn disable_rule(
    State(state): State<Arc<AppState>>,
    Path(id): Path<i64>,
) -> impl IntoResponse {
    match db::set_rule_enabled(&state.db, id, false).await {
        Ok(true) => {
            monitor::on_rule_updated(&state, id).await;
            Json(ApiResponse::ok("Rule disabled", json!({ "rule_id": id }))).into_response()
        },
        Ok(false) => not_found("Rule not found"),
        Err(e) => {
            error!("disable_rule: {}", e);
            server_error("Failed to disable rule")
        },
    }
}

/// List all rules bound to a given channel.
///
/// Convenience over `list_rules?channel_id=N` that returns the full set
/// (not paged) wrapped in `PagedData` for response-shape consistency. Used
/// by the channel-detail UI to render "alarms watching this channel".
#[utoipa::path(get, path = "/alarmApi/rules/channel/{channel_id}", tag = "Rules",
    params(("channel_id" = i64, Path, description = "Channel ID")),
    responses((status = 200, description = "Rules for the given channel")))]
async fn rules_by_channel(
    State(state): State<Arc<AppState>>,
    Path(channel_id): Path<i64>,
) -> impl IntoResponse {
    match db::get_rules_by_channel(&state.db, channel_id).await {
        Ok(list) => {
            let total = list.len() as i64;
            let page_size = total.max(1);
            Json(ApiResponse::ok(
                format!("Found {} rule(s) for channel {}", total, channel_id),
                crate::models::PagedData {
                    total,
                    list,
                    page: 1,
                    page_size,
                },
            ))
            .into_response()
        },
        Err(e) => {
            error!("rules_by_channel: {}", e);
            server_error("Failed to query rules")
        },
    }
}

// ============================================================================
// Alerts
// ============================================================================

/// List currently active alerts (paged).
///
/// Returns rows from the `alert` table (status=active only — resolved
/// alerts have been moved to `alert_event`). Supports keyword search and
/// filter by warning_level / service_type / channel_id. For historical
/// alerts (resolved or trigger events) use `/alarmApi/alert-events`.
#[utoipa::path(get, path = "/alarmApi/alerts", tag = "Alerts",
    params(AlertQueryParams),
    responses((status = 200, description = "Active alert list")))]
async fn list_alerts(
    State(state): State<Arc<AppState>>,
    Query(params): Query<AlertQueryParams>,
) -> impl IntoResponse {
    match db::list_alerts(&state.db, &params).await {
        Ok(paged) => Json(ApiResponse::ok(
            format!("Found {} active alert(s)", paged.total),
            paged,
        ))
        .into_response(),
        Err(e) => {
            error!("list_alerts: {}", e);
            server_error("Failed to query alerts")
        },
    }
}

/// Get one active alert by id.
///
/// Same legacy-compat `{ total: 1, list: [alert] }` envelope as
/// `get_rule`. Returns 404 once the alert is resolved (it has moved to
/// `alert_event`).
#[utoipa::path(get, path = "/alarmApi/alerts/{id}", tag = "Alerts",
    params(("id" = i64, Path, description = "Alert ID")),
    responses(
        (status = 200, description = "Alert detail", body = crate::models::Alert),
        (status = 404, description = "Alert not found"),
    ))]
async fn get_alert(State(state): State<Arc<AppState>>, Path(id): Path<i64>) -> impl IntoResponse {
    match db::get_alert_by_id(&state.db, id).await {
        Ok(Some(alert)) => {
            // Return list format for compatibility with alarm-py (data.list[0])
            Json(ApiResponse::ok(
                "Alert retrieved",
                json!({ "total": 1, "list": [alert] }),
            ))
            .into_response()
        },
        Ok(None) => not_found("Alert not found"),
        Err(e) => {
            error!("get_alert: {}", e);
            server_error("Failed to get alert")
        },
    }
}

/// Manually resolve an active alert.
///
/// Operator-driven recovery for the case where the underlying condition has
/// cleared but the polling loop hasn't seen the new value yet (or the rule's
/// data source is broken). Moves the row from `alert` → `alert_event`,
/// captures the current value as `recovery_value`, and broadcasts a
/// `send_alarm_recovery` event with reason "manually resolved" to the
/// WebSocket so the UI clears.
///
/// The recovery is permanent for this alert id; if the underlying condition
/// is still true, the next monitor tick will create a NEW alert with a new
/// id, not resurrect this one.
#[utoipa::path(patch, path = "/alarmApi/alerts/{id}/resolve", tag = "Alerts",
    params(("id" = i64, Path, description = "Alert ID")),
    responses(
        (status = 200, description = "Alert resolved"),
        (status = 404, description = "Alert not found"),
    ))]
async fn resolve_alert(
    State(state): State<Arc<AppState>>,
    Path(id): Path<i64>,
) -> impl IntoResponse {
    let alert = match db::get_alert_by_id(&state.db, id).await {
        Ok(Some(a)) => a,
        Ok(None) => return not_found("Alert not found"),
        Err(e) => {
            error!("resolve_alert fetch: {}", e);
            return server_error("Failed to resolve alert");
        },
    };

    let recovery_value = alert.current_value;
    let rule_id = alert.rule_id;

    match db::resolve_alert(&state.db, &alert, recovery_value).await {
        Ok(_) => {
            if let Ok(Some(rule)) = db::get_rule_by_id(&state.db, rule_id).await {
                state
                    .broadcaster
                    .send_alarm_recovery(id, &rule, Some(recovery_value), "manually resolved")
                    .await;
            }
            if let Ok(counts) = db::get_active_alarm_counts(&state.db).await {
                state.broadcaster.send_alarm_count(&counts).await;
            }
            Json(ApiResponse::ok("Alert resolved", json!({ "alert_id": id }))).into_response()
        },
        Err(e) => {
            error!("resolve_alert: {}", e);
            server_error("Failed to resolve alert")
        },
    }
}

// ============================================================================
// Alert events
// ============================================================================

/// Query the historical alert event log (paged).
///
/// `alert_event` records every trigger and recovery transition, so a single
/// alarm episode is two rows (one `event_type=trigger`, one
/// `event_type=recovery`). Supports filter by rule_id / event_type /
/// service_type / warning_level / time range (start_time/end_time, epoch
/// seconds).
///
/// Active (unresolved) alerts live in `alert` and only appear here once they
/// recover or are deleted — use `/alarmApi/alerts` if you want "currently
/// firing".
#[utoipa::path(get, path = "/alarmApi/alert-events", tag = "Events",
    params(EventQueryParams),
    responses((status = 200, description = "Alert event history list")))]
async fn list_events(
    State(state): State<Arc<AppState>>,
    Query(params): Query<EventQueryParams>,
) -> impl IntoResponse {
    match db::list_events(&state.db, &params).await {
        Ok(paged) => Json(ApiResponse::ok(
            format!("Found {} event(s)", paged.total),
            paged,
        ))
        .into_response(),
        Err(e) => {
            error!("list_events: {}", e);
            server_error("Failed to query alert events")
        },
    }
}

/// Export alert events as CSV.
///
/// Accepts the same filters as `list_events` but bypasses pagination —
/// returns all matching rows in one CSV stream with
/// `Content-Disposition: attachment; filename=alert_events.csv`. Used for
/// regulatory / operations report export.
///
/// Beware of unbounded result sets: an empty filter dumps the entire
/// `alert_event` table. Frontend should encourage operators to set a time
/// range.
#[utoipa::path(get, path = "/alarmApi/alert-events/export", tag = "Events",
    params(EventQueryParams),
    responses(
        (status = 200, description = "CSV file stream",
         content_type = "text/csv"),
    ))]
async fn export_events_csv(
    State(state): State<Arc<AppState>>,
    Query(params): Query<EventQueryParams>,
) -> impl IntoResponse {
    let events = match db::get_all_events_for_export(&state.db, &params).await {
        Ok(e) => e,
        Err(e) => {
            error!("export_events_csv: {}", e);
            return server_error("Export failed");
        },
    };

    let mut wtr = csv::WriterBuilder::new().from_writer(vec![]);

    // Header
    let _ = wtr.write_record([
        "Event ID",
        "Rule ID",
        "Rule Name",
        "Service Type",
        "Channel ID",
        "Data Type",
        "Point ID",
        "Warning Level",
        "Operator",
        "Threshold",
        "Trigger Value",
        "Recovery Value",
        "Event Type",
        "Triggered At",
        "Recovered At",
        "Duration (Seconds)",
    ]);

    for ev in &events {
        let triggered_str = ev.triggered_at.map(format_timestamp).unwrap_or_default();
        let recovered_str = ev.recovered_at.map(format_timestamp).unwrap_or_default();
        let duration_str = ev.duration.map(|d| d.to_string()).unwrap_or_default();

        let _ = wtr.write_record(&[
            ev.id.to_string(),
            ev.rule_id.to_string(),
            ev.rule_name.clone(),
            ev.service_type.clone(),
            ev.channel_id.to_string(),
            ev.data_type.clone(),
            ev.point_id.to_string(),
            ev.warning_level.to_string(),
            ev.operator.clone(),
            ev.threshold_value.to_string(),
            ev.trigger_value.map(|v| v.to_string()).unwrap_or_default(),
            ev.recovery_value.map(|v| v.to_string()).unwrap_or_default(),
            ev.event_type.clone(),
            triggered_str,
            recovered_str,
            duration_str,
        ]);
    }

    match wtr.into_inner() {
        Ok(bytes) => (
            StatusCode::OK,
            [
                (header::CONTENT_TYPE, "text/csv; charset=utf-8"),
                (
                    header::CONTENT_DISPOSITION,
                    "attachment; filename=\"alert_events.csv\"",
                ),
            ],
            bytes,
        )
            .into_response(),
        Err(e) => {
            error!("csv flush: {}", e);
            server_error("Export failed")
        },
    }
}

// ============================================================================
// Statistics & monitor
// ============================================================================

/// Aggregate alert statistics for dashboards.
///
/// Returns counts by warning level, by service_type, today vs this-week
/// totals, etc. — whatever `db::get_statistics` happens to roll up. The UI
/// uses this for the alarm overview cards on the home page.
#[utoipa::path(get, path = "/alarmApi/alert-statistics", tag = "Monitor",
    responses((status = 200, description = "Alert statistics")))]
async fn alert_statistics(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    match db::get_statistics(&state.db).await {
        Ok(stats) => Json(ApiResponse::ok("Statistics retrieved", stats)).into_response(),
        Err(e) => {
            error!("alert_statistics: {}", e);
            server_error("Failed to get statistics")
        },
    }
}

/// Monitor loop liveness and configuration snapshot.
///
/// Returns `running` (is the polling task alive), `last_check_time` (epoch
/// seconds of the most recent successful `check_all_rules` pass),
/// `check_interval` (configured `data_fetch_interval`).
/// Use this to verify alarm is actually evaluating rules rather than
/// silently hung — `running=true` + `last_check_time` stale by N×interval
/// is the diagnostic signal.
#[utoipa::path(get, path = "/alarmApi/monitor/status", tag = "Monitor",
    responses((status = 200, description = "Monitor loop status", body = MonitorStatus)))]
async fn monitor_status(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let ms = state.monitor_status.read().await.clone();
    Json(ApiResponse::ok(
        "Monitor status retrieved",
        json!({
            "running": ms.running,
            "last_check_time": ms.last_check_time,
            "check_interval": ms.check_interval,
        }),
    ))
}

/// Manually trigger a single rule evaluation (debug helper).
///
/// Resolves and reads the rule's current SHM/health target, runs `evaluate()`, and returns
/// the value, threshold comparison, and whether an active alert currently
/// exists — without going through the normal poll loop. Does NOT
/// create / resolve alerts; this is a read-only diagnostic.
///
/// Useful for debugging "why isn't my rule firing" without waiting for the
/// next monitor tick, and for verifying target resolution after configuring a
/// new rule.
#[utoipa::path(post, path = "/alarmApi/monitor/check-rule/{id}", tag = "Monitor",
    params(("id" = i64, Path, description = "Rule ID")),
    responses(
        (status = 200, description = "Manual check result"),
        (status = 404, description = "Rule not found"),
    ))]
async fn manual_check_rule(
    State(state): State<Arc<AppState>>,
    Path(id): Path<i64>,
) -> impl IntoResponse {
    match monitor::manual_check_rule(&state, id).await {
        Ok(result) => Json(result).into_response(),
        Err(e) => {
            error!("manual_check_rule: {}", e);
            Json(json!({
                "success": false,
                "message": format!("Check failed: {}", e),
                "data": {},
            }))
            .into_response()
        },
    }
}

/// Rebroadcast all currently active alerts to the WebSocket.
///
/// Doesn't change any state — re-publishes the current active alert set on
/// the broadcast channel and refreshes the alarm-count counter. Used when a
/// frontend client reconnects or wakes up after sleep and needs to catch up
/// without polling individual endpoints.
#[utoipa::path(post, path = "/alarmApi/call-data", tag = "Monitor",
    responses((status = 200, description = "Broadcast all active alerts")))]
async fn call_data(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let alerts = match db::get_all_active_alerts(&state.db).await {
        Ok(a) => a,
        Err(e) => {
            error!("call_data get alerts: {}", e);
            return server_error("Failed to get alerts");
        },
    };

    if alerts.is_empty() {
        if let Ok(counts) = db::get_active_alarm_counts(&state.db).await {
            state.broadcaster.send_alarm_count(&counts).await;
        }
        return Json(ApiResponse::ok(
            "No active alerts",
            json!({ "broadcast_count": 0, "alarm_count": 0 }),
        ))
        .into_response();
    }

    let mut rule_map: HashMap<i64, crate::models::AlertRule> = HashMap::new();
    for alert in &alerts {
        if !rule_map.contains_key(&alert.rule_id)
            && let Ok(Some(rule)) = db::get_rule_by_id(&state.db, alert.rule_id).await
        {
            rule_map.insert(rule.id, rule);
        }
    }

    let alarm_count = alerts.len();
    state
        .broadcaster
        .broadcast_active_alerts(&alerts, &rule_map)
        .await;

    if let Ok(counts) = db::get_active_alarm_counts(&state.db).await {
        state.broadcaster.send_alarm_count(&counts).await;
    }

    Json(ApiResponse::ok(
        format!("Broadcast complete: {} alert(s)", alarm_count),
        json!({
            "broadcast_count": alarm_count,
            "alarm_count": alarm_count,
        }),
    ))
    .into_response()
}

// ============================================================================
// Helpers
// ============================================================================

fn is_valid_operator(op: &str) -> bool {
    matches!(op, ">" | "<" | ">=" | "<=" | "==" | "!=")
}

/// Reject rules whose shape looks like a channel-online sentinel but with a
/// non-zero `point_id` — `point_id` is ignored for online rules (the health
/// entry is selected by `channel_id`), so a non-zero value misleads operators into
/// thinking they bound the rule to a specific point.
fn validate_channel_online_shape(
    service_type: &str,
    data_type: &str,
    point_id: i64,
) -> Result<(), String> {
    let is_online = service_type == "io" && data_type == AlertRule::CHANNEL_ONLINE_DATA_TYPE;
    if is_online && point_id != 0 {
        return Err(format!(
            "Channel online rules (data_type=\"{}\") ignore point_id; pass point_id=0 (got {})",
            AlertRule::CHANNEL_ONLINE_DATA_TYPE,
            point_id,
        ));
    }
    Ok(())
}

fn format_timestamp(ts: i64) -> String {
    chrono::Local
        .timestamp_opt(ts, 0)
        .single()
        .map(|dt| dt.format("%Y-%m-%d %H:%M:%S").to_string())
        .unwrap_or_default()
}

fn not_found(msg: &str) -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(json!({ "success": false, "message": msg, "data": null })),
    )
        .into_response()
}

fn bad_request(msg: &str) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({ "success": false, "message": msg, "data": null })),
    )
        .into_response()
}

fn server_error(msg: &str) -> Response {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({ "success": false, "message": msg, "data": null })),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_online_shape_rejects_nonzero_point_id() {
        let err = validate_channel_online_shape("io", "online", 5).unwrap_err();
        assert!(err.contains("ignore point_id"), "actual: {err}");
    }

    #[test]
    fn channel_online_shape_accepts_zero_point_id() {
        assert!(validate_channel_online_shape("io", "online", 0).is_ok());
    }

    #[test]
    fn channel_online_shape_only_applies_to_io_service_type() {
        // "inst:online" is a regular (if odd) rule; point_id is meaningful
        // there, so don't reject it.
        assert!(validate_channel_online_shape("inst", "online", 5).is_ok());
    }

    #[test]
    fn channel_online_shape_ignores_regular_data_types() {
        // A normal channel-telemetry rule must not get the sentinel check.
        assert!(validate_channel_online_shape("io", "T", 5).is_ok());
    }
}
