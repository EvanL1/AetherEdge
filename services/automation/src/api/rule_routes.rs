//! HTTP adapter for persisted rules and deterministic execution.

#![allow(clippy::disallowed_methods)] // json! macro used in multiple functions

use crate::error::AutomationError;
use crate::infra::rule_queries::RuleQueries;
use aether_domain::RuleId;
use aether_ports::{RevisionedRuleMutation, RuleMutation};
use axum::{
    Router,
    extract::{Path, Query, State},
    http::{HeaderMap, HeaderValue, header::ETAG},
    response::{IntoResponse, Json, Response},
    routing::{get, post},
};
use common::{PaginatedResponse, SuccessResponse};
use serde_json::json;
use std::sync::Arc;
use tracing::{debug, error, info};
#[cfg(feature = "openapi")]
use utoipa::OpenApi;

/// Rule API state shared across handlers.
pub struct RuleEngineState {
    queries: Arc<RuleQueries>,
    execution_application: Arc<aether_application::RuleExecutionApplication>,
    mutation_application: Arc<aether_application::RuleMutationApplication>,
    authenticator: Arc<crate::infra::application_control::ControlAuthenticator>,
}

impl RuleEngineState {
    pub fn new(
        queries: Arc<RuleQueries>,
        execution_application: Arc<aether_application::RuleExecutionApplication>,
        mutation_application: Arc<aether_application::RuleMutationApplication>,
        authenticator: Arc<crate::infra::application_control::ControlAuthenticator>,
    ) -> Self {
        Self {
            queries,
            execution_application,
            mutation_application,
            authenticator,
        }
    }
}

/// Create rule engine API routes
pub fn create_rule_routes(state: Arc<RuleEngineState>) -> Router {
    Router::new()
        .route("/api/rules", get(list_rules).post(create_rule))
        .route(
            "/api/rules/{id}",
            get(get_rule).put(update_rule).delete(delete_rule),
        )
        .route("/api/rules/{id}/enable", post(enable_rule))
        .route("/api/rules/{id}/disable", post(disable_rule))
        .route("/api/rules/{id}/execute", post(execute_rule_now))
        .route("/api/rules/{id}/variables", get(get_rule_variables))
        .route("/api/scheduler/status", get(scheduler_status))
        .route("/api/scheduler/reload", post(scheduler_reload))
        .layer(axum::middleware::from_fn(
            common::logging::http_request_logger,
        ))
        .with_state(state)
}

#[cfg(feature = "openapi")]
#[derive(OpenApi)]
#[openapi(
    paths(list_rules, create_rule, get_rule, update_rule, delete_rule, enable_rule, disable_rule, execute_rule_now, get_rule_variables, scheduler_status, scheduler_reload),
    components(
        schemas(
            CreateRuleRequest,
            UpdateRuleRequest,
            RuleMutationRequest,
            RuleListQuery,
            ExecuteRuleRequest
        )
    ),
    tags(
        (name = "rules", description = "Rule management and execution")
    )
)]
pub struct RuleApiDoc;

/// Rule list query parameters (pagination)
#[derive(Debug, serde::Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct RuleListQuery {
    /// Page number (starting from 1)
    #[serde(default = "default_page")]
    pub page: usize,
    /// Items per page
    #[serde(default = "default_page_size")]
    pub page_size: usize,
}

fn default_page() -> usize {
    1
}

fn default_page_size() -> usize {
    20
}

/// Request DTO for creating a new rule (empty shell, ID auto-generated)
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct CreateRuleRequest {
    /// Rule name (required)
    #[cfg_attr(feature = "openapi", schema(example = "High Temperature Protection"))]
    pub name: String,

    /// Rule description (optional)
    #[cfg_attr(
        feature = "openapi",
        schema(example = "Stop the machine when temperature exceeds its safe limit")
    )]
    pub description: Option<String>,

    /// Current automation-rules revision. Omission uses the staged browser
    /// compatibility shim and does not protect the user's prior read.
    #[serde(default)]
    pub expected_revision: Option<u64>,

    /// Must be true because rule definitions are device-control policy.
    pub confirmed: bool,
}

