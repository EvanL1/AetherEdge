//! Rule Engine API Routes
//!
//! Provides Vue Flow-based rule management and execution endpoints.
//! These routes are integrated into automation and served on port 6002.

#![allow(clippy::disallowed_methods)] // json! macro used in multiple functions

use crate::error::AutomationError;
use aether_calc::StateStore;
use aether_domain::RuleId;
use aether_ports::{RevisionedRuleMutation, RuleMutation};
use aether_rules::{self as rule_repository, RuleNode, RuleScheduler, RuleVariable};
use axum::{
    Router,
    extract::{Path, Query, State},
    http::{HeaderMap, HeaderValue, header::ETAG},
    response::{IntoResponse, Json, Response},
    routing::{get, post},
};
use common::{PaginatedResponse, SuccessResponse};
use serde_json::json;
use sqlx::SqlitePool;
use std::sync::Arc;
use tracing::{debug, error, info};
#[cfg(feature = "swagger-ui")]
use utoipa::OpenApi;

/// Rule Engine state shared across handlers
///
/// Generic over `S: StateStore` to support different state backends:
/// - `MemoryStateStore`: In-memory (default, lost on restart)
pub struct RuleEngineState<S: StateStore = aether_calc::MemoryStateStore> {
    /// SQLite pool for rule persistence
    pub pool: SqlitePool,
    /// Rule scheduler (owns the executor)
    pub scheduler: Arc<RuleScheduler<S>>,
    /// Governed manual rule-execution use case. `None` is allowed only in
    /// tests that never mount a usable execution boundary.
    execution_application: Option<Arc<aether_application::RuleExecutionApplication>>,
    /// Governed rule-management use case. Production composition must install
    /// this before mounting mutating rule or scheduler routes.
    mutation_application: Option<Arc<aether_application::RuleMutationApplication>>,
    /// Authenticator shared with external device control.
    execution_authenticator: Option<Arc<crate::infra::application_control::ControlAuthenticator>>,
}

impl<S: StateStore + 'static> RuleEngineState<S> {
    pub fn new(pool: SqlitePool, scheduler: Arc<RuleScheduler<S>>) -> Self {
        Self {
            pool,
            scheduler,
            execution_application: None,
            mutation_application: None,
            execution_authenticator: None,
        }
    }

    /// Installs the mandatory production boundary for manual rule execution.
    #[must_use]
    pub fn with_execution_boundary(
        mut self,
        application: Arc<aether_application::RuleExecutionApplication>,
        authenticator: Arc<crate::infra::application_control::ControlAuthenticator>,
    ) -> Self {
        self.execution_application = Some(application);
        self.execution_authenticator = Some(authenticator);
        self
    }

    /// Installs the mandatory production boundary for rule mutations/reload.
    #[must_use]
    pub fn with_mutation_boundary(
        mut self,
        application: Arc<aether_application::RuleMutationApplication>,
        authenticator: Arc<crate::infra::application_control::ControlAuthenticator>,
    ) -> Self {
        self.mutation_application = Some(application);
        self.execution_authenticator = Some(authenticator);
        self
    }
}

/// Create rule engine API routes
pub fn create_rule_routes<S: StateStore + 'static>(state: Arc<RuleEngineState<S>>) -> Router {
    Router::new()
        // Rule management (Vue Flow-based)
        .route("/api/rules", get(list_rules::<S>).post(create_rule::<S>))
        .route(
            "/api/rules/{id}",
            get(get_rule::<S>)
                .put(update_rule::<S>)
                .delete(delete_rule::<S>),
        )
        .route("/api/rules/{id}/enable", post(enable_rule::<S>))
        .route("/api/rules/{id}/disable", post(disable_rule::<S>))
        .route("/api/rules/{id}/execute", post(execute_rule_now::<S>))
        .route("/api/rules/{id}/variables", get(get_rule_variables::<S>))
        // Scheduler control
        .route("/api/scheduler/status", get(scheduler_status::<S>))
        .route("/api/scheduler/reload", post(scheduler_reload::<S>))
        // Apply HTTP request logging middleware
        .layer(axum::middleware::from_fn(common::logging::http_request_logger))
        .with_state(state)
}

