//! Rules Repository - SQLite persistence for rules
//!
//! Stores rules with compact flow topology (simplified Vue Flow structure)

use crate::error::{Result, RuleError};
use crate::parser::flow_column_values;
use crate::types::{Rule, RuleFlow};
use serde_json::Value;
use sqlx::{Row, SqlitePool, sqlite::SqliteRow};

/// List all rules (returns metadata and flow_json for frontend editing)
pub async fn list_rules(pool: &SqlitePool) -> Result<Vec<Value>> {
    let rows = sqlx::query(
        r#"
        SELECT id, name, description, nodes_json, flow_json, format, enabled, priority, cooldown_ms, trigger_config
        FROM rules
        ORDER BY priority DESC, id ASC
        "#,
    )
    .fetch_all(pool)
    .await?;

    let mut rules = Vec::with_capacity(rows.len());
    for row in rows {
        rules.push(hydrate_rule_json(row)?);
    }
    Ok(rules)
}

/// List rules with pagination, returning rules and total count
pub async fn list_rules_paginated(
    pool: &SqlitePool,
    page: usize,
    page_size: usize,
) -> Result<(Vec<Value>, usize)> {
    // Clamp inputs to reasonable bounds
    let page = page.max(1);
    let page_size = page_size.clamp(1, 100);
    let offset = (page - 1) * page_size;

    // Total count
    let total: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM rules")
        .fetch_one(pool)
        .await?;

    // Paged rows
    let rows = sqlx::query(
        r#"
        SELECT id, name, description, nodes_json, flow_json, format, enabled, priority, cooldown_ms, trigger_config
        FROM rules
        ORDER BY priority DESC, id ASC
        LIMIT ? OFFSET ?
        "#,
    )
    .bind(page_size as i64)
    .bind(offset as i64)
    .fetch_all(pool)
    .await?;

    let mut rules = Vec::with_capacity(rows.len());
    for row in rows {
        rules.push(hydrate_rule_json(row)?);
    }

    Ok((rules, total as usize))
}

/// Get a single rule by ID (returns metadata and flow_json for frontend editing)
pub async fn get_rule(pool: &SqlitePool, id: i64) -> Result<Value> {
    let row = sqlx::query(
        r#"
        SELECT id, name, description, nodes_json, flow_json, format, enabled, priority, cooldown_ms, trigger_config
        FROM rules
        WHERE id = ?
        "#,
    )
    .bind(id)
    .fetch_optional(pool)
    .await?;

    match row {
        Some(row) => hydrate_rule_json(row),
        None => Err(RuleError::NotFound(id.to_string())),
    }
}

/// Get a rule for execution (returns parsed Rule struct)
pub async fn get_rule_for_execution(pool: &SqlitePool, id: i64) -> Result<Rule> {
    let row = sqlx::query(
        r#"
        SELECT id, name, description, enabled, priority, cooldown_ms, trigger_config, nodes_json
        FROM rules
        WHERE id = ? AND enabled = 1
        "#,
    )
    .bind(id)
    .fetch_optional(pool)
    .await?;

    match row {
        Some(row) => hydrate_rule(row),
        None => Err(RuleError::NotFound(id.to_string())),
    }
}

/// Load all enabled rules for execution
pub async fn load_enabled_rules(pool: &SqlitePool) -> Result<Vec<Rule>> {
    let rows = sqlx::query(
        r#"
        SELECT id, name, description, enabled, priority, cooldown_ms, trigger_config, nodes_json
        FROM rules
        WHERE enabled = 1
        ORDER BY priority DESC, id ASC
        "#,
    )
    .fetch_all(pool)
    .await?;

    let mut rules = Vec::with_capacity(rows.len());
    for row in rows {
        rules.push(hydrate_rule(row)?);
    }
    Ok(rules)
}

/// Load all rules (including disabled) for hot reload
pub async fn load_all_rules(pool: &SqlitePool) -> Result<Vec<Rule>> {
    let rows = sqlx::query(
        r#"
        SELECT id, name, description, enabled, priority, cooldown_ms, trigger_config, nodes_json
        FROM rules
        ORDER BY priority DESC, id ASC
        "#,
    )
    .fetch_all(pool)
    .await?;

    let mut rules = Vec::with_capacity(rows.len());
    for row in rows {
        rules.push(hydrate_rule(row)?);
    }
    Ok(rules)
}

