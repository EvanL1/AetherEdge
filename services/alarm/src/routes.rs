//! HTTP route handlers for the alarm service

use std::collections::HashMap;
use std::sync::Arc;

use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, patch, post},
};
use chrono::TimeZone;
use serde_json::{Value, json};
use tracing::error;
#[cfg(feature = "swagger-ui")]
use utoipa::OpenApi;
#[cfg(feature = "swagger-ui")]
use utoipa_swagger_ui::{Config, SwaggerUi};

use crate::db::{self};
#[cfg(feature = "swagger-ui")]
use crate::models::AlertEvent;
use crate::models::{
    AlertQueryParams, AlertResolutionData, AlertRule, ApiResponse, CompletionAuditData,
    CreateRuleData, CreateRuleRequest, EventQueryParams, MonitorStatus, RuleIdData,
    RuleQueryParams, SingleItemData, UpdateRuleRequest,
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

#[cfg(feature = "swagger-ui")]
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
        common::admin_api::get_log_level,
        common::admin_api::set_log_level,
        common::admin_api::list_log_files,
        common::admin_api::view_log_file,
    ),
    components(schemas(
        AlertRule,
        crate::models::Alert,
        AlertEvent,
        CreateRuleRequest,
        UpdateRuleRequest,
        MonitorStatus,
        CreateRuleData,
        RuleIdData,
        AlertResolutionData,
        CompletionAuditData,
        SingleItemData<AlertRule>,
        SingleItemData<crate::models::Alert>,
        ApiResponse<CreateRuleData>,
        ApiResponse<SingleItemData<AlertRule>>,
        ApiResponse<RuleIdData>,
        ApiResponse<AlertResolutionData>,
        ApiResponse<SingleItemData<crate::models::Alert>>,
        ApiResponse<MonitorStatus>,
        common::admin_api::SetLogLevelRequest,
        common::admin_api::LogLevelResponse,
    )),
    tags(
        (name = "Rules",   description = "Alarm rule CRUD"),
        (name = "Alerts",  description = "Active alert query and resolution"),
        (name = "Events",  description = "Alert event history and export"),
        (name = "Monitor", description = "Monitor status and manual trigger"),
        (name = "Meta",    description = "Service info"),
        (name = "admin",   description = "Host-local service administration"),
    ),
    modifiers(&SecurityAddon),
    info(
        title = "Aether Alarm Service API",
        version = env!("CARGO_PKG_VERSION"),
        description = "Internal loopback API for alarm rules, active alerts, event history, and monitoring. Do not expose this service port remotely; use an authenticated ingress for remote operations."
    )
)]
pub struct ApiDoc;

#[cfg(feature = "swagger-ui")]
struct SecurityAddon;

#[cfg(feature = "swagger-ui")]
impl utoipa::Modify for SecurityAddon {
    fn modify(&self, openapi: &mut utoipa::openapi::OpenApi) {
        if let Some(components) = openapi.components.as_mut() {
            components.add_security_scheme(
                "bearer_auth",
                utoipa::openapi::security::SecurityScheme::Http(
                    utoipa::openapi::security::HttpBuilder::new()
                        .scheme(utoipa::openapi::security::HttpAuthScheme::Bearer)
                        .bearer_format("JWT")
                        .description(Some("Signed Aether access token"))
                        .build(),
                ),
            );
        }
    }
}

#[cfg(all(test, feature = "swagger-ui"))]
mod openapi_tests {
    use super::*;

    fn specification() -> Value {
        serde_json::to_value(ApiDoc::openapi()).expect("serialize OpenAPI")
    }

    fn resolve_schema<'a>(specification: &'a Value, schema: &'a Value) -> &'a Value {
        match schema["$ref"].as_str() {
            Some(reference) => specification
                .pointer(reference.trim_start_matches('#'))
                .unwrap_or_else(|| panic!("missing schema reference {reference}")),
            None => schema,
        }
    }

    fn assert_enveloped_response_data(
        specification: &Value,
        path: &str,
        method: &str,
        expected_data_properties: &[&str],
    ) {
        let response_schema = &specification["paths"][path][method]["responses"]["200"]["content"]
            ["application/json"]["schema"];
        let envelope = resolve_schema(specification, response_schema);
        for property in ["success", "message", "data"] {
            assert!(
                envelope["properties"][property].is_object(),
                "{method} {path} response is missing envelope property {property}"
            );
        }

        let data = resolve_schema(specification, &envelope["properties"]["data"]);
        for property in expected_data_properties {
            assert!(
                data["properties"][property].is_object(),
                "{method} {path} response data is missing property {property}"
            );
        }
    }