// ============================================================================
// OpenAPI Documentation
// ============================================================================

#[cfg(feature = "swagger-ui")]
#[derive(OpenApi)]
#[openapi(
    paths(list_rules, create_rule, get_rule, update_rule, delete_rule, enable_rule, disable_rule, execute_rule_now, get_rule_variables, scheduler_status, scheduler_reload),
    components(
        schemas(
            CreateRuleRequest,
            UpdateRuleRequest,
            RuleMutationRequest,
            RuleListQuery,
            ExecuteRuleRequest,
            // PeriodDelta Swagger Schemas
            RuleVariableSchema,
            PeriodType,
            PeriodDeltaNodeSchema,
            VueFlowPeriodDeltaNode,
            VueFlowPeriodDeltaNodeData,
            PeriodDeltaConfigSchema
        )
    ),
    tags(
        (name = "rules", description = "Rule management and execution")
    )
)]
pub struct RuleApiDoc;

// ============================================================================
// PeriodDelta Swagger Schema Types (for API documentation only)
// ============================================================================

/// Rule variable definition for Swagger documentation
///
/// Represents a data point reference within a rule, identifying an instance and point.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "swagger-ui", derive(utoipa::ToSchema))]
pub struct RuleVariableSchema {
    /// Variable name (e.g., "X1", "Y1")
    #[cfg_attr(feature = "swagger-ui", schema(example = "X1"))]
    pub name: String,

    /// Device instance ID
    #[cfg_attr(feature = "swagger-ui", schema(example = 1))]
    pub instance: u32,

    /// Point type: "measurement" or "action"
    #[serde(rename = "pointType")]
    #[cfg_attr(feature = "swagger-ui", schema(example = "measurement"))]
    pub point_type: String,

    /// Point ID within the device
    #[cfg_attr(feature = "swagger-ui", schema(example = 9))]
    pub point: u32,
}

/// Period type for PeriodDelta node
///
/// Defines the time window for delta calculation.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "swagger-ui", derive(utoipa::ToSchema))]
pub enum PeriodType {
    /// Daily period (resets at midnight local time)
    #[serde(rename = "daily")]
    Daily,
    /// Weekly period (resets on Monday midnight)
    #[serde(rename = "weekly")]
    Weekly,
    /// Monthly period (resets on 1st of month)
    #[serde(rename = "monthly")]
    Monthly,
    /// Quarterly period (resets on Q1/Q2/Q3/Q4 start)
    #[serde(rename = "quarterly")]
    Quarterly,
}

/// PeriodDelta node configuration for Swagger documentation
///
/// This node calculates the delta (change) of a cumulative value within a specified period.
/// Common use case: Calculate daily/weekly/monthly production from a cumulative counter.
///
/// # Example Use Cases
/// - **Daily Production**: Input from a total unit counter (ID 9), output to a daily counter (ID 101)
/// - **Monthly Runtime**: Track accumulated machine runtime for maintenance
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "swagger-ui", derive(utoipa::ToSchema))]
pub struct PeriodDeltaNodeSchema {
    /// Node type identifier (always "action-periodDelta")
    #[serde(rename = "type")]
    #[cfg_attr(feature = "swagger-ui", schema(example = "action-periodDelta"))]
    pub node_type: String,

    /// Input variable - source cumulative value (e.g., total unit count)
    pub input: RuleVariableSchema,

    /// Output variable - period delta result (e.g., daily unit count)
    pub output: RuleVariableSchema,

    /// Period type: daily, weekly, monthly, or quarterly
    #[cfg_attr(feature = "swagger-ui", schema(example = "daily"))]
    pub period: String,

