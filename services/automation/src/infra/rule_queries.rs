//! Narrow read seam for persisted rules and scheduler diagnostics.
//!
//! The HTTP adapter consumes these read models without owning SQLite or the
//! concrete scheduler.

use std::sync::Arc;

use aether_calc::MemoryStateStore;
use aether_rules::{RuleNode, RuleScheduler, RuleVariable, SchedulerStatus};
use sqlx::SqlitePool;

use crate::error::AutomationError;

pub struct RuleQueries {
    pool: SqlitePool,
    scheduler: Arc<RuleScheduler<MemoryStateStore>>,
}

impl RuleQueries {
    #[must_use]
    pub fn new(pool: SqlitePool, scheduler: Arc<RuleScheduler<MemoryStateStore>>) -> Self {
        Self { pool, scheduler }
    }

    pub async fn current_revision(&self) -> Result<u64, AutomationError> {
        let revision = sqlx::query_scalar::<_, i64>(
            "SELECT revision FROM configuration_revisions WHERE scope = 'automation_rules'",
        )
        .fetch_one(&self.pool)
        .await
        .map_err(|error| {
            AutomationError::DatabaseError(format!(
                "failed to read automation-rules revision: {error}"
            ))
        })?;
        u64::try_from(revision).map_err(|_| {
            AutomationError::InternalError("automation-rules revision became negative".to_string())
        })
    }

    pub async fn list_rules_paginated(
        &self,
        page: usize,
        page_size: usize,
    ) -> Result<(Vec<serde_json::Value>, usize), AutomationError> {
        aether_rules::list_rules_paginated(&self.pool, page, page_size)
            .await
            .map_err(AutomationError::from)
    }

    pub async fn get_rule(&self, id: i64) -> Result<serde_json::Value, AutomationError> {
        aether_rules::get_rule(&self.pool, id)
            .await
            .map_err(AutomationError::from)
    }

    pub async fn scheduler_status(&self) -> SchedulerStatus {
        self.scheduler.status().await
    }

    pub async fn rule_variables(&self, id: i64) -> Result<Vec<RuleVariable>, AutomationError> {
        let rule = aether_rules::get_rule_for_execution(&self.pool, id)
            .await
            .map_err(AutomationError::from)?;
        let mut variables = Vec::new();
        for node in rule.flow.nodes.values() {
            match node {
                RuleNode::Switch {
                    variables: values, ..
                }
                | RuleNode::ChangeValue {
                    variables: values, ..
                } => variables.extend(values.iter().cloned()),
                _ => {},
            }
        }
        variables.sort_by(|left, right| left.name.cmp(&right.name));
        variables.dedup_by(|left, right| left.name == right.name);
        Ok(variables)
    }
}
