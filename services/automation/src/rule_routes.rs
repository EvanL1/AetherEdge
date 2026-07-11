//! Rule Engine API Routes
//!
//! Provides Vue Flow-based rule management and execution endpoints.
//! These routes are integrated into automation and served on port 6002.

#![allow(clippy::disallowed_methods)] // json! macro used in multiple functions

use crate::error::AutomationError;
use aether_calc::StateStore;
use aether_rules::{self as rule_repository, RuleNode, RuleScheduler, RuleVariable, TriggerConfig};
use axum::{
    Router,
    extract::{Path, Query, State},
    response::Json,
    routing::{get, post},
};
use common::{PaginatedResponse, SuccessResponse};
use serde_json::json;
use sqlx::SqlitePool;
use std::sync::Arc;
use tracing::{debug, error, info, warn};
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
}

impl<S: StateStore + 'static> RuleEngineState<S> {
    pub fn new(pool: SqlitePool, scheduler: Arc<RuleScheduler<S>>) -> Self {
        Self { pool, scheduler }
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
    paths(list_rules, create_rule, get_rule, update_rule, delete_rule, enable_rule, disable_rule, execute_rule_now, scheduler_status, scheduler_reload),
    components(
        schemas(
            CreateRuleRequest,
            UpdateRuleRequest,
            RuleListQuery,
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
/// Common use case: Calculate daily/weekly/monthly charge/discharge energy from cumulative meters.
///
/// # Example Use Cases
/// - **Daily Charge Energy**: Input from total charge counter (ID 9), output to daily counter (ID 101)
/// - **Monthly Discharge Energy**: Track monthly discharge for billing
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "swagger-ui", derive(utoipa::ToSchema))]
pub struct PeriodDeltaNodeSchema {
    /// Node type identifier (always "action-periodDelta")
    #[serde(rename = "type")]
    #[cfg_attr(feature = "swagger-ui", schema(example = "action-periodDelta"))]
    pub node_type: String,

    /// Input variable - source cumulative value (e.g., total charge energy)
    pub input: RuleVariableSchema,

    /// Output variable - period delta result (e.g., daily charge energy)
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
    #[cfg_attr(feature = "swagger-ui", schema(example = "Daily Charge Energy"))]
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
/// | 9 (Charge Energy) | 101 (Daily Charge) | daily |
/// | 9 (Charge Energy) | 103 (Weekly Charge) | weekly |
/// | 10 (Discharge Energy) | 102 (Daily Discharge) | daily |
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "swagger-ui", derive(utoipa::ToSchema))]
pub struct PeriodDeltaConfigSchema {
    /// Input variable (cumulative source, e.g., total charge energy from meter)
    pub input: RuleVariableSchema,

    /// Output variable (period delta destination, e.g., daily charge energy)
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
    #[cfg_attr(feature = "swagger-ui", schema(example = "Battery SOC Protection"))]
    pub name: String,

    /// Rule description (optional)
    #[cfg_attr(
        feature = "swagger-ui",
        schema(example = "Protect battery when SOC is too low")
    )]
    pub description: Option<String>,
}

