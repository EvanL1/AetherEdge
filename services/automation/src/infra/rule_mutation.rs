//! SQLite-backed automation-rule mutation adapter.

use std::sync::Arc;

use aether_calc::StateStore;
use aether_domain::RuleId;
use aether_ports::{
    AutomationRuleMutator, PortError, PortErrorKind, PortResult, RuleMutation, RuleMutationReceipt,
};
use aether_rules::{RuleScheduler, TriggerConfig};
use async_trait::async_trait;
use serde_json::Value;
use sqlx::SqlitePool;

/// Owns durable rule mutation and the corresponding scheduler refresh.
pub struct SqliteRuleMutator<S: StateStore> {
    pool: SqlitePool,
    scheduler: Arc<RuleScheduler<S>>,
}

impl<S: StateStore> SqliteRuleMutator<S> {
    /// Creates the adapter over the rule database and active scheduler.
    #[must_use]
    pub fn new(pool: SqlitePool, scheduler: Arc<RuleScheduler<S>>) -> Self {
        Self { pool, scheduler }
    }
}

#[async_trait]
impl<S: StateStore + 'static> AutomationRuleMutator for SqliteRuleMutator<S> {
    async fn mutate(&self, mutation: RuleMutation) -> PortResult<RuleMutationReceipt> {
        let kind = mutation.kind();
        let rule_id = match mutation {
            RuleMutation::Create { name, description } => {
                let result = sqlx::query(
                    "INSERT INTO rules \
                     (name, description, nodes_json, flow_json, format, enabled, priority, cooldown_ms) \
                     VALUES (?, ?, '{}', NULL, 'vue-flow', FALSE, 0, 0)",
                )
                .bind(name)
                .bind(description)
                .execute(&self.pool)
                .await
                .map_err(database_error)?;
                let id = u64::try_from(result.last_insert_rowid()).map_err(|_| {
                    PortError::new(
                        PortErrorKind::Permanent,
                        "SQLite returned a negative rule identifier",
                    )
                })?;
                RuleId::new(id)
            },
            RuleMutation::Update {
                rule_id,
                name,
                description,
                enabled,
                priority,
                cooldown_ms,
                flow_json,
                trigger_config,
            } => {
                let mut updates = Vec::new();
                if name.is_some() {
                    updates.push("name = ?");
                }
                if description.is_some() {
                    updates.push("description = ?");
                }
                if enabled.is_some() {
                    updates.push("enabled = ?");
                }
                if priority.is_some() {
                    updates.push("priority = ?");
                }
                if cooldown_ms.is_some() {
                    updates.push("cooldown_ms = ?");
                }

                let flow_columns = flow_json
                    .as_deref()
                    .map(|flow| {
                        let value: Value = serde_json::from_str(flow).map_err(|error| {
                            PortError::new(
                                PortErrorKind::InvalidData,
                                format!("invalid flow JSON: {error}"),
                            )
                        })?;
                        aether_rules::flow_column_values(&value).map_err(|error| {
                            PortError::new(PortErrorKind::InvalidData, error.to_string())
                        })
                    })
                    .transpose()?;
                if flow_columns.is_some() {
                    updates.push("flow_json = ?");
                    updates.push("nodes_json = ?");
                }

                if trigger_config.is_some() {
                    updates.push("trigger_config = ?");
                }
                let trigger_config = trigger_config
                    .map(|trigger| {
                        serde_json::from_str::<TriggerConfig>(&trigger).map_err(|error| {
                            PortError::new(
                                PortErrorKind::InvalidData,
                                format!("invalid trigger configuration: {error}"),
                            )
                        })?;
                        Ok(trigger)
                    })
                    .transpose()?;

                if updates.is_empty() {
                    return Err(PortError::new(
                        PortErrorKind::InvalidData,
                        "no rule fields were provided",
                    ));
                }

                let database_id = database_rule_id(rule_id)?;
                let sql = format!(
                    "UPDATE rules SET {}, updated_at = CURRENT_TIMESTAMP WHERE id = ?",
                    updates.join(", ")
                );
                let mut query = sqlx::query(&sql);
                if let Some(name) = name {
                    query = query.bind(name);
                }
                if let Some(description) = description {
                    query = query.bind(description);
                }
                if let Some(enabled) = enabled {
                    query = query.bind(enabled);
                }
                if let Some(priority) = priority {
                    query = query.bind(i64::from(priority));
                }
                if let Some(cooldown_ms) = cooldown_ms {
                    let cooldown_ms = i64::try_from(cooldown_ms).map_err(|_| {
                        PortError::new(
                            PortErrorKind::InvalidData,
                            "cooldown exceeds SQLite INTEGER range",
                        )
                    })?;
                    query = query.bind(cooldown_ms);
                }
                if let Some(columns) = flow_columns {
                    query = query.bind(columns.flow_json).bind(columns.nodes_json);
                }
                if let Some(trigger_config) = trigger_config {
                    query = query.bind(trigger_config);
                }
                let result = query
                    .bind(database_id)
                    .execute(&self.pool)
                    .await
                    .map_err(database_error)?;
                require_affected_rule(result.rows_affected(), rule_id)?;
                rule_id
            },
            RuleMutation::SetEnabled { rule_id, enabled } => {
                let result = sqlx::query(
                    "UPDATE rules SET enabled = ?, updated_at = CURRENT_TIMESTAMP WHERE id = ?",
                )
                .bind(enabled)
                .bind(database_rule_id(rule_id)?)
                .execute(&self.pool)
                .await
                .map_err(database_error)?;
                require_affected_rule(result.rows_affected(), rule_id)?;
                rule_id
            },
            RuleMutation::Delete { rule_id } => {
                let result = sqlx::query("DELETE FROM rules WHERE id = ?")
                    .bind(database_rule_id(rule_id)?)
                    .execute(&self.pool)
                    .await
                    .map_err(database_error)?;
                require_affected_rule(result.rows_affected(), rule_id)?;
                rule_id
            },
            RuleMutation::Reload => {
                return match self.scheduler.reload_rules().await {
                    Ok(_) => Ok(RuleMutationReceipt::reload()),
                    Err(error) => {
                        self.scheduler.stop();
                        Ok(RuleMutationReceipt::scheduler_stopped(
                            None,
                            kind,
                            PortError::new(
                                PortErrorKind::Unavailable,
                                format!("rule scheduler reload failed: {error}"),
                            ),
                        ))
                    },
                };
            },
        };

        match self.scheduler.reload_rules().await {
            Ok(_) => Ok(RuleMutationReceipt::new(rule_id, kind)),
            Err(error) => {
                self.scheduler.stop();
                Ok(RuleMutationReceipt::scheduler_stopped(
                    Some(rule_id),
                    kind,
                    PortError::new(
                        PortErrorKind::Unavailable,
                        format!("rule scheduler reload failed: {error}"),
                    ),
                ))
            },
        }
    }
}

fn database_rule_id(rule_id: RuleId) -> PortResult<i64> {
    i64::try_from(rule_id.get()).map_err(|_| {
        PortError::new(
            PortErrorKind::InvalidData,
            "rule identifier exceeds SQLite INTEGER range",
        )
    })
}

fn require_affected_rule(rows_affected: u64, rule_id: RuleId) -> PortResult<()> {
    if rows_affected == 0 {
        return Err(PortError::new(
            PortErrorKind::NotFound,
            format!("rule {} does not exist", rule_id.get()),
        ));
    }
    Ok(())
}

fn database_error(error: sqlx::Error) -> PortError {
    PortError::new(
        PortErrorKind::Unavailable,
        format!("rule database mutation failed: {error}"),
    )
}