/// Request DTO for updating an existing rule (all fields optional, partial update)
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct UpdateRuleRequest {
    /// Rule name (optional)
    #[cfg_attr(
        feature = "openapi",
        schema(example = "High Temperature Protection v2")
    )]
    pub name: Option<String>,

    /// Rule description (optional)
    #[cfg_attr(feature = "openapi", schema(example = "Updated protection logic"))]
    pub description: Option<String>,

    /// Whether the rule is enabled (optional)
    #[cfg_attr(feature = "openapi", schema(example = true))]
    pub enabled: Option<bool>,

    /// Execution priority (optional)
    #[cfg_attr(feature = "openapi", schema(example = 20))]
    pub priority: Option<u32>,

    /// Cooldown period in milliseconds (optional)
    #[cfg_attr(feature = "openapi", schema(example = 10000))]
    pub cooldown_ms: Option<u64>,

    /// Vue Flow complete data (nodes, edges, viewport)
    #[cfg_attr(feature = "openapi", schema(value_type = Option<Object>))]
    pub flow_json: Option<serde_json::Value>,

    /// Trigger configuration (optional). Replaces legacy `cooldown_ms`-based
    /// interval triggers with explicit per-rule trigger semantics.
    ///
    /// Two variants, discriminated by `"type"`:
    /// - `{"type":"interval","interval_ms":1000}` — periodic execution
    /// - `{"type":"on_change","point_refs":[{"instance":1,"point_type":"measurement","point":0}],"time_deadband_ms":200,"value_deadband":null}`
    ///   — event-sampling execution gated by time/value deadbands
    #[cfg_attr(feature = "openapi", schema(value_type = Option<Object>))]
    pub trigger_config: Option<serde_json::Value>,

    /// Current automation-rules revision. Omission uses the staged browser
    /// compatibility shim and does not protect the user's prior read.
    #[serde(default)]
    pub expected_revision: Option<u64>,

    /// Must be true because this mutation can change future device behavior.
    pub confirmed: bool,
}

/// Explicit confirmation envelope for rule enable/disable/delete/reload.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct RuleMutationRequest {
    /// Current automation-rules revision. Omission uses the staged browser
    /// compatibility shim and does not protect the user's prior read.
    #[serde(default)]
    pub expected_revision: Option<u64>,
    /// Must be true because rule management changes device-control policy.
    pub confirmed: bool,
}

/// Explicit confirmation envelope for manual rule execution.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ExecuteRuleRequest {
    /// Must be true because a rule may dispatch one or more device commands.
    pub confirmed: bool,
}

/// List paginated rule summaries.
#[cfg_attr(feature = "openapi", utoipa::path(
    get,
    path = "/api/rules",
    params(
        ("page" = Option<usize>, Query, description = "Page number (default: 1)"),
        ("page_size" = Option<usize>, Query, description = "Items per page (default: 20, max: 100)")
    ),
    responses(
        (status = 200, description = "List rules (paginated)", body = common::PaginatedResponse<serde_json::Value>,
            example = json!({
                "success": true,
                "data": {
                    "list": [
                        { "id": "rule-001", "name": "Test Rule", "enabled": true, "description": "demo rule" }
                    ],
                    "total": 1,
                    "page": 1,
                    "page_size": 20,
                    "total_pages": 1,
                    "has_next": false,
                    "has_previous": false
                }
            })
        )
    ),
    tag = "rules"
))]
pub async fn list_rules(
    State(state): State<Arc<RuleEngineState>>,
    Query(query): Query<RuleListQuery>,
) -> Result<Response, AutomationError> {
    let page = query.page.max(1);
    let page_size = query.page_size.clamp(1, 100);

    match state.queries.list_rules_paginated(page, page_size).await {
        Ok((rules, total)) => {
            let summaries: Vec<serde_json::Value> = rules
                .into_iter()
                .map(|rule| {
                    json!({
                        "id": rule.get("id").cloned().unwrap_or(serde_json::Value::Null),
                        "name": rule.get("name").cloned().unwrap_or(serde_json::Value::Null),
                        "enabled": rule.get("enabled").and_then(|v| v.as_bool()).unwrap_or(false),
                        "description": rule.get("description").cloned().unwrap_or(serde_json::Value::Null),
                    })
                })
                .collect();

            let paginated = PaginatedResponse::new(summaries, total, page, page_size);
            rules_query_response(&state.queries, paginated).await
        },
        Err(e) => {
            error!("List rules err: {}", e);
            Err(AutomationError::InternalError(
                "Failed to list rules".to_string(),
            ))
        },
    }
}