    #[test]
    fn openapi_metadata_and_admin_routes_match_the_router() {
        let specification = specification();
        assert_eq!(specification["info"]["title"], "Aether Alarm Service API");
        assert_eq!(specification["info"]["version"], env!("CARGO_PKG_VERSION"));
        for (path, method) in [
            ("/api/admin/logs/level", "get"),
            ("/api/admin/logs/level", "post"),
            ("/api/admin/logs/files", "get"),
            ("/api/admin/logs/view", "get"),
        ] {
            assert!(
                specification["paths"][path][method].is_object(),
                "missing {method} {path}"
            );
        }
        let operation_count = specification["paths"]
            .as_object()
            .expect("paths object")
            .values()
            .map(|item| {
                item.as_object()
                    .expect("path item")
                    .keys()
                    .filter(|method| {
                        matches!(
                            method.as_str(),
                            "get"
                                | "put"
                                | "post"
                                | "delete"
                                | "patch"
                                | "options"
                                | "head"
                                | "trace"
                        )
                    })
                    .count()
            })
            .sum::<usize>();
        assert_eq!(operation_count, 23, "Router/OpenAPI operation drift");

        assert!(
            specification["components"]["securitySchemes"]["bearer_auth"].is_object(),
            "alarm rule commands must document Bearer authentication"
        );
        for (path, method) in [
            ("/alarmApi/rules", "post"),
            ("/alarmApi/rules/{id}", "put"),
            ("/alarmApi/rules/{id}", "delete"),
            ("/alarmApi/rules/{id}/enable", "patch"),
            ("/alarmApi/rules/{id}/disable", "patch"),
            ("/alarmApi/alerts/{id}/resolve", "patch"),
        ] {
            assert!(
                specification["paths"][path][method]["security"].is_array(),
                "{method} {path} must document Bearer authentication"
            );
            let parameters = specification["paths"][path][method]["parameters"]
                .as_array()
                .unwrap_or_else(|| panic!("{method} {path} must document command headers"));
            for expected in ["x-request-id", "x-aether-confirmed"] {
                assert!(
                    parameters
                        .iter()
                        .any(|parameter| parameter["name"] == expected),
                    "{method} {path} is missing {expected}"
                );
            }
            let forbidden = specification["paths"][path][method]["responses"]["403"]["description"]
                .as_str()
                .unwrap_or_default();
            assert!(
                forbidden.contains("Missing/invalid Bearer credentials"),
                "{method} {path} must describe authentication and authorization failure"
            );
        }
        for (path, method) in [
            ("/alarmApi/rules/{id}", "delete"),
            ("/alarmApi/rules/{id}/enable", "patch"),
            ("/alarmApi/rules/{id}/disable", "patch"),
            ("/alarmApi/alerts/{id}/resolve", "patch"),
        ] {
            assert!(
                specification["paths"][path][method]["responses"]["400"].is_object(),
                "{method} {path} must document invalid non-positive identifiers"
            );
        }
    }

    #[test]
    fn openapi_rule_alert_and_monitor_responses_match_wire_envelopes() {
        let specification = specification();

        assert_enveloped_response_data(
            &specification,
            "/alarmApi/rules",
            "post",
            &[
                "rule_id",
                "rule_name",
                "logical_key",
                "point_id",
                "monitoring",
                "rule",
                "request_id",
                "audit",
            ],
        );
        assert_enveloped_response_data(
            &specification,
            "/alarmApi/rules/{id}",
            "get",
            &["total", "list"],
        );
        assert_enveloped_response_data(
            &specification,
            "/alarmApi/rules/{id}",
            "put",
            &["rule_id", "request_id", "audit"],
        );
        assert_enveloped_response_data(
            &specification,
            "/alarmApi/alerts/{id}",
            "get",
            &["total", "list"],
        );
        assert_enveloped_response_data(
            &specification,
            "/alarmApi/alerts/{id}/resolve",
            "patch",
            &[
                "alert_id",
                "rule_id",
                "resolved_at_ms",
                "request_id",
                "audit",
            ],
        );
        assert_enveloped_response_data(
            &specification,
            "/alarmApi/monitor/status",
            "get",
            &["running", "last_check_time", "check_interval"],
        );

        assert!(
            specification["paths"]["/alarmApi/rules/{id}"]["put"]["responses"]["409"].is_object(),
            "update rule must document its duplicate-name conflict"
        );
        for status in ["404", "500"] {
            assert!(
                specification["paths"]["/alarmApi/monitor/check-rule/{id}"]["post"]["responses"]
                    [status]
                    .is_object(),
                "manual rule check must document HTTP {status}"
            );
        }
    }
}

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
            "description": "Aether alarm service (Rust)",
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
    params(
        ("x-request-id" = Option<String>, Header, description = "Optional UUID audit correlation ID"),
        ("x-aether-confirmed" = bool, Header, description = "Required explicit confirmation; must be true")
    ),
    responses(
        (status = 200, description = "Rule created; accepted commands are never automatically retryable", body = ApiResponse<CreateRuleData>),
        (status = 400, description = "Invalid rule definition"),
        (status = 403, description = "Missing/invalid Bearer credentials or actor lacks alarm.rule.manage"),
        (status = 409, description = "Duplicate rule"),
        (status = 422, description = "Explicit confirmation is required"),
        (status = 503, description = "Mandatory pre-mutation audit or rule storage is unavailable")
    ),
    security(("bearer_auth" = []))
)]
async fn create_rule(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(req): Json<CreateRuleRequest>,
) -> impl IntoResponse {
    let target = match parse_alarm_target(
        &req.service_type,
        req.channel_id,
        &req.data_type,
        req.point_id,
    ) {
        Ok(target) => target,
        Err(message) => return bad_request(&message),
    };
    let severity = match aether_domain::AlarmSeverity::new(req.warning_level) {
        Ok(severity) => severity,
        Err(error) => return bad_request(&error.to_string()),
    };
    let comparator = match aether_domain::AlarmComparator::try_from(req.operator.as_str()) {
        Ok(comparator) => comparator,
        Err(_) => return bad_request("Invalid operator. Allowed: >, <, >=, <=, ==, !="),
    };
    let response_name = req.rule_name.clone();
    let response_point_id = req.point_id;
    let response_enabled = req.enabled;
    let definition = match aether_domain::AlarmRuleDefinition::new(
        target,
        req.rule_name,
        severity,
        comparator,
        req.value,
        req.enabled,
        req.description,
    ) {
        Ok(definition) => definition,
        Err(error) => return bad_request(&error.to_string()),
    };
    let acceptance = match apply_alarm_mutation(
        &state,
        &headers,
        aether_ports::AlarmRuleMutation::create(definition),
    )
    .await
    {
        Ok(acceptance) => acceptance,
        Err(response) => return response,
    };
    let id = acceptance.rule_id().get();
    let rule = match i64::try_from(id) {
        Ok(storage_id) => db::get_rule_by_id(&state.db, storage_id)
            .await
            .ok()
            .flatten(),
        Err(_) => None,
    };
    let logical_key = rule.as_ref().map(AlertRule::logical_key);
    log_incomplete_alarm_audit(&acceptance);
    let audit = completion_audit_data(acceptance.completion_audit());
    Json(ApiResponse::ok(
        format!("Rule '{response_name}' created"),
        CreateRuleData {
            rule_id: id,
            rule_name: response_name,
            logical_key,
            point_id: response_point_id,
            monitoring: response_enabled,
            rule,
            request_id: acceptance.request_id().to_string(),
            audit,
        },
    ))
    .into_response()
}

