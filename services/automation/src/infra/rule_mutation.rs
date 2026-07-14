//! SQLite-backed automation-rule mutation adapter.

use std::sync::Arc;

use aether_calc::StateStore;
use aether_domain::RuleId;
use aether_ports::{
    AutomationRuleMutator, AutomationRulesRevision, PortError, PortErrorKind, PortResult,
    RevisionedRuleMutation, RuleMutation, RuleMutationReceipt,
};
use aether_rules::{RuleScheduler, TriggerConfig};
use async_trait::async_trait;
use serde_json::Value;
use sqlx::SqlitePool;

use crate::infra::runtime_topology::{AutomationTopologyHandle, PointWatchReadiness};

enum RuleRuntimeRefresh {
    Refreshed,
    PointWatchGated(PortError),
}

/// Owns durable rule mutation and the corresponding scheduler refresh.
pub struct SqliteRuleMutator<S: StateStore> {
    pool: SqlitePool,
    scheduler: Arc<RuleScheduler<S>>,
    topology: Option<Arc<AutomationTopologyHandle>>,
    point_watch_readiness: Option<Arc<PointWatchReadiness>>,
    manifest_source: Option<aether_shm_bridge::ChannelPointManifestSource>,
}

impl<S: StateStore + 'static> SqliteRuleMutator<S> {
    /// Creates the adapter over the rule database and active scheduler.
    #[must_use]
    pub fn new(pool: SqlitePool, scheduler: Arc<RuleScheduler<S>>) -> Self {
        Self {
            pool,
            scheduler,
            topology: None,
            point_watch_readiness: None,
            manifest_source: None,
        }
    }

    /// Serializes production subscription reloads with topology publication.
    #[must_use]
    pub fn with_topology_guard(
        mut self,
        topology: Arc<AutomationTopologyHandle>,
        point_watch_readiness: Arc<PointWatchReadiness>,
        manifest_source: aether_shm_bridge::ChannelPointManifestSource,
    ) -> Self {
        self.topology = Some(topology);
        self.point_watch_readiness = Some(point_watch_readiness);
        self.manifest_source = Some(manifest_source);
        self
    }

    async fn reload_scheduler(&self) -> PortResult<RuleRuntimeRefresh> {
        let (Some(topology), Some(readiness), Some(manifest_source)) = (
            &self.topology,
            &self.point_watch_readiness,
            &self.manifest_source,
        ) else {
            return self
                .scheduler
                .reload_rules()
                .await
                .map(|_| RuleRuntimeRefresh::Refreshed)
                .map_err(scheduler_reload_error);
        };

        let _rebuild = readiness.lock_rebuild().await;
        let view = Arc::clone(topology).pin_command().await;
        readiness.mark_unready();
        self.scheduler
            .reload_rules()
            .await
            .map_err(scheduler_reload_error)?;
        let generation = view.generation();
        generation.rebuild_point_watch(&self.scheduler).await;
        let manifest_matches = manifest_source.load().is_some_and(|manifest| {
            manifest.layout_hash() == generation.point_manifest().layout_hash()
                && manifest.slot_count() == generation.point_manifest().slot_count()
        });
        if manifest_matches {
            readiness.mark_ready(generation.sequence());
            Ok(RuleRuntimeRefresh::Refreshed)
        } else {
            Ok(RuleRuntimeRefresh::PointWatchGated(PortError::new(
                PortErrorKind::Unavailable,
                "PointWatch subscription publication does not match the active topology generation",
            )))
        }
    }
}