    /// Output wires to next node(s)
    #[cfg_attr(feature = "swagger-ui", schema(value_type = Object, example = json!({"default": ["next-node-id"]})))]
    pub wires: serde_json::Value,
}

/// Vue Flow node wrapper for PeriodDelta
///
/// This is the full structure as stored in flow_json for the Vue Flow editor.
/// Contains position, display properties, and the nested config.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "swagger-ui", derive(utoipa::ToSchema))]
pub struct VueFlowPeriodDeltaNode {
    /// Unique node ID
    #[cfg_attr(feature = "swagger-ui", schema(example = "period-delta-1"))]
    pub id: String,

    /// Node type (use "custom" for custom nodes)
    #[serde(rename = "type")]
    #[cfg_attr(feature = "swagger-ui", schema(example = "custom"))]
    pub node_type: String,

    /// Node position on canvas
    #[cfg_attr(feature = "swagger-ui", schema(value_type = Object, example = json!({"x": 150, "y": 100})))]
    pub position: serde_json::Value,

    /// Node data containing the PeriodDelta configuration
    pub data: VueFlowPeriodDeltaNodeData,
}

/// Vue Flow node data for PeriodDelta
///
/// Contains the internal type identifier and configuration.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "swagger-ui", derive(utoipa::ToSchema))]
pub struct VueFlowPeriodDeltaNodeData {
    /// Internal node type (must be "action-periodDelta")
    #[serde(rename = "type")]
    #[cfg_attr(feature = "swagger-ui", schema(example = "action-periodDelta"))]
    pub data_type: String,

    /// Display label for the node
    #[cfg_attr(feature = "swagger-ui", schema(example = "Daily Production"))]
    pub label: Option<String>,

    /// Node configuration
    pub config: PeriodDeltaConfigSchema,
}

/// PeriodDelta config within Vue Flow node data
///
/// The actual configuration parameters for the PeriodDelta calculation.
///
/// # Point Mapping Table
/// | Input Point (Cumulative) | Output Point (Period Delta) | Period |
/// |--------------------------|----------------------------|--------|
/// | 9 (Total Units) | 101 (Daily Units) | daily |
/// | 9 (Total Units) | 103 (Weekly Units) | weekly |
/// | 10 (Runtime Hours) | 102 (Daily Runtime) | daily |
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "swagger-ui", derive(utoipa::ToSchema))]
pub struct PeriodDeltaConfigSchema {
    /// Input variable (cumulative source, e.g., total production counter)
    pub input: RuleVariableSchema,

    /// Output variable (period delta destination, e.g., daily production)
    pub output: RuleVariableSchema,

    /// Period: "daily" | "weekly" | "monthly" | "quarterly"
    #[cfg_attr(feature = "swagger-ui", schema(example = "daily"))]
    pub period: String,

    /// Wires to next nodes
    #[cfg_attr(feature = "swagger-ui", schema(value_type = Object, example = json!({"default": ["next-node-id"]})))]
    pub wires: serde_json::Value,
}

// ============================================================================
// Handlers
// ============================================================================