/// Create rule metadata with an automatically assigned identifier.
#[cfg_attr(feature = "openapi", utoipa::path(
    post,
    path = "/api/rules",
    request_body(
        content = CreateRuleRequest,
        description = "Rule metadata plus explicit high-risk confirmation (ID auto-generated)"
    ),
    params(("x-request-id" = Option<String>, Header, description = "Optional UUID audit correlation ID")),
    responses(
        (status = 200, description = "Rule mutation persisted; the response includes non-retryable terminal-audit and scheduler-refresh state", body = serde_json::Value,
         example = json!({ "success": true, "data": { "id": 1, "name": "High Temperature Protection", "status": "created", "request_id": "018f0000-0000-7000-8000-000000000007", "audit": { "status": "recorded", "retryable": false }, "scheduler_refresh": { "status": "refreshed", "retryable": false } } })),
        (status = 403, description = "Missing/invalid Bearer credentials or actor lacks automation.rule.manage"),
        (status = 409, description = "The automation-rules revision is stale"),
        (status = 422, description = "Explicit confirmation or rule data is invalid"),
        (status = 503, description = "Mandatory pre-mutation audit or rule storage is unavailable")
    ),
    security(("bearer_auth" = [])),
    tag = "rules"
))]
pub async fn create_rule(
    State(state): State<Arc<RuleEngineState>>,
    headers: HeaderMap,
    Json(req): Json<CreateRuleRequest>,
) -> Result<Json<SuccessResponse<serde_json::Value>>, AutomationError> {
    let acceptance = apply_rule_mutation(
        &state,
        &headers,
        req.confirmed,
        RevisionedRuleMutation::create(
            req.name.clone(),
            req.description,
            resolve_rules_revision(&state.queries, req.expected_revision, "POST /api/rules")
                .await?,
        ),
    )
    .await?;
    let new_id = acceptance.rule_id().ok_or_else(|| {
        AutomationError::InternalError("rule creation returned no rule identifier".to_string())
    })?;

    debug!("Rule created: {} ({})", req.name, new_id.get());
    Ok(Json(SuccessResponse::new(json!({
        "id": new_id.get(),
        "name": req.name,
        "status": "created",
        "resulting_revision": acceptance.resulting_revision().get(),
        "request_id": acceptance.request_id(),
        "audit": crate::api::http_boundary::completion_audit_response(
            acceptance.completion_audit()
        ),
        "scheduler_refresh": scheduler_refresh_response(acceptance.runtime_status())
    }))))
}

/// Get one complete rule definition.
#[cfg_attr(feature = "openapi", utoipa::path(
    get,
    path = "/api/rules/{id}",
    params(("id" = i64, Path, description = "Rule identifier")),
    responses(
        (status = 200, description = "Rule details", body = serde_json::Value)
    ),
    tag = "rules"
))]
pub async fn get_rule(
    State(state): State<Arc<RuleEngineState>>,
    Path(id): Path<i64>,
) -> Result<Response, AutomationError> {
    match state.queries.get_rule(id).await {
        Ok(rule) => rules_query_response(&state.queries, rule).await,
        Err(e) => {
            error!("Get rule {}: {}", id, e);
            Err(AutomationError::RuleNotFound(id.to_string()))
        },
    }
}