/// Get one alarm rule by its primary key.
///
/// Response wraps the rule in `{ total: 1, list: [rule] }` for
/// compatibility with the legacy Python-era frontend that consumed
/// `data.list[0]`. Use `list_rules` for multi-rule queries.
#[utoipa::path(get, path = "/alarmApi/rules/{id}", tag = "Rules",
    params(("id" = i64, Path, description = "Rule ID")),
    responses(
        (status = 200, description = "Rule detail", body = ApiResponse<SingleItemData<AlertRule>>),
        (status = 404, description = "Rule not found"),
    ))]
async fn get_rule(State(state): State<Arc<AppState>>, Path(id): Path<i64>) -> impl IntoResponse {
    match db::get_rule_by_id(&state.db, id).await {
        Ok(Some(rule)) => {
            // Return list format for compatibility with alarm-py (data.list[0])
            Json(ApiResponse::ok(
                "Rule retrieved",
                SingleItemData {
                    total: 1,
                    list: vec![rule],
                },
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
/// written. After a successful update, the governed mutation adapter reconciles
/// active monitoring state; if
/// the rule's `enabled` flag flipped to false, its active alerts are
/// resolved with reason "rule disabled" (stored as the Chinese literal "规则被禁用" in alert records).
///
/// If the patch touches `service_type` / `data_type` / `point_id`, the
/// resulting tuple is re-validated against the channel-online sentinel
/// shape (see POST), so partial updates can't sneak a malformed rule
/// through.
#[utoipa::path(put, path = "/alarmApi/rules/{id}", tag = "Rules",
    params(
        ("id" = i64, Path, description = "Rule ID"),
        ("x-request-id" = Option<String>, Header, description = "Optional UUID audit correlation ID"),
        ("x-aether-confirmed" = bool, Header, description = "Required explicit confirmation; must be true")
    ),
    request_body = UpdateRuleRequest,
    responses(
        (status = 200, description = "Rule updated; accepted commands are never automatically retryable", body = ApiResponse<RuleIdData>),
        (status = 400, description = "Invalid rule definition"),
        (status = 403, description = "Missing/invalid Bearer credentials or actor lacks alarm.rule.manage"),
        (status = 404, description = "Rule not found"),
        (status = 409, description = "Duplicate rule name"),
        (status = 422, description = "Explicit confirmation is required or the patch is empty"),
        (status = 503, description = "Mandatory pre-mutation audit or rule storage is unavailable")
    ),
    security(("bearer_auth" = []))
)]
async fn update_rule(
    State(state): State<Arc<AppState>>,
    Path(id): Path<i64>,
    headers: HeaderMap,
    Json(req): Json<UpdateRuleRequest>,
) -> impl IntoResponse {
    let rule_id = match parse_alarm_rule_id(id) {
        Ok(rule_id) => rule_id,
        Err(message) => return bad_request(message),
    };
    let target = if req.service_type.is_some()
        || req.channel_id.is_some()
        || req.data_type.is_some()
        || req.point_id.is_some()
    {
        let existing = match db::get_rule_by_id(&state.db, id).await {
            Ok(Some(existing)) => existing,
            Ok(None) => return not_found("Rule not found"),
            Err(error) => {
                error!("update_rule target lookup: {error}");
                return server_error("Failed to update rule");
            },
        };
        match parse_alarm_target(
            req.service_type
                .as_deref()
                .unwrap_or(&existing.service_type),
            req.channel_id.unwrap_or(existing.channel_id),
            req.data_type.as_deref().unwrap_or(&existing.data_type),
            req.point_id.unwrap_or(existing.point_id),
        ) {
            Ok(target) => Some(target),
            Err(message) => return bad_request(&message),
        }
    } else {
        None
    };
    let severity = match req.warning_level.map(aether_domain::AlarmSeverity::new) {
        Some(Ok(severity)) => Some(severity),
        Some(Err(error)) => return bad_request(&error.to_string()),
        None => None,
    };
    let comparator = match req
        .operator
        .as_deref()
        .map(aether_domain::AlarmComparator::try_from)
    {
        Some(Ok(comparator)) => Some(comparator),
        Some(Err(_)) => return bad_request("Invalid operator. Allowed: >, <, >=, <=, ==, !="),
        None => None,
    };
    let patch = match aether_ports::AlarmRulePatch::new(
        target,
        req.rule_name,
        severity,
        comparator,
        req.value,
        req.enabled,
        req.description.map(Some),
    ) {
        Ok(patch) => patch,
        Err(error) => return bad_request(&error.to_string()),
    };
    match apply_alarm_mutation(
        &state,
        &headers,
        aether_ports::AlarmRuleMutation::update(rule_id, patch),
    )
    .await
    {
        Ok(acceptance) => mutation_success("Rule updated", &acceptance),
        Err(response) => response,
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
    params(
        ("id" = i64, Path, description = "Rule ID"),
        ("x-request-id" = Option<String>, Header, description = "Optional UUID audit correlation ID"),
        ("x-aether-confirmed" = bool, Header, description = "Required explicit confirmation; must be true")
    ),
    responses(
        (status = 200, description = "Rule deleted; accepted commands are never automatically retryable", body = ApiResponse<RuleIdData>),
        (status = 400, description = "Rule ID must be greater than zero"),
        (status = 403, description = "Missing/invalid Bearer credentials or actor lacks alarm.rule.manage"),
        (status = 404, description = "Rule not found"),
        (status = 422, description = "Explicit confirmation is required"),
        (status = 503, description = "Mandatory pre-mutation audit or rule storage is unavailable")
    ),
    security(("bearer_auth" = []))
)]
async fn delete_rule(
    State(state): State<Arc<AppState>>,
    Path(id): Path<i64>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let rule_id = match parse_alarm_rule_id(id) {
        Ok(rule_id) => rule_id,
        Err(message) => return bad_request(message),
    };
    match apply_alarm_mutation(
        &state,
        &headers,
        aether_ports::AlarmRuleMutation::delete(rule_id),
    )
    .await
    {
        Ok(acceptance) => mutation_success("Rule deleted", &acceptance),
        Err(response) => response,
    }
}

/// Enable a rule (set `enabled=true`).
///
/// The rule joins the polling loop on the next tick. Convenience shortcut
/// over PUT with `{"enabled": true}`.
#[utoipa::path(patch, path = "/alarmApi/rules/{id}/enable", tag = "Rules",
    params(
        ("id" = i64, Path, description = "Rule ID"),
        ("x-request-id" = Option<String>, Header, description = "Optional UUID audit correlation ID"),
        ("x-aether-confirmed" = bool, Header, description = "Required explicit confirmation; must be true")
    ),
    responses(
        (status = 200, description = "Rule enabled; accepted commands are never automatically retryable", body = ApiResponse<RuleIdData>),
        (status = 400, description = "Rule ID must be greater than zero"),
        (status = 403, description = "Missing/invalid Bearer credentials or actor lacks alarm.rule.manage"),
        (status = 404, description = "Rule not found"),
        (status = 422, description = "Explicit confirmation is required"),
        (status = 503, description = "Mandatory pre-mutation audit or rule storage is unavailable")
    ),
    security(("bearer_auth" = []))
)]
async fn enable_rule(
    State(state): State<Arc<AppState>>,
    Path(id): Path<i64>,
    headers: HeaderMap,
) -> impl IntoResponse {
    set_rule_enabled(&state, &headers, id, true).await
}

/// Disable a rule (set `enabled=false`).
///
/// Stops the monitor from evaluating this rule on the next tick AND resolves
/// any currently-active alerts produced by it (reason "rule disabled", stored as "规则被禁用"), so the
/// UI clears stale alerts immediately rather than waiting for them to age
/// out. Convenience over PUT with `{"enabled": false}` plus the side effect.
#[utoipa::path(patch, path = "/alarmApi/rules/{id}/disable", tag = "Rules",
    params(
        ("id" = i64, Path, description = "Rule ID"),
        ("x-request-id" = Option<String>, Header, description = "Optional UUID audit correlation ID"),
        ("x-aether-confirmed" = bool, Header, description = "Required explicit confirmation; must be true")
    ),
    responses(
        (status = 200, description = "Rule disabled; accepted commands are never automatically retryable", body = ApiResponse<RuleIdData>),
        (status = 400, description = "Rule ID must be greater than zero"),
        (status = 403, description = "Missing/invalid Bearer credentials or actor lacks alarm.rule.manage"),
        (status = 404, description = "Rule not found"),
        (status = 422, description = "Explicit confirmation is required"),
        (status = 503, description = "Mandatory pre-mutation audit or rule storage is unavailable")
    ),
    security(("bearer_auth" = []))
)]
async fn disable_rule(
    State(state): State<Arc<AppState>>,
    Path(id): Path<i64>,
    headers: HeaderMap,
) -> impl IntoResponse {
    set_rule_enabled(&state, &headers, id, false).await
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
        (status = 200, description = "Alert detail", body = ApiResponse<SingleItemData<crate::models::Alert>>),
        (status = 404, description = "Alert not found"),
    ))]