/// Rule list query parameters (pagination)
#[derive(Debug, serde::Deserialize)]
#[cfg_attr(feature = "swagger-ui", derive(utoipa::ToSchema))]
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
#[cfg_attr(feature = "swagger-ui", derive(utoipa::ToSchema))]
pub struct CreateRuleRequest {
    /// Rule name (required)
    #[cfg_attr(
        feature = "swagger-ui",
        schema(example = "High Temperature Protection")
    )]
    pub name: String,

    /// Rule description (optional)
    #[cfg_attr(
        feature = "swagger-ui",
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
#[cfg_attr(feature = "swagger-ui", derive(utoipa::ToSchema))]
pub struct UpdateRuleRequest {
    /// Rule name (optional)
    #[cfg_attr(
        feature = "swagger-ui",
        schema(example = "High Temperature Protection v2")
    )]
    pub name: Option<String>,

    /// Rule description (optional)
    #[cfg_attr(feature = "swagger-ui", schema(example = "Updated protection logic"))]
    pub description: Option<String>,

    /// Whether the rule is enabled (optional)
    #[cfg_attr(feature = "swagger-ui", schema(example = true))]
    pub enabled: Option<bool>,

    /// Execution priority (optional)
    #[cfg_attr(feature = "swagger-ui", schema(example = 20))]
    pub priority: Option<u32>,

    /// Cooldown period in milliseconds (optional)
    #[cfg_attr(feature = "swagger-ui", schema(example = 10000))]
    pub cooldown_ms: Option<u64>,

    /// Vue Flow complete data (nodes, edges, viewport)
    #[cfg_attr(feature = "swagger-ui", schema(value_type = Option<Object>))]
    pub flow_json: Option<serde_json::Value>,

    /// Trigger configuration (optional). Replaces legacy `cooldown_ms`-based
    /// interval triggers with explicit per-rule trigger semantics.
    ///
    /// Two variants, discriminated by `"type"`:
    /// - `{"type":"interval","interval_ms":1000}` — periodic execution
    /// - `{"type":"on_change","point_refs":[{"instance":1,"point_type":"measurement","point":0}],"time_deadband_ms":200,"value_deadband":null}`
    ///   — event-sampling execution gated by time/value deadbands
    #[cfg_attr(feature = "swagger-ui", schema(value_type = Option<Object>))]
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
#[cfg_attr(feature = "swagger-ui", derive(utoipa::ToSchema))]
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
#[cfg_attr(feature = "swagger-ui", derive(utoipa::ToSchema))]
pub struct ExecuteRuleRequest {
    /// Must be true because a rule may dispatch one or more device commands.
    pub confirmed: bool,
}

/// List all rules.
///
/// Returns the full rule definitions including both `nodes_json` (compact
/// execution topology used by the scheduler) and `flow_json` (Vue Flow
/// layout used by the frontend editor). No pagination — rule count is
/// typically small. Use `/api/rules/{id}` for a single rule.
#[cfg_attr(feature = "swagger-ui", utoipa::path(
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
pub async fn list_rules<S: StateStore + 'static>(
    State(state): State<Arc<RuleEngineState<S>>>,
    Query(query): Query<RuleListQuery>,
) -> Result<Response, AutomationError> {
    let page = query.page.max(1);
    let page_size = query.page_size.clamp(1, 100);

    match rule_repository::list_rules_paginated(&state.pool, page, page_size).await {
        Ok((rules, total)) => {
            // Only expose summary fields for list view
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
            rules_query_response(&state.pool, paginated).await
        },
        Err(e) => {
            error!("List rules err: {}", e);
            Err(AutomationError::InternalError(
                "Failed to list rules".to_string(),
            ))
        },
    }
}

/// Create a new rule (metadata only)
///
/// Creates rule metadata. ID is auto-generated (sequential: 1, 2, 3...).
/// The execution topology (flow_json) is updated later via PUT endpoint.
#[cfg_attr(feature = "swagger-ui", utoipa::path(
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
pub async fn create_rule<S: StateStore + 'static>(
    State(state): State<Arc<RuleEngineState<S>>>,
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
            resolve_rules_revision(&state.pool, req.expected_revision, "POST /api/rules").await?,
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
        "audit": crate::infra::application_control::completion_audit_response(
            acceptance.completion_audit()
        ),
        "scheduler_refresh": scheduler_refresh_response(acceptance.runtime_status())
    }))))
}