/// Partially update a rule definition.
#[cfg_attr(feature = "openapi", utoipa::path(
    put,
    path = "/api/rules/{id}",
    params(
        ("id" = i64, Path, description = "Rule ID"),
        ("x-request-id" = Option<String>, Header, description = "Optional UUID audit correlation ID")
    ),
    request_body(
        content = UpdateRuleRequest,
        description = "Fields to update plus explicit high-risk confirmation"
    ),
    responses(
        (status = 200, description = "Rule mutation persisted; the response includes non-retryable terminal-audit and scheduler-refresh state", body = serde_json::Value,
         example = json!({ "success": true, "data": { "id": 1, "status": "updated", "request_id": "018f0000-0000-7000-8000-000000000007", "audit": { "status": "recorded", "retryable": false }, "scheduler_refresh": { "status": "refreshed", "retryable": false } } })),
        (status = 403, description = "Missing/invalid Bearer credentials or actor lacks automation.rule.manage"),
        (status = 409, description = "The automation-rules revision is stale"),
        (status = 404, description = "Rule not found"),
        (status = 422, description = "Explicit confirmation or rule data is invalid"),
        (status = 503, description = "Mandatory pre-mutation audit or rule storage is unavailable")
    ),
    security(("bearer_auth" = [])),
    tag = "rules"
))]
pub async fn update_rule(
    State(state): State<Arc<RuleEngineState>>,
    Path(id): Path<i64>,
    headers: HeaderMap,
    Json(req): Json<UpdateRuleRequest>,
) -> Result<Json<SuccessResponse<serde_json::Value>>, AutomationError> {
    let rule_id = parse_rule_id(id)?;
    let flow_json = req
        .flow_json
        .map(|value| serde_json::to_string(&value))
        .transpose()
        .map_err(|error| AutomationError::SerializationError(error.to_string()))?;
    let trigger_config = req
        .trigger_config
        .map(|value| serde_json::to_string(&value))
        .transpose()
        .map_err(|error| AutomationError::SerializationError(error.to_string()))?;
    let acceptance = apply_rule_mutation(
        &state,
        &headers,
        req.confirmed,
        RevisionedRuleMutation::new(
            RuleMutation::Update {
                rule_id,
                name: req.name,
                description: req.description,
                enabled: req.enabled,
                priority: req.priority,
                cooldown_ms: req.cooldown_ms,
                flow_json,
                trigger_config,
            },
            resolve_rules_revision(&state.queries, req.expected_revision, "PUT /api/rules/{id}")
                .await?,
        ),
    )
    .await?;

    debug!("Rule {} updated", id);
    Ok(Json(SuccessResponse::new(json!({
        "id": id,
        "status": "updated",
        "resulting_revision": acceptance.resulting_revision().get(),
        "request_id": acceptance.request_id(),
        "audit": crate::api::http_boundary::completion_audit_response(
            acceptance.completion_audit()
        ),
        "scheduler_refresh": scheduler_refresh_response(acceptance.runtime_status())
    }))))
}

/// Delete a rule and refresh the runtime.
#[cfg_attr(feature = "openapi", utoipa::path(
    delete,
    path = "/api/rules/{id}",
    params(
        ("id" = i64, Path, description = "Rule identifier"),
        ("x-request-id" = Option<String>, Header, description = "Optional UUID audit correlation ID")
    ),
    request_body(
        content = RuleMutationRequest,
        description = "Explicit high-risk confirmation"
    ),
    responses(
        (status = 200, description = "Rule deletion persisted; the response includes non-retryable terminal-audit and scheduler-refresh state", body = serde_json::Value),
        (status = 403, description = "Missing/invalid Bearer credentials or actor lacks automation.rule.manage"),
        (status = 409, description = "The automation-rules revision is stale"),
        (status = 404, description = "Rule not found"),
        (status = 422, description = "Explicit confirmation is required"),
        (status = 503, description = "Mandatory pre-mutation audit or rule storage is unavailable")
    ),
    security(("bearer_auth" = [])),
    tag = "rules"
))]
pub async fn delete_rule(
    State(state): State<Arc<RuleEngineState>>,
    Path(id): Path<i64>,
    headers: HeaderMap,
    Json(request): Json<RuleMutationRequest>,
) -> Result<Json<SuccessResponse<serde_json::Value>>, AutomationError> {
    let rule_id = parse_rule_id(id)?;
    let expected_revision = resolve_rules_revision(
        &state.queries,
        request.expected_revision,
        "DELETE /api/rules/{id}",
    )
    .await?;
    let acceptance = apply_rule_mutation(
        &state,
        &headers,
        request.confirmed,
        RevisionedRuleMutation::delete(rule_id, expected_revision),
    )
    .await?;

    debug!("Rule {} deleted", id);
    Ok(Json(SuccessResponse::new(json!({
        "id": id,
        "status": "OK",
        "resulting_revision": acceptance.resulting_revision().get(),
        "request_id": acceptance.request_id(),
        "audit": crate::api::http_boundary::completion_audit_response(
            acceptance.completion_audit()
        ),
        "scheduler_refresh": scheduler_refresh_response(acceptance.runtime_status())
    }))))
}