/// Request DTO for updating an existing rule (all fields optional, partial update)
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
#[cfg_attr(feature = "swagger-ui", derive(utoipa::ToSchema))]
pub struct UpdateRuleRequest {
    /// Rule name (optional)
    #[cfg_attr(feature = "swagger-ui", schema(example = "Battery SOC Protection v2"))]
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
) -> Result<Json<SuccessResponse<PaginatedResponse<serde_json::Value>>>, AutomationError> {
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
            Ok(Json(SuccessResponse::new(paginated)))
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
        description = "Rule metadata (ID auto-generated)"
    ),
    responses(
        (status = 200, description = "Rule created successfully", body = serde_json::Value,
         example = json!({ "success": true, "data": { "id": "1", "name": "Battery Protection", "status": "created" } }))
    ),
    tag = "rules"
))]
pub async fn create_rule<S: StateStore + 'static>(
    State(state): State<Arc<RuleEngineState<S>>>,
    Json(req): Json<CreateRuleRequest>,
) -> Result<Json<SuccessResponse<serde_json::Value>>, AutomationError> {
    // Insert empty rule record (metadata only, no flow)
    // Let SQLite auto-assign INTEGER PRIMARY KEY to avoid TOCTOU race
    let result = sqlx::query(
        r#"
        INSERT INTO rules (name, description, nodes_json, flow_json, format, enabled, priority, cooldown_ms)
        VALUES (?, ?, '{}', NULL, 'vue-flow', FALSE, 0, 0)
        "#,
    )
    .bind(&req.name)
    .bind(&req.description)
    .execute(&state.pool)
    .await
    .map_err(|e| {
        error!("Create rule: {}", e);
        AutomationError::InternalError("Failed to create rule".to_string())
    })?;

    let new_id = result.last_insert_rowid();

    // Reload scheduler to pick up new rule
    if let Err(e) = state.scheduler.reload_rules().await {
        warn!("Reload scheduler: {}", e);
    }

    debug!("Rule created: {} ({})", req.name, new_id);
    Ok(Json(SuccessResponse::new(json!({
        "id": new_id,
        "name": req.name,
        "status": "created"
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
) -> Result<Json<SuccessResponse<serde_json::Value>>, AutomationError> {
    match rule_repository::get_rule(&state.pool, id).await {
        Ok(rule) => Ok(Json(SuccessResponse::new(rule))),
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
    params(("id" = i64, Path, description = "Rule ID")),
    request_body(
        content = UpdateRuleRequest,
        description = "Fields to update (only provided fields are updated)"
    ),
    responses(
        (status = 200, description = "Rule updated successfully", body = serde_json::Value,
         example = json!({ "success": true, "data": { "id": "1", "status": "updated" } })),
        (status = 404, description = "Rule not found")
    ),
    tag = "rules"
))]
pub async fn update_rule<S: StateStore + 'static>(
    State(state): State<Arc<RuleEngineState<S>>>,
    Path(id): Path<i64>,
    Json(req): Json<UpdateRuleRequest>,
) -> Result<Json<SuccessResponse<serde_json::Value>>, AutomationError> {
    // Check rule exists (properly propagate database errors)
    let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM rules WHERE id = ?)")
        .bind(id)
        .fetch_one(&state.pool)
        .await
        .map_err(|e| {
            AutomationError::DatabaseError(format!("Failed to check rule existence: {}", e))
        })?;

    if !exists {
        return Err(AutomationError::RuleNotFound(id.to_string()));
    }

    // Build dynamic UPDATE query for provided fields only (partial update)
    // SAFETY: All field names are hardcoded strings, not user input.
    // Values are bound via .bind() which prevents SQL injection.
    let mut updates = Vec::new();

    if req.name.is_some() {
        updates.push("name = ?");
    }
    if req.description.is_some() {
        updates.push("description = ?");
    }
    if req.enabled.is_some() {
        updates.push("enabled = ?");
    }
    if req.priority.is_some() {
        updates.push("priority = ?");
    }
    if req.cooldown_ms.is_some() {
        updates.push("cooldown_ms = ?");
    }
    if req.flow_json.is_some() {
        updates.push("flow_json = ?");
        updates.push("nodes_json = ?"); // Also update compact format for execution
    }
    if req.trigger_config.is_some() {
        updates.push("trigger_config = ?");
    }

    if updates.is_empty() {
        return Err(AutomationError::InvalidRule(
            "No fields to update".to_string(),
        ));
    }

    let sql = format!("UPDATE rules SET {} WHERE id = ?", updates.join(", "));
    let mut query = sqlx::query(&sql);

    // Bind values in order
    if let Some(name) = &req.name {
        query = query.bind(name);
    }
    if let Some(desc) = &req.description {
        query = query.bind(desc);
    }
    if let Some(enabled) = req.enabled {
        query = query.bind(enabled);
    }
    if let Some(priority) = req.priority {
        query = query.bind(priority as i64);
    }
    if let Some(cooldown) = req.cooldown_ms {
        query = query.bind(cooldown as i64);
    }
    if let Some(flow) = &req.flow_json {
        // Both flow columns come from the single sanctioned producer so
        // flow_json/nodes_json can never diverge. Bind order matches the
        // "flow_json = ?, nodes_json = ?" pushes above.
        let columns = aether_rules::flow_column_values(flow)
            .map_err(|e| AutomationError::ParseError(e.to_string()))?;
        query = query.bind(columns.flow_json);
        query = query.bind(columns.nodes_json);
    }
    if let Some(trig) = &req.trigger_config {
        // Validate by parsing into the strongly-typed enum; reject malformed
        // configs at the API boundary rather than at scheduler load time.
        let _: TriggerConfig = serde_json::from_value(trig.clone())
            .map_err(|e| AutomationError::InvalidRule(format!("Invalid trigger_config: {}", e)))?;
        let trig_str = serde_json::to_string(trig)
            .map_err(|e| AutomationError::SerializationError(e.to_string()))?;
        query = query.bind(trig_str);
    }
    query = query.bind(id);

    if let Err(e) = query.execute(&state.pool).await {
        error!("Update rule {}: {}", id, e);
        return Err(AutomationError::InternalError(format!(
            "Failed to update rule in database: {}",
            e
        )));
    }

    // Reload scheduler to pick up changes
    if let Err(e) = state.scheduler.reload_rules().await {
        warn!("Reload scheduler: {}", e);
    }

    debug!("Rule {} updated", id);
    Ok(Json(SuccessResponse::new(json!({
        "id": id,
        "status": "updated"
    }))))
}

/// Delete a rule and remove it from the scheduler.
///
/// Stops the scheduler from invoking this rule on the next tick, then removes
/// the row from the local `rules` table.
#[cfg_attr(feature = "swagger-ui", utoipa::path(
    delete,
    path = "/api/rules/{id}",
    params(("id" = i64, Path, description = "Rule identifier")),
    responses(
        (status = 200, description = "Rule deleted", body = serde_json::Value)
    ),
    tag = "rules"
))]
pub async fn delete_rule<S: StateStore + 'static>(
    State(state): State<Arc<RuleEngineState<S>>>,
    Path(id): Path<i64>,
) -> Result<Json<SuccessResponse<serde_json::Value>>, AutomationError> {
    if let Err(e) = rule_repository::delete_rule(&state.pool, id).await {
        error!("Delete rule {}: {}", id, e);
        return Err(AutomationError::InternalError(
            "Failed to delete rule".to_string(),
        ));
    }

    // Reload scheduler to remove the rule
    if let Err(e) = state.scheduler.reload_rules().await {
        warn!("Reload scheduler: {}", e);
    }

    debug!("Rule {} deleted", id);
    Ok(Json(SuccessResponse::new(
        json!({ "id": id, "status": "OK" }),
    )))
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
    params(("id" = i64, Path, description = "Rule identifier")),
    responses(
        (status = 200, description = "Rule enabled", body = serde_json::Value)
    ),
    tag = "rules"
))]
pub async fn enable_rule<S: StateStore + 'static>(
    State(state): State<Arc<RuleEngineState<S>>>,
    Path(id): Path<i64>,
) -> Result<Json<SuccessResponse<serde_json::Value>>, AutomationError> {
    if let Err(e) = rule_repository::set_rule_enabled(&state.pool, id, true).await {
        error!("Failed to enable rule {}: {}", id, e);
        return Err(AutomationError::InternalError(
            "Failed to enable rule".to_string(),
        ));
    }

    // Reload scheduler
    if let Err(e) = state.scheduler.reload_rules().await {
        warn!("Failed to reload scheduler after rule enable: {}", e);
    }

    info!("Enabled rule: {}", id);
    Ok(Json(SuccessResponse::new(
        json!({ "id": id, "status": "OK" }),
    )))
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
    params(("id" = i64, Path, description = "Rule identifier")),
    responses(
        (status = 200, description = "Rule disabled", body = serde_json::Value)
    ),
    tag = "rules"
))]
pub async fn disable_rule<S: StateStore + 'static>(
    State(state): State<Arc<RuleEngineState<S>>>,
    Path(id): Path<i64>,
) -> Result<Json<SuccessResponse<serde_json::Value>>, AutomationError> {
    if let Err(e) = rule_repository::set_rule_enabled(&state.pool, id, false).await {
        error!("Failed to disable rule {}: {}", id, e);
        return Err(AutomationError::InternalError(
            "Failed to disable rule".to_string(),
        ));
    }

    // Reload scheduler
    if let Err(e) = state.scheduler.reload_rules().await {
        warn!("Failed to reload scheduler after rule disable: {}", e);
    }

    info!("Disabled rule: {}", id);
    Ok(Json(SuccessResponse::new(
        json!({ "id": id, "status": "OK" }),
    )))
}