/// Get one rule by ID.
///
/// Same shape as the entries in `GET /api/rules` but a single object.
/// Returns 404 when the id doesn't exist. Frontend rule-editor opens
/// this to populate the canvas before edit.
#[cfg_attr(feature = "swagger-ui", utoipa::path(
    get,
    path = "/api/rules/{id}",
    params(("id" = i64, Path, description = "Rule identifier")),
    responses(
        (status = 200, description = "Rule details", body = serde_json::Value)
    ),
    tag = "rules"
))]
pub async fn get_rule<S: StateStore + 'static>(
    State(state): State<Arc<RuleEngineState<S>>>,
    Path(id): Path<i64>,
) -> Result<Response, AutomationError> {
    match rule_repository::get_rule(&state.pool, id).await {
        Ok(rule) => rules_query_response(&state.pool, rule).await,
        Err(e) => {
            error!("Get rule {}: {}", id, e);
            Err(AutomationError::RuleNotFound(id.to_string()))
        },
    }
}

/// Update rule metadata
///
/// Updates rule metadata. Only provided fields are updated (partial update).
#[cfg_attr(feature = "swagger-ui", utoipa::path(
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
pub async fn update_rule<S: StateStore + 'static>(
    State(state): State<Arc<RuleEngineState<S>>>,
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
            resolve_rules_revision(&state.pool, req.expected_revision, "PUT /api/rules/{id}")
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
        "audit": crate::infra::application_control::completion_audit_response(
            acceptance.completion_audit()
        ),
        "scheduler_refresh": scheduler_refresh_response(acceptance.runtime_status())
    }))))
}

/// Delete a rule and remove it from the scheduler.
///
/// Stops the scheduler from invoking this rule on the next tick, then removes
/// the row from the local `rules` table.
#[cfg_attr(feature = "swagger-ui", utoipa::path(
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
pub async fn delete_rule<S: StateStore + 'static>(
    State(state): State<Arc<RuleEngineState<S>>>,
    Path(id): Path<i64>,
    headers: HeaderMap,
    Json(request): Json<RuleMutationRequest>,
) -> Result<Json<SuccessResponse<serde_json::Value>>, AutomationError> {
    let rule_id = parse_rule_id(id)?;
    let expected_revision = resolve_rules_revision(
        &state.pool,
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
        "audit": crate::infra::application_control::completion_audit_response(
            acceptance.completion_audit()
        ),
        "scheduler_refresh": scheduler_refresh_response(acceptance.runtime_status())
    }))))
}

/// Enable a rule (joins the scheduler on the next tick).
///
/// Sets `enabled=true` in the `rules` table and refreshes the scheduler's
/// in-memory enabled set. The rule's next evaluation lands within
/// `tick_ms` (default 100ms). Convenience over PUT with `{"enabled":
/// true}`. Returns 404 if the rule id doesn't exist.
#[cfg_attr(feature = "swagger-ui", utoipa::path(
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
pub async fn enable_rule<S: StateStore + 'static>(
    State(state): State<Arc<RuleEngineState<S>>>,
    Path(id): Path<i64>,
    headers: HeaderMap,
    Json(request): Json<RuleMutationRequest>,
) -> Result<Json<SuccessResponse<serde_json::Value>>, AutomationError> {
    let rule_id = parse_rule_id(id)?;
    let expected_revision = resolve_rules_revision(
        &state.pool,
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
        "audit": crate::infra::application_control::completion_audit_response(
            acceptance.completion_audit()
        ),
        "scheduler_refresh": scheduler_refresh_response(acceptance.runtime_status())
    }))))
}