async fn get_alert(State(state): State<Arc<AppState>>, Path(id): Path<i64>) -> impl IntoResponse {
    match db::get_alert_by_id(&state.db, id).await {
        Ok(Some(alert)) => {
            // Return list format for compatibility with alarm-py (data.list[0])
            Json(ApiResponse::ok(
                "Alert retrieved",
                SingleItemData {
                    total: 1,
                    list: vec![alert],
                },
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
    params(
        ("id" = i64, Path, description = "Alert ID"),
        ("x-request-id" = Option<String>, Header, description = "Optional UUID audit correlation ID"),
        ("x-aether-confirmed" = bool, Header, description = "Required explicit confirmation; must be true")
    ),
    responses(
        (status = 200, description = "Alert resolved; accepted commands are never automatically retryable", body = ApiResponse<AlertResolutionData>),
        (status = 400, description = "Alert ID must be greater than zero"),
        (status = 403, description = "Missing/invalid Bearer credentials or actor lacks alarm.alert.resolve"),
        (status = 404, description = "Alert not found"),
        (status = 422, description = "Explicit confirmation is required"),
        (status = 503, description = "Mandatory pre-resolution audit or alarm storage is unavailable")
    ),
    security(("bearer_auth" = []))
)]
async fn resolve_alert(
    State(state): State<Arc<AppState>>,
    Path(id): Path<i64>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let alert_id = match u64::try_from(id) {
        Ok(id) if id > 0 => aether_domain::AlertId::new(id),
        _ => return bad_request("alert id must be greater than zero"),
    };
    match apply_alert_resolution(&state, &headers, alert_id).await {
        Ok(acceptance) => Json(ApiResponse::ok(
            "Alert resolved",
            AlertResolutionData {
                alert_id: acceptance.alert_id().get(),
                rule_id: acceptance.rule_id().get(),
                resolved_at_ms: acceptance.resolved_at().get(),
                request_id: acceptance.request_id().to_string(),
                audit: completion_audit_data(acceptance.completion_audit()),
            },
        ))
        .into_response(),
        Err(response) => response,
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
    responses((status = 200, description = "Monitor loop status", body = ApiResponse<MonitorStatus>)))]
async fn monitor_status(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let ms = state.monitor_status.read().await.clone();
    Json(ApiResponse::ok("Monitor status retrieved", ms))
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
        (status = 500, description = "Manual check failed"),
    ))]
async fn manual_check_rule(
    State(state): State<Arc<AppState>>,
    Path(id): Path<i64>,
) -> impl IntoResponse {
    match monitor::manual_check_rule(&state, id).await {
        Ok(result) => Json(result).into_response(),
        Err(monitor::ManualCheckError::RuleNotFound) => not_found("Rule not found"),
        Err(monitor::ManualCheckError::Internal(e)) => {
            error!("manual_check_rule: {}", e);
            server_error("Manual rule check failed")
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

fn parse_alarm_target(
    service_type: &str,
    channel_id: i64,
    data_type: &str,
    point_id: i64,
) -> Result<aether_domain::AlarmRuleTarget, String> {
    validate_channel_online_shape(service_type, data_type, point_id)?;
    let channel_id = u32::try_from(channel_id)
        .map(aether_domain::ChannelId::new)
        .map_err(|_| "channel_id must be between 0 and 4294967295".to_string())?;
    if service_type == "io" && data_type == AlertRule::CHANNEL_ONLINE_DATA_TYPE {
        return Ok(aether_domain::AlarmRuleTarget::channel_online(channel_id));
    }
    let point_id = u32::try_from(point_id)
        .map(aether_domain::PointId::new)
        .map_err(|_| "point_id must be between 0 and 4294967295".to_string())?;
    aether_domain::AlarmRuleTarget::point(service_type, channel_id, data_type, point_id)
        .map_err(|error| error.to_string())
}

fn parse_alarm_rule_id(id: i64) -> Result<aether_domain::AlarmRuleId, &'static str> {
    u64::try_from(id)
        .ok()
        .filter(|id| *id > 0)
        .map(aether_domain::AlarmRuleId::new)
        .ok_or("alarm rule id must be greater than zero")
}

async fn set_rule_enabled(
    state: &Arc<AppState>,
    headers: &HeaderMap,
    id: i64,
    enabled: bool,
) -> Response {
    let rule_id = match parse_alarm_rule_id(id) {
        Ok(rule_id) => rule_id,
        Err(message) => return bad_request(message),
    };
    match apply_alarm_mutation(
        state,
        headers,
        aether_ports::AlarmRuleMutation::set_enabled(rule_id, enabled),
    )
    .await
    {
        Ok(acceptance) => mutation_success(
            if enabled {
                "Rule enabled"
            } else {
                "Rule disabled"
            },
            &acceptance,
        ),
        Err(response) => response,
    }
}

async fn apply_alarm_mutation(
    state: &AppState,
    headers: &HeaderMap,
    mutation: aether_ports::AlarmRuleMutation,
) -> Result<aether_application::AlarmRuleMutationAcceptance, Response> {
    let authorization = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok());
    let request_id = headers
        .get("x-request-id")
        .and_then(|value| value.to_str().ok());
    let confirmed = headers
        .get("x-aether-confirmed")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.trim().eq_ignore_ascii_case("true"));
    let timestamp = chrono::Utc::now().timestamp_millis().max(0) as u64;
    let invocation = state.access_authenticator.invocation(
        authorization,
        request_id,
        confirmed,
        aether_domain::TimestampMs::new(timestamp),
    );
    state
        .rule_application
        .mutate(invocation.context(), mutation)
        .await
        .map_err(application_error_response)
}

async fn apply_alert_resolution(
    state: &AppState,
    headers: &HeaderMap,
    alert_id: aether_domain::AlertId,
) -> Result<aether_application::AlertResolutionAcceptance, Response> {
    let authorization = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok());
    let request_id = headers
        .get("x-request-id")
        .and_then(|value| value.to_str().ok());
    let confirmed = headers
        .get("x-aether-confirmed")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.trim().eq_ignore_ascii_case("true"));
    let timestamp = chrono::Utc::now().timestamp_millis().max(0) as u64;
    let invocation = state.access_authenticator.invocation(
        authorization,
        request_id,
        confirmed,
        aether_domain::TimestampMs::new(timestamp),
    );
    let acceptance = state
        .alert_resolution_application
        .resolve(invocation.context(), alert_id)
        .await
        .map_err(application_error_response)?;
    if let Some(failure) = acceptance.completion_audit().failure() {
        error!(
            request_id = acceptance.request_id(),
            alert_id = acceptance.alert_id().get(),
            error = %failure,
            "alert resolution was accepted but its terminal audit is incomplete; do not retry"
        );
    }
    Ok(acceptance)
}