/// Enable a rule and refresh the runtime.
#[cfg_attr(feature = "openapi", utoipa::path(
    post,
    path = "/api/rules/{id}/enable",
    params(
        ("id" = i64, Path, description = "Rule identifier"),
        ("x-request-id" = Option<String>, Header, description = "Optional UUID audit correlation ID")
    ),
    request_body(
        content = RuleMutationRequest,
        description = "Explicit high-risk confirmation"
    ),
    responses(
        (status = 200, description = "Rule enablement persisted; the response includes non-retryable terminal-audit and scheduler-refresh state", body = serde_json::Value),
        (status = 403, description = "Missing/invalid Bearer credentials or actor lacks automation.rule.manage"),
        (status = 409, description = "The automation-rules revision is stale"),
        (status = 404, description = "Rule not found"),
        (status = 422, description = "Explicit confirmation is required"),
        (status = 503, description = "Mandatory pre-mutation audit or rule storage is unavailable")
    ),
    security(("bearer_auth" = [])),
    tag = "rules"
))]
pub async fn enable_rule(
    State(state): State<Arc<RuleEngineState>>,
    Path(id): Path<i64>,
    headers: HeaderMap,
    Json(request): Json<RuleMutationRequest>,
) -> Result<Json<SuccessResponse<serde_json::Value>>, AutomationError> {
    let rule_id = parse_rule_id(id)?;
    let expected_revision = resolve_rules_revision(
        &state.queries,
        request.expected_revision,
        "POST /api/rules/{id}/enable",
    )
    .await?;
    let acceptance = apply_rule_mutation(
        &state,
        &headers,
        request.confirmed,
        RevisionedRuleMutation::set_enabled(rule_id, true, expected_revision),
    )
    .await?;

    info!("Enabled rule: {}", id);
    Ok(Json(SuccessResponse::new(json!({
        "id": id,
        "status": "OK",
        "resulting_revision": acceptance.resulting_revision().get(),
        "request_id": acceptance.request_id(),
        "audit": crate::api::http_boundary::completion_audit_response(
            acceptance.completion_audit()
        ),
        "scheduler_refresh": scheduler_refresh_response(acceptance.runtime_status())
    }))))
}

/// Disable a rule and refresh the runtime.
#[cfg_attr(feature = "openapi", utoipa::path(
    post,
    path = "/api/rules/{id}/disable",
    params(
        ("id" = i64, Path, description = "Rule identifier"),
        ("x-request-id" = Option<String>, Header, description = "Optional UUID audit correlation ID")
    ),
    request_body(
        content = RuleMutationRequest,
        description = "Explicit high-risk confirmation"
    ),
    responses(
        (status = 200, description = "Rule disablement persisted; the response includes non-retryable terminal-audit and scheduler-refresh state", body = serde_json::Value),
        (status = 403, description = "Missing/invalid Bearer credentials or actor lacks automation.rule.manage"),
        (status = 409, description = "The automation-rules revision is stale"),
        (status = 404, description = "Rule not found"),
        (status = 422, description = "Explicit confirmation is required"),
        (status = 503, description = "Mandatory pre-mutation audit or rule storage is unavailable")
    ),
    security(("bearer_auth" = [])),
    tag = "rules"
))]
pub async fn disable_rule(
    State(state): State<Arc<RuleEngineState>>,
    Path(id): Path<i64>,
    headers: HeaderMap,
    Json(request): Json<RuleMutationRequest>,
) -> Result<Json<SuccessResponse<serde_json::Value>>, AutomationError> {
    let rule_id = parse_rule_id(id)?;
    let expected_revision = resolve_rules_revision(
        &state.queries,
        request.expected_revision,
        "POST /api/rules/{id}/disable",
    )
    .await?;
    let acceptance = apply_rule_mutation(
        &state,
        &headers,
        request.confirmed,
        RevisionedRuleMutation::set_enabled(rule_id, false, expected_revision),
    )
    .await?;

    info!("Disabled rule: {}", id);
    Ok(Json(SuccessResponse::new(json!({
        "id": id,
        "status": "OK",
        "resulting_revision": acceptance.resulting_revision().get(),
        "request_id": acceptance.request_id(),
        "audit": crate::api::http_boundary::completion_audit_response(
            acceptance.completion_audit()
        ),
        "scheduler_refresh": scheduler_refresh_response(acceptance.runtime_status())
    }))))
}