/// Upsert a rule - extracts compact flow and stores both original and parsed data
pub async fn upsert_rule(pool: &SqlitePool, rule_id: i64, rule: &Value) -> Result<()> {
    // Extract metadata from the incoming JSON
    let name = rule
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or(&rule_id.to_string())
        .to_string();

    let description = rule
        .get("description")
        .and_then(Value::as_str)
        .map(|s| s.to_string());

    let enabled = rule.get("enabled").and_then(Value::as_bool).unwrap_or(true);

    let priority = rule
        .get("priority")
        .and_then(Value::as_i64)
        .and_then(|n| u32::try_from(n).ok())
        .unwrap_or(0);

    let cooldown_ms = rule
        .get("cooldown_ms")
        .and_then(Value::as_i64)
        .and_then(|n| u64::try_from(n).ok())
        .unwrap_or(0);

    // Get format type (default: "vue-flow")
    let format = rule
        .get("format")
        .and_then(Value::as_str)
        .unwrap_or("vue-flow")
        .to_string();

    // Both flow columns come from the single sanctioned producer
    // (parser::flow_column_values) so they can never diverge. The source is
    // the "flow_json" field, or the entire rule object for legacy
    // compact-only rules — those keep storing NULL flow_json.
    let flow_json_value = rule.get("flow_json");
    let columns = flow_column_values(flow_json_value.unwrap_or(rule))?;
    let flow_json_str = flow_json_value.map(|_| columns.flow_json);
    let nodes_json = columns.nodes_json;

    sqlx::query(
        r#"
        INSERT INTO rules (id, name, description, nodes_json, flow_json, format, enabled, priority, cooldown_ms)
        VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
        ON CONFLICT(id) DO UPDATE SET
            name = excluded.name,
            description = excluded.description,
            nodes_json = excluded.nodes_json,
            flow_json = excluded.flow_json,
            format = excluded.format,
            enabled = excluded.enabled,
            priority = excluded.priority,
            cooldown_ms = excluded.cooldown_ms,
            updated_at = CURRENT_TIMESTAMP
        "#,
    )
    .bind(rule_id)
    .bind(&name)
    .bind(description)
    .bind(&nodes_json)
    .bind(flow_json_str)
    .bind(&format)
    .bind(enabled)
    .bind(priority as i64)
    .bind(cooldown_ms as i64)
    .execute(pool)
    .await?;

    Ok(())
}

/// Delete a rule
pub async fn delete_rule(pool: &SqlitePool, id: i64) -> Result<()> {
    let result = sqlx::query("DELETE FROM rules WHERE id = ?")
        .bind(id)
        .execute(pool)
        .await?;

    if result.rows_affected() == 0 {
        return Err(RuleError::NotFound(id.to_string()));
    }

    Ok(())
}

/// Enable or disable a rule
pub async fn set_rule_enabled(pool: &SqlitePool, id: i64, enabled: bool) -> Result<()> {
    let result = sqlx::query(
        r#"
        UPDATE rules
        SET enabled = ?, updated_at = CURRENT_TIMESTAMP
        WHERE id = ?
        "#,
    )
    .bind(enabled)
    .bind(id)
    .execute(pool)
    .await?;

    if result.rows_affected() == 0 {
        return Err(RuleError::NotFound(id.to_string()));
    }

    Ok(())
}

/// Hydrate a row into JSON Value (for API response)
#[allow(clippy::disallowed_methods)] // json! macro internal unwrap is safe for static structure
fn hydrate_rule_json(row: SqliteRow) -> Result<Value> {
    let id: i64 = row.try_get("id")?;
    let name: String = row.try_get("name")?;
    let description: Option<String> = row.try_get("description")?;
    let nodes_json_str: String = row.try_get("nodes_json")?;
    let flow_json_str: Option<String> = row.try_get("flow_json")?;
    let format: Option<String> = row.try_get("format")?;
    let enabled: i64 = row.try_get("enabled")?;
    let priority: i64 = row.try_get("priority")?;
    let cooldown_ms: i64 = row.try_get("cooldown_ms")?;
    let trigger_config_str: Option<String> = row.try_get("trigger_config")?;

    // Parse compact flow (for execution info)
    let flow: Value = serde_json::from_str(&nodes_json_str)
        .map_err(|e| RuleError::SerializationError(format!("nodes_json: {}", e)))?;

    // Parse original flow_json (for frontend editing) if available
    let flow_json: Option<Value> = flow_json_str
        .as_ref()
        .map(|s| serde_json::from_str(s))
        .transpose()
        .map_err(|e| RuleError::SerializationError(format!("flow_json: {}", e)))?;

    // Parse trigger_config JSON column into an object so the frontend can
    // edit it directly without a second decode step.
    let trigger_config: Option<Value> = trigger_config_str
        .as_ref()
        .map(|s| serde_json::from_str(s))
        .transpose()
        .map_err(|e| RuleError::SerializationError(format!("trigger_config: {}", e)))?;

    Ok(serde_json::json!({
        "id": id,
        "name": name,
        "description": description,
        "format": format.unwrap_or_else(|| "vue-flow".to_string()),
        "enabled": enabled != 0,
        "priority": priority,
        "cooldown_ms": cooldown_ms,
        "flow": flow,
        "flow_json": flow_json,
        "trigger_config": trigger_config
    }))
}

/// Hydrate a row into Rule struct (for execution)
fn hydrate_rule(row: SqliteRow) -> Result<Rule> {
    let id: i64 = row.try_get("id")?;
    let name: String = row.try_get("name")?;
    let description: Option<String> = row.try_get("description")?;
    let enabled: i64 = row.try_get("enabled")?;
    let priority: i64 = row.try_get("priority")?;
    let cooldown_ms: i64 = row.try_get("cooldown_ms")?;
    let trigger_config: Option<String> = row.try_get("trigger_config")?;
    let nodes_json_str: String = row.try_get("nodes_json")?;

    // Deserialize compact flow
    let flow: RuleFlow = serde_json::from_str(&nodes_json_str)
        .map_err(|e| RuleError::SerializationError(format!("nodes_json: {}", e)))?;

    Ok(Rule {
        id,
        name,
        description,
        enabled: enabled != 0,
        priority: u32::try_from(priority).unwrap_or(0),
        cooldown_ms: u64::try_from(cooldown_ms).unwrap_or(0),
        trigger_config,
        flow,
    })
}