fn mutation_success(
    message: &str,
    acceptance: &aether_application::AlarmRuleMutationAcceptance,
) -> Response {
    log_incomplete_alarm_audit(acceptance);
    Json(ApiResponse::ok(
        message,
        RuleIdData {
            rule_id: acceptance.rule_id().get(),
            request_id: acceptance.request_id().to_string(),
            audit: completion_audit_data(acceptance.completion_audit()),
        },
    ))
    .into_response()
}

fn log_incomplete_alarm_audit(acceptance: &aether_application::AlarmRuleMutationAcceptance) {
    if let Some(failure) = acceptance.completion_audit().failure() {
        error!(
            request_id = acceptance.request_id(),
            rule_id = acceptance.rule_id().get(),
            error = %failure,
            "alarm rule mutation was accepted but its terminal audit is incomplete; do not retry"
        );
    }
}

fn completion_audit_data(
    status: &aether_application::CompletionAuditStatus,
) -> CompletionAuditData {
    match status {
        aether_application::CompletionAuditStatus::Recorded => CompletionAuditData {
            status: "recorded".to_string(),
            retryable: false,
            message: None,
        },
        aether_application::CompletionAuditStatus::Incomplete { .. } => CompletionAuditData {
            status: "incomplete".to_string(),
            retryable: false,
            message: Some(
                "operation was accepted but its terminal audit is incomplete; do not retry"
                    .to_string(),
            ),
        },
    }
}