fn parse_rule_id(id: i64) -> Result<RuleId, AutomationError> {
    u64::try_from(id)
        .map(RuleId::new)
        .map_err(|_| AutomationError::InvalidData("rule id must be non-negative".to_string()))
}

async fn resolve_rules_revision(
    queries: &RuleQueries,
    requested: Option<u64>,
    endpoint: &'static str,
) -> Result<aether_ports::AutomationRulesRevision, AutomationError> {
    let value = match requested {
        Some(value) => value,
        None => {
            let current = queries.current_revision().await?;
            tracing::warn!(
                endpoint,
                revision = current,
                "revisionless rules compatibility shim used; this request is CAS-safe at commit but cannot detect edits made since the caller's prior read"
            );
            current
        },
    };
    if value == 0 || value >= i64::MAX as u64 {
        return Err(AutomationError::InvalidData(
            "expected_revision must be in 1..i64::MAX".to_string(),
        ));
    }
    Ok(aether_ports::AutomationRulesRevision::new(value))
}

async fn rules_query_response<T: serde::Serialize>(
    queries: &RuleQueries,
    data: T,
) -> Result<Response, AutomationError> {
    let revision = queries.current_revision().await?;
    let mut response = Json(SuccessResponse::new(data)).into_response();
    let revision_text = revision.to_string();
    let revision = HeaderValue::from_str(&revision_text)
        .map_err(|error| AutomationError::InternalError(error.to_string()))?;
    let etag = HeaderValue::from_str(&format!("\"{revision_text}\""))
        .map_err(|error| AutomationError::InternalError(error.to_string()))?;
    response.headers_mut().insert(ETAG, etag);
    response
        .headers_mut()
        .insert("x-aether-configuration-revision", revision);
    Ok(response)
}

async fn apply_rule_mutation(
    state: &RuleEngineState,
    headers: &HeaderMap,
    confirmed: bool,
    mutation: RevisionedRuleMutation,
) -> Result<aether_application::RuleMutationAcceptance, AutomationError> {
    let timestamp_ms = chrono::Utc::now().timestamp_millis().max(0) as u64;
    let invocation = crate::api::http_boundary::command_invocation_from_headers(
        &state.authenticator,
        headers,
        confirmed,
        aether_domain::TimestampMs::new(timestamp_ms),
    );
    let acceptance = state
        .mutation_application
        .mutate_revisioned(invocation.context(), mutation)
        .await
        .map_err(rule_mutation_error)?;
    if let Some(failure) = acceptance.completion_audit().failure() {
        error!(
            request_id = acceptance.request_id(),
            operation = acceptance.kind().as_str(),
            rule_id = ?acceptance.rule_id().map(aether_domain::RuleId::get),
            error = %failure,
            "rule mutation completed but its terminal audit is incomplete; do not retry"
        );
    }
    if let Some(failure) = acceptance.runtime_status().failure() {
        if acceptance.runtime_status().scheduler_running() {
            error!(
                request_id = acceptance.request_id(),
                operation = acceptance.kind().as_str(),
                rule_id = ?acceptance.rule_id().map(aether_domain::RuleId::get),
                error = %failure,
                "rule mutation was persisted; deterministic tick evaluation remains active but PointWatch hints are gated pending reconciliation"
            );
        } else {
            error!(
                request_id = acceptance.request_id(),
                operation = acceptance.kind().as_str(),
                rule_id = ?acceptance.rule_id().map(aether_domain::RuleId::get),
                error = %failure,
                "rule mutation was persisted but scheduler refresh failed; scheduler stopped fail-closed"
            );
        }
    }
    Ok(acceptance)
}