/// Execute rule immediately (manual trigger)
///
/// Manually trigger rule execution, returns execution result and list of executed actions.
#[cfg_attr(feature = "swagger-ui", utoipa::path(
    post,
    path = "/api/rules/{id}/execute",
    params(("id" = i64, Path, description = "Rule ID")),
    responses(
        (status = 200, description = "Rule execution result", body = serde_json::Value,
         example = json!({
             "success": true,
             "data": {
                 "result": "executed",
                 "rule_id": "soc-strategy-001",
                 "execution_id": "manual-a1b2c3d4",
                 "success": true,
                 "actions_executed": [
                     { "target_type": "instance", "target_id": "pv_01", "point_type": "action", "point_id": 5, "value": 78.0, "success": true }
                 ],
                 "execution_path": ["start", "switch-soc", "action-high", "end"],
                 "timestamp": "2024-01-01T12:00:00Z"
             }
         }))
    ),
    tag = "rules"
))]
pub async fn execute_rule_now<S: StateStore + 'static>(
    Path(id): Path<i64>,
    State(state): State<Arc<RuleEngineState<S>>>,
) -> Result<Json<SuccessResponse<serde_json::Value>>, AutomationError> {
    let execution_id = format!("manual-{}", uuid::Uuid::new_v4());
    let timestamp = chrono::Utc::now();

    // Execute through scheduler (which handles rule loading)
    let result = state.scheduler.execute_rule(id).await?;

    // Format action results for response
    let action_results: Vec<serde_json::Value> = result
        .actions_executed
        .iter()
        .map(|a| {
            json!({
                "target_type": a.target_type,
                "target_id": a.target_id,
                "point_type": a.point_type,
                "point_id": a.point_id,
                "value": a.value,
                "success": a.success
            })
        })
        .collect();

    // Build unified response (include error field only on failure)
    let mut response = json!({
        "result": if result.success { "executed" } else { "failed" },
        "rule_id": result.rule_id,
        "execution_id": execution_id,
        "success": result.success,
        "actions_executed": action_results,
        "execution_path": result.execution_path,
        "timestamp": timestamp
    });
    if !result.success {
        response["error"] = json!(result.error);
    }
    Ok(Json(SuccessResponse::new(response)))
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
    responses(
        (status = 200, description = "Rules reloaded", body = serde_json::Value)
    ),
    tag = "rules"
))]
pub async fn scheduler_reload<S: StateStore + 'static>(
    State(state): State<Arc<RuleEngineState<S>>>,
) -> Result<Json<SuccessResponse<serde_json::Value>>, AutomationError> {
    match state.scheduler.reload_rules().await {
        Ok(count) => {
            info!("Scheduler reloaded {} rules", count);
            Ok(Json(SuccessResponse::new(json!({
                "status": "OK",
                "rules_loaded": count
            }))))
        },
        Err(e) => {
            error!("Failed to reload scheduler: {}", e);
            Err(AutomationError::SchedulerError(format!(
                "Failed to reload rules: {}",
                e
            )))
        },
    }
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