#[async_trait]
impl<S: StateStore + 'static> AutomationRuleMutator for SqliteRuleMutator<S> {
    async fn mutate(&self, mutation: RuleMutation) -> PortResult<RuleMutationReceipt> {
        let revision = sqlx::query_scalar::<_, i64>(
            "SELECT revision FROM configuration_revisions WHERE scope = 'automation_rules'",
        )
        .fetch_one(&self.pool)
        .await
        .map_err(database_error)?;
        let revision = AutomationRulesRevision::new(u64::try_from(revision).map_err(|_| {
            PortError::new(
                PortErrorKind::Permanent,
                "automation-rules revision became negative",
            )
        })?);
        self.mutate_revisioned(RevisionedRuleMutation::new(mutation, revision))
            .await
    }

    async fn mutate_revisioned(
        &self,
        mutation: RevisionedRuleMutation,
    ) -> PortResult<RuleMutationReceipt> {
        let kind = mutation.kind();
        let expected = mutation.expected_revision();
        let mutation = mutation.into_mutation();
        if expected.get() == 0 || expected.get() >= i64::MAX as u64 {
            return Err(PortError::new(
                PortErrorKind::InvalidData,
                "expected automation-rules revision must be in 1..i64::MAX",
            ));
        }

        let mut transaction = self.pool.begin().await.map_err(database_error)?;
        let resulting_revision = sqlx::query_scalar::<_, i64>(
            "UPDATE configuration_revisions \
             SET revision = revision + 1, updated_at = CURRENT_TIMESTAMP \
             WHERE scope = 'automation_rules' AND revision = ? \
             RETURNING revision",
        )
        .bind(expected.get() as i64)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(database_error)?
        .ok_or_else(|| {
            PortError::new(
                PortErrorKind::Conflict,
                format!(
                    "automation rules changed concurrently; expected revision {}",
                    expected.get()
                ),
            )
        })?;

        let rule_id = match mutation {
            RuleMutation::Create { name, description } => {
                let result = sqlx::query(
                    "INSERT INTO rules \
                     (name, description, nodes_json, flow_json, format, enabled, priority, cooldown_ms) \
                     VALUES (?, ?, '{}', NULL, 'vue-flow', FALSE, 0, 0)",
                )
                .bind(name)
                .bind(description)
                .execute(&mut *transaction)
                .await
                .map_err(database_error)?;
                let id = u64::try_from(result.last_insert_rowid()).map_err(|_| {
                    PortError::new(
                        PortErrorKind::Permanent,
                        "SQLite returned a negative rule identifier",
                    )
                })?;
                Some(RuleId::new(id))
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
                    .execute(&mut *transaction)
                    .await
                    .map_err(database_error)?;
                require_affected_rule(result.rows_affected(), rule_id)?;
                Some(rule_id)
            },
            RuleMutation::SetEnabled { rule_id, enabled } => {
                let result = sqlx::query(
                    "UPDATE rules SET enabled = ?, updated_at = CURRENT_TIMESTAMP WHERE id = ?",
                )
                .bind(enabled)
                .bind(database_rule_id(rule_id)?)
                .execute(&mut *transaction)
                .await
                .map_err(database_error)?;
                require_affected_rule(result.rows_affected(), rule_id)?;
                Some(rule_id)
            },
            RuleMutation::Delete { rule_id } => {
                let result = sqlx::query("DELETE FROM rules WHERE id = ?")
                    .bind(database_rule_id(rule_id)?)
                    .execute(&mut *transaction)
                    .await
                    .map_err(database_error)?;
                require_affected_rule(result.rows_affected(), rule_id)?;
                Some(rule_id)
            },
            RuleMutation::Reload => None,
        };

        transaction.commit().await.map_err(database_error)?;
        let resulting_revision = aether_ports::AutomationRulesRevision::new(
            u64::try_from(resulting_revision).map_err(|_| {
                PortError::new(
                    PortErrorKind::Permanent,
                    "automation-rules revision became negative",
                )
            })?,
        );

        match self.reload_scheduler().await {
            Ok(RuleRuntimeRefresh::Refreshed) => match rule_id {
                Some(rule_id) => Ok(RuleMutationReceipt::new_at_revision(
                    rule_id,
                    kind,
                    resulting_revision,
                )),
                None => Ok(RuleMutationReceipt::reload_at_revision(resulting_revision)),
            },
            Ok(RuleRuntimeRefresh::PointWatchGated(failure)) => Ok(
                RuleMutationReceipt::point_watch_gated(rule_id, kind, resulting_revision, failure),
            ),
            Err(failure) => {
                self.scheduler.stop();
                Ok(RuleMutationReceipt::scheduler_stopped_at_revision(
                    rule_id,
                    kind,
                    resulting_revision,
                    failure,
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

fn scheduler_reload_error(error: aether_rules::RuleError) -> PortError {
    PortError::new(
        PortErrorKind::Unavailable,
        format!("rule scheduler reload failed: {error}"),
    )
}