fn scheduler_refresh_response(status: &aether_ports::RuleRuntimeStatus) -> serde_json::Value {
    match status.as_str() {
        "refreshed" => json!({
            "status": "refreshed",
            "reconciliation_required": false,
            "retryable": false
        }),
        "point_watch_gated" => json!({
            "status": "point_watch_gated",
            "reconciliation_required": true,
            "scheduler_running": true,
            "retryable": false,
            "message": "mutation was persisted and deterministic tick evaluation remains active, but PointWatch hints are gated until reconciliation; do not retry the committed command"
        }),
        _ => json!({
            "status": "stopped",
            "reconciliation_required": true,
            "scheduler_running": false,
            "retryable": false,
            "message": "mutation was persisted but scheduler refresh failed; scheduler stopped fail-closed; do not retry"
        }),
    }
}

fn rule_mutation_error(error: aether_application::ApplicationError) -> AutomationError {
    if let aether_application::ApplicationError::Port(port_error) = &error
        && port_error.kind() == aether_ports::PortErrorKind::NotFound
    {
        return AutomationError::RuleNotFound(port_error.to_string());
    }
    if let aether_application::ApplicationError::Port(port_error) = &error
        && port_error.kind() == aether_ports::PortErrorKind::Conflict
    {
        return AutomationError::RoutingConflict(port_error.to_string());
    }
    AutomationError::from(error)
}

/// Execute a commissioned rule through the shared application API.
#[cfg_attr(feature = "openapi", utoipa::path(
    post,
    path = "/api/rules/{id}/execute",
    params(
        ("id" = i64, Path, description = "Rule ID"),
        ("x-request-id" = Option<String>, Header, description = "Optional UUID audit correlation ID")
    ),
    request_body = ExecuteRuleRequest,
    responses(
        (status = 200, description = "Accepted rule execution summary. A terminal-audit append failure is represented by `audit.status=incomplete` and `retryable=false`; the completed rule must not be retried.", body = serde_json::Value,
         example = json!({
             "success": true,
             "data": {
                 "result": "executed",
                 "rule_id": 7,
                 "request_id": "018f0000-0000-7000-8000-000000000007",
                 "actions_attempted": 1,
                 "actions_succeeded": 1,
                 "audit": { "status": "recorded", "retryable": false },
                 "completed_at_ms": 1720000000000_u64
             }
         })),
        (status = 403, description = "Missing/invalid Bearer credentials or actor lacks automation.rule.execute"),
        (status = 422, description = "Explicit confirmation is required"),
        (status = 503, description = "The required attempted audit or deterministic rule runtime failed before completed execution")
    ),
    security(("bearer_auth" = [])),
    tag = "rules"
))]
pub async fn execute_rule_now(
    Path(id): Path<i64>,
    State(state): State<Arc<RuleEngineState>>,
    headers: HeaderMap,
    Json(request): Json<ExecuteRuleRequest>,
) -> Result<Json<SuccessResponse<serde_json::Value>>, AutomationError> {
    let rule_id = u64::try_from(id)
        .map(aether_domain::RuleId::new)
        .map_err(|_| AutomationError::InvalidData("rule id must be non-negative".to_string()))?;
    let timestamp_ms = chrono::Utc::now().timestamp_millis().max(0) as u64;
    let invocation = crate::api::http_boundary::command_invocation_from_headers(
        &state.authenticator,
        &headers,
        request.confirmed,
        aether_domain::TimestampMs::new(timestamp_ms),
    );
    let acceptance = state
        .execution_application
        .execute(invocation.context(), rule_id)
        .await
        .map_err(AutomationError::from)?;
    if let Some(failure) = acceptance.completion_audit().failure() {
        error!(
            request_id = acceptance.request_id(),
            rule_id = acceptance.rule_id().get(),
            error = %failure,
            "manual rule execution completed but its terminal audit is incomplete; do not retry"
        );
    }

    Ok(Json(SuccessResponse::new(json!({
        "result": "executed",
        "rule_id": acceptance.rule_id().get(),
        "request_id": acceptance.request_id(),
        "actions_attempted": acceptance.actions_attempted(),
        "actions_succeeded": acceptance.actions_succeeded(),
        "audit": crate::api::http_boundary::completion_audit_response(
            acceptance.completion_audit()
        ),
        "completed_at_ms": acceptance.completed_at().get()
    }))))
}