fn application_error_response(error: aether_application::ApplicationError) -> Response {
    match error {
        error @ aether_application::ApplicationError::PermissionDenied { .. } => {
            error_response(StatusCode::FORBIDDEN, &error.to_string())
        },
        aether_application::ApplicationError::ConfirmationRequired { .. } => error_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            "Explicit confirmation is required (x-aether-confirmed: true)",
        ),
        aether_application::ApplicationError::AuditUnavailable(error) => {
            tracing::error!(error = %error, "mandatory alarm mutation audit unavailable");
            error_response(
                StatusCode::SERVICE_UNAVAILABLE,
                "Mandatory alarm mutation audit is unavailable",
            )
        },
        aether_application::ApplicationError::Port(error) => {
            use aether_ports::PortErrorKind;
            match error.kind() {
                PortErrorKind::NotFound => error_response(StatusCode::NOT_FOUND, error.message()),
                PortErrorKind::Rejected | PortErrorKind::Conflict => {
                    error_response(StatusCode::CONFLICT, error.message())
                },
                PortErrorKind::InvalidData => {
                    error_response(StatusCode::UNPROCESSABLE_ENTITY, error.message())
                },
                PortErrorKind::Unavailable | PortErrorKind::Timeout => {
                    tracing::error!(error = %error, "alarm mutation storage unavailable");
                    error_response(
                        StatusCode::SERVICE_UNAVAILABLE,
                        "Alarm rule storage is unavailable",
                    )
                },
                PortErrorKind::Permanent => {
                    tracing::error!(error = %error, "permanent alarm mutation adapter failure");
                    server_error("Failed to mutate alarm rule")
                },
            }
        },
        other => {
            tracing::error!(error = %other, "unexpected alarm mutation application failure");
            server_error("Failed to mutate alarm rule")
        },
    }
}