/// Disable a rule (skipped by the scheduler from the next tick on).
///
/// Sets `enabled=false`. The rule definition stays in the table — re-
/// enabling later picks up the same flow. Currently-running invocations
/// finish; subsequent ticks skip it. Use this to safely pause control
/// rules during maintenance without losing their definition.
#[cfg_attr(feature = "swagger-ui", utoipa::path(
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
pub async fn disable_rule<S: StateStore + 'static>(
    State(state): State<Arc<RuleEngineState<S>>>,
    Path(id): Path<i64>,
    headers: HeaderMap,
    Json(request): Json<RuleMutationRequest>,
) -> Result<Json<SuccessResponse<serde_json::Value>>, AutomationError> {
    let rule_id = parse_rule_id(id)?;
    let expected_revision = resolve_rules_revision(
        &state.pool,
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
        "audit": crate::infra::application_control::completion_audit_response(
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
    pool: &SqlitePool,
    requested: Option<u64>,
    endpoint: &'static str,
) -> Result<aether_ports::AutomationRulesRevision, AutomationError> {
    let value = match requested {
        Some(value) => value,
        None => {
            let current = current_rules_revision(pool).await?;
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

async fn current_rules_revision(pool: &SqlitePool) -> Result<u64, AutomationError> {
    let revision = sqlx::query_scalar::<_, i64>(
        "SELECT revision FROM configuration_revisions WHERE scope = 'automation_rules'",
    )
    .fetch_one(pool)
    .await
    .map_err(|error| {
        AutomationError::DatabaseError(format!("failed to read automation-rules revision: {error}"))
    })?;
    u64::try_from(revision).map_err(|_| {
        AutomationError::InternalError("automation-rules revision became negative".to_string())
    })
}

async fn rules_query_response<T: serde::Serialize>(
    pool: &SqlitePool,
    data: T,
) -> Result<Response, AutomationError> {
    let revision = current_rules_revision(pool).await?;
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

async fn apply_rule_mutation<S: StateStore + 'static>(
    state: &RuleEngineState<S>,
    headers: &HeaderMap,
    confirmed: bool,
    mutation: RevisionedRuleMutation,
) -> Result<aether_application::RuleMutationAcceptance, AutomationError> {
    let application = state.mutation_application.as_ref().ok_or_else(|| {
        AutomationError::DispatchDegraded("rule mutation application is not configured".to_string())
    })?;
    let authenticator = state.execution_authenticator.as_ref().ok_or_else(|| {
        AutomationError::DispatchDegraded(
            "rule mutation authentication is not configured".to_string(),
        )
    })?;
    let timestamp_ms = chrono::Utc::now().timestamp_millis().max(0) as u64;
    let invocation = crate::infra::application_control::command_invocation_from_headers(
        authenticator,
        headers,
        confirmed,
        aether_domain::TimestampMs::new(timestamp_ms),
    );
    let acceptance = application
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

/// Execute rule immediately (manual trigger)
///
/// Manually triggers a commissioned rule through the shared application API.
#[cfg_attr(feature = "swagger-ui", utoipa::path(
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
pub async fn execute_rule_now<S: StateStore + 'static>(
    Path(id): Path<i64>,
    State(state): State<Arc<RuleEngineState<S>>>,
    headers: HeaderMap,
    Json(request): Json<ExecuteRuleRequest>,
) -> Result<Json<SuccessResponse<serde_json::Value>>, AutomationError> {
    let application = state.execution_application.as_ref().ok_or_else(|| {
        AutomationError::DispatchDegraded(
            "manual rule execution application is not configured".to_string(),
        )
    })?;
    let authenticator = state.execution_authenticator.as_ref().ok_or_else(|| {
        AutomationError::DispatchDegraded(
            "manual rule execution authentication is not configured".to_string(),
        )
    })?;
    let rule_id = u64::try_from(id)
        .map(aether_domain::RuleId::new)
        .map_err(|_| AutomationError::InvalidData("rule id must be non-negative".to_string()))?;
    let timestamp_ms = chrono::Utc::now().timestamp_millis().max(0) as u64;
    let invocation = crate::infra::application_control::command_invocation_from_headers(
        authenticator,
        &headers,
        request.confirmed,
        aether_domain::TimestampMs::new(timestamp_ms),
    );
    let acceptance = application
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
        "audit": crate::infra::application_control::completion_audit_response(
            acceptance.completion_audit()
        ),
        "completed_at_ms": acceptance.completed_at().get()
    }))))
}