/// Return scheduler liveness and commissioned-rule counts.
#[cfg_attr(feature = "openapi", utoipa::path(
    get,
    path = "/api/scheduler/status",
    responses(
        (status = 200, description = "Scheduler status", body = serde_json::Value)
    ),
    tag = "rules"
))]
pub async fn scheduler_status(
    State(state): State<Arc<RuleEngineState>>,
) -> Result<Json<SuccessResponse<serde_json::Value>>, AutomationError> {
    let status = state.queries.scheduler_status().await;

    Ok(Json(SuccessResponse::new(json!({
        "running": status.running,
        "total_rules": status.total_rules,
        "enabled_rules": status.enabled_rules,
        "tick_interval_ms": status.tick_interval_ms
    }))))
}

/// Reconcile the scheduler with committed rule configuration.
#[cfg_attr(feature = "openapi", utoipa::path(
    post,
    path = "/api/scheduler/reload",
    request_body(
        content = RuleMutationRequest,
        description = "Explicit high-risk confirmation because reload can activate enabled rules"
    ),
    params(("x-request-id" = Option<String>, Header, description = "Optional UUID audit correlation ID")),
    responses(
        (status = 200, description = "Scheduler reload accepted; the response includes non-retryable terminal-audit and scheduler-refresh state", body = serde_json::Value),
        (status = 403, description = "Missing/invalid Bearer credentials or actor lacks automation.rule.manage"),
        (status = 409, description = "The automation-rules revision is stale"),
        (status = 422, description = "Explicit confirmation is required"),
        (status = 503, description = "Mandatory pre-reload audit is unavailable")
    ),
    security(("bearer_auth" = [])),
    tag = "rules"
))]
pub async fn scheduler_reload(
    State(state): State<Arc<RuleEngineState>>,
    headers: HeaderMap,
    Json(request): Json<RuleMutationRequest>,
) -> Result<Json<SuccessResponse<serde_json::Value>>, AutomationError> {
    let expected_revision = resolve_rules_revision(
        &state.queries,
        request.expected_revision,
        "POST /api/scheduler/reload",
    )
    .await?;
    let acceptance = apply_rule_mutation(
        &state,
        &headers,
        request.confirmed,
        RevisionedRuleMutation::reload(expected_revision),
    )
    .await?;
    let count = state.queries.scheduler_status().await.enabled_rules;
    info!("Scheduler reloaded {} rules", count);
    Ok(Json(SuccessResponse::new(json!({
        "status": "OK",
        "rules_loaded": count,
        "resulting_revision": acceptance.resulting_revision().get(),
        "request_id": acceptance.request_id(),
        "audit": crate::api::http_boundary::completion_audit_response(
            acceptance.completion_audit()
        ),
        "scheduler_refresh": scheduler_refresh_response(acceptance.runtime_status())
    }))))
}

/// Return the unique variables declared by a rule.
#[cfg_attr(feature = "openapi", utoipa::path(
    get,
    path = "/api/rules/{id}/variables",
    params(("id" = i64, Path, description = "Rule identifier")),
    responses(
        (status = 200, description = "Rule variables", body = serde_json::Value)
    ),
    tag = "rules"
))]
pub async fn get_rule_variables(
    State(state): State<Arc<RuleEngineState>>,
    Path(id): Path<i64>,
) -> Result<Json<SuccessResponse<serde_json::Value>>, AutomationError> {
    let variables = state.queries.rule_variables(id).await?;

    debug!(
        "Rule {} has {} unique variables: {:?}",
        id,
        variables.len(),
        variables.iter().map(|v| &v.name).collect::<Vec<_>>()
    );

    Ok(Json(SuccessResponse::new(json!({
        "rule_id": id,
        "variables": variables
    }))))
}