fn error_response(status: StatusCode, message: &str) -> Response {
    (
        status,
        Json(json!({ "success": false, "message": message, "data": null })),
    )
        .into_response()
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
    use aether_ports::PortResult;
    use aether_shm_bridge::SlotSnapshot;
    use axum::body::Body;
    use axum::body::to_bytes;
    use axum::http::Request;
    use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};
    use serde::Serialize;
    use tower::ServiceExt;

    const TEST_JWT_SECRET: &str = "test-only-alarm-jwt-secret-32-bytes";

    #[derive(Serialize)]
    struct AccessClaims<'a> {
        user_id: i64,
        role: &'a str,
        #[serde(rename = "type")]
        token_type: &'a str,
        exp: usize,
        iat: usize,
    }

    fn access_token() -> String {
        encode(
            &Header::new(Algorithm::HS256),
            &AccessClaims {
                user_id: 17,
                role: "Engineer",
                token_type: "access",
                exp: 4_102_444_800,
                iat: 1,
            },
            &EncodingKey::from_secret(TEST_JWT_SECRET.as_bytes()),
        )
        .expect("encode access token")
    }

    fn create_rule_request(name: &str) -> serde_json::Value {
        serde_json::json!({
            "service_type": "io",
            "channel_id": 1,
            "data_type": "T",
            "point_id": 2,
            "rule_name": name,
            "warning_level": 2,
            "operator": ">",
            "value": 80.0,
            "enabled": true
        })
    }

    #[derive(Debug)]
    struct NoLiveValues;

    impl crate::live_values::AlarmValueSource for NoLiveValues {
        fn read_rule(&self, _rule: &AlertRule) -> PortResult<Option<SlotSnapshot>> {
            Ok(None)
        }

        fn watched_slot(&self, _rule: &AlertRule) -> PortResult<Option<usize>> {
            Ok(None)
        }
    }

    async fn test_state() -> Arc<AppState> {
        let db = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("create in-memory alarm database");
        db::create_tables(&db)
            .await
            .expect("create alarm database tables");
        let config = Arc::new(crate::config::AlarmConfig::default());
        let broadcaster = crate::broadcast::Broadcaster::new(
            reqwest::Client::new(),
            "http://127.0.0.1:1".to_string(),
            "http://127.0.0.1:1".to_string(),
        );
        let alarm_store = Arc::new(crate::alarm_rule_mutation::SqliteAlarmRuleMutator::new(
            db.clone(),
            broadcaster.clone(),
        ));
        let audit: Arc<dyn aether_ports::AuditSink> =
            Arc::new(aether_store_local::MemoryAuditSink::new());
        Arc::new(AppState {
            db: db.clone(),
            live_values: Arc::new(NoLiveValues),
            broadcaster: crate::broadcast::Broadcaster::new(
                reqwest::Client::new(),
                config.api_url.clone(),
                config.uplink_url.clone(),
            ),
            monitor_status: Arc::new(tokio::sync::RwLock::new(MonitorStatus {
                running: false,
                last_check_time: None,
                check_interval: config.data_fetch_interval,
            })),
            config,
            rule_application: Arc::new(aether_application::AlarmRuleApplication::new(
                alarm_store.clone(),
                Arc::clone(&audit),
                aether_application::SafetyPolicy,
            )),
            alert_resolution_application: Arc::new(
                aether_application::AlertResolutionApplication::new(
                    alarm_store,
                    audit,
                    aether_application::SafetyPolicy,
                ),
            ),
            access_authenticator: Arc::new(
                aether_auth_jwt::AccessTokenAuthenticator::new(TEST_JWT_SECRET)
                    .expect("valid test JWT secret"),
            ),
        })
    }

    #[tokio::test]
    async fn manual_check_missing_rule_returns_http_not_found() {
        let response = manual_check_rule(State(test_state().await), Path(404))
            .await
            .into_response();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("read response body");
        let payload: Value = serde_json::from_slice(&body).expect("parse response JSON");
        assert_eq!(payload["success"], false);
        assert_eq!(payload["message"], "Rule not found");
    }

    #[tokio::test]
    async fn unauthenticated_rule_create_is_rejected_without_database_side_effect() {
        let state = test_state().await;
        let response = create_routes(Arc::clone(&state))
            .oneshot(
                Request::post("/alarmApi/rules")
                    .header("content-type", "application/json")
                    .header("x-aether-confirmed", "true")
                    .body(Body::from(
                        create_rule_request("unauthenticated").to_string(),
                    ))
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert!(
            db::find_rule_by_name(&state.db, "unauthenticated")
                .await
                .expect("query rule")
                .is_none(),
            "denied command must not reach alarm storage"
        );
    }

    #[tokio::test]
    async fn authenticated_unconfirmed_rule_create_is_rejected_without_database_side_effect() {
        let state = test_state().await;
        let response = create_routes(Arc::clone(&state))
            .oneshot(
                Request::post("/alarmApi/rules")
                    .header("content-type", "application/json")
                    .header("authorization", format!("Bearer {}", access_token()))
                    .body(Body::from(create_rule_request("unconfirmed").to_string()))
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
        assert!(
            db::find_rule_by_name(&state.db, "unconfirmed")
                .await
                .expect("query rule")
                .is_none(),
            "unconfirmed command must not reach alarm storage"
        );
    }

    #[tokio::test]
    async fn authenticated_confirmed_rule_create_uses_application_and_reports_audit_state() {
        let state = test_state().await;
        let response = create_routes(Arc::clone(&state))
            .oneshot(
                Request::post("/alarmApi/rules")
                    .header("content-type", "application/json")
                    .header("authorization", format!("Bearer {}", access_token()))
                    .header("x-request-id", "018f0000-0000-7000-8000-000000000017")
                    .header("x-aether-confirmed", "true")
                    .body(Body::from(create_rule_request("governed").to_string()))
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::OK);
        let payload: Value = serde_json::from_slice(
            &to_bytes(response.into_body(), usize::MAX)
                .await
                .expect("response body"),
        )
        .expect("response JSON");
        assert_eq!(
            payload["data"]["request_id"],
            "018f0000-0000-7000-8000-000000000017"
        );
        assert_eq!(payload["data"]["audit"]["status"], "recorded");
        assert_eq!(payload["data"]["audit"]["retryable"], false);
        assert!(
            db::find_rule_by_name(&state.db, "governed")
                .await
                .expect("query rule")
                .is_some(),
            "accepted application command must reach alarm storage exactly once"
        );
    }

    #[tokio::test]
    async fn authenticated_confirmed_alert_resolution_is_audited_and_retained() {
        let state = test_state().await;
        let create_response = create_routes(Arc::clone(&state))
            .oneshot(
                Request::post("/alarmApi/rules")
                    .header("content-type", "application/json")
                    .header("authorization", format!("Bearer {}", access_token()))
                    .header("x-aether-confirmed", "true")
                    .body(Body::from(
                        create_rule_request("resolve-source").to_string(),
                    ))
                    .expect("create request"),
            )
            .await
            .expect("create response");
        assert_eq!(create_response.status(), StatusCode::OK);
        let rule = db::find_rule_by_name(&state.db, "resolve-source")
            .await
            .expect("query rule")
            .expect("created rule");
        let alert_id = db::insert_alert(&state.db, &rule, 95.0)
            .await
            .expect("active alert");

        let response = create_routes(Arc::clone(&state))
            .oneshot(
                Request::patch(format!("/alarmApi/alerts/{alert_id}/resolve"))
                    .header("authorization", format!("Bearer {}", access_token()))
                    .header("x-request-id", "018f0000-0000-7000-8000-000000000018")
                    .header("x-aether-confirmed", "true")
                    .body(Body::empty())
                    .expect("resolve request"),
            )
            .await
            .expect("resolve response");

        assert_eq!(response.status(), StatusCode::OK);
        let payload: Value = serde_json::from_slice(
            &to_bytes(response.into_body(), usize::MAX)
                .await
                .expect("resolution body"),
        )
        .expect("resolution JSON");
        assert_eq!(payload["data"]["alert_id"], alert_id);
        assert_eq!(payload["data"]["rule_id"], rule.id);
        assert_eq!(
            payload["data"]["request_id"],
            "018f0000-0000-7000-8000-000000000018"
        );
        assert_eq!(payload["data"]["audit"]["status"], "recorded");
        assert!(
            db::get_alert_by_id(&state.db, alert_id)
                .await
                .expect("active alert lookup")
                .is_none()
        );
        let retained: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM alert_event WHERE rule_id = ? AND event_type = 'recovery'",
        )
        .bind(rule.id)
        .fetch_one(&state.db)
        .await
        .expect("retained recovery");
        assert_eq!(retained, 1);
    }

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