/// Rule scheduler runtime status.
///
/// Returns `running` flag, number of enabled / total rules, tick interval
/// (ms), last tick timestamp, max concurrency. Used by the operations
/// console to diagnose "rules aren't firing" — `running=false` or
/// `last_tick` stale by N×interval flags a hung scheduler.
#[cfg_attr(feature = "swagger-ui", utoipa::path(
    get,
    path = "/api/scheduler/status",
    responses(
        (status = 200, description = "Scheduler status", body = serde_json::Value)
    ),
    tag = "rules"
))]
pub async fn scheduler_status<S: StateStore + 'static>(
    State(state): State<Arc<RuleEngineState<S>>>,
) -> Result<Json<SuccessResponse<serde_json::Value>>, AutomationError> {
    let status = state.scheduler.status().await;

    Ok(Json(SuccessResponse::new(json!({
        "running": status.running,
        "total_rules": status.total_rules,
        "enabled_rules": status.enabled_rules,
        "tick_interval_ms": status.tick_interval_ms
    }))))
}

/// Force the scheduler to re-read rules from SQLite right now.
///
/// Normally the scheduler picks up rule changes after the next tick;
/// this endpoint forces an immediate reload, useful after bulk import
/// or `aether sync` so admins don't wait. Doesn't restart in-flight
/// invocations, just refreshes the enabled set the next tick will use.
#[cfg_attr(feature = "swagger-ui", utoipa::path(
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
pub async fn scheduler_reload<S: StateStore + 'static>(
    State(state): State<Arc<RuleEngineState<S>>>,
    headers: HeaderMap,
    Json(request): Json<RuleMutationRequest>,
) -> Result<Json<SuccessResponse<serde_json::Value>>, AutomationError> {
    let expected_revision = resolve_rules_revision(
        &state.pool,
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
    let count = state.scheduler.status().await.enabled_rules;
    info!("Scheduler reloaded {} rules", count);
    Ok(Json(SuccessResponse::new(json!({
        "status": "OK",
        "rules_loaded": count,
        "resulting_revision": acceptance.resulting_revision().get(),
        "request_id": acceptance.request_id(),
        "audit": crate::infra::application_control::completion_audit_response(
            acceptance.completion_audit()
        ),
        "scheduler_refresh": scheduler_refresh_response(acceptance.runtime_status())
    }))))
}

/// Get rule variables for monitoring
///
/// Returns all variable definitions from a rule's nodes, which can be used
/// for WebSocket monitoring to display real-time variable values.
#[cfg_attr(feature = "swagger-ui", utoipa::path(
    get,
    path = "/api/rules/{id}/variables",
    params(("id" = i64, Path, description = "Rule identifier")),
    responses(
        (status = 200, description = "Rule variables", body = serde_json::Value)
    ),
    tag = "rules"
))]
pub async fn get_rule_variables<S: StateStore + 'static>(
    State(state): State<Arc<RuleEngineState<S>>>,
    Path(id): Path<i64>,
) -> Result<Json<SuccessResponse<serde_json::Value>>, AutomationError> {
    // Get the rule from database
    let rule = rule_repository::get_rule_for_execution(&state.pool, id).await?;

    // Extract all variables from nodes
    let mut variables: Vec<RuleVariable> = Vec::new();

    for node in rule.flow.nodes.values() {
        match node {
            RuleNode::Switch {
                variables: vars, ..
            } => {
                variables.extend(vars.iter().cloned());
            },
            RuleNode::ChangeValue {
                variables: vars, ..
            } => {
                variables.extend(vars.iter().cloned());
            },
            _ => {},
        }
    }

    // Deduplicate by variable name (sort + dedup to avoid clone)
    variables.sort_by(|a, b| a.name.cmp(&b.name));
    variables.dedup_by(|a, b| a.name == b.name);

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
