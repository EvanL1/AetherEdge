//! SQLite adapter for the governed alarm-rule mutation port.

use aether_domain::{AlarmRuleDefinition, AlarmRuleId, AlarmRuleTarget, AlertId, TimestampMs};
use aether_ports::{
    AlarmRuleMutation, AlarmRuleMutationKind, AlarmRuleMutationReceipt, AlarmRuleMutator,
    AlertResolutionReceipt, AlertResolver, PortError, PortErrorKind, PortResult,
};
use async_trait::async_trait;
use chrono::Utc;
use sqlx::{Sqlite, SqlitePool, Transaction};

use crate::broadcast::Broadcaster;
use crate::db;
use crate::models::{Alert, AlertRule};

/// SQLite persistence adapter whose successful receipt also guarantees that
/// disable/delete alert reconciliation committed atomically with the rule.
pub struct SqliteAlarmRuleMutator {
    pool: SqlitePool,
    broadcaster: Broadcaster,
}

impl SqliteAlarmRuleMutator {
    /// Creates an adapter over the alarm service's local database.
    #[must_use]
    pub fn new(pool: SqlitePool, broadcaster: Broadcaster) -> Self {
        Self { pool, broadcaster }
    }

    async fn create(
        &self,
        definition: AlarmRuleDefinition,
    ) -> PortResult<AlarmRuleMutationReceipt> {
        let target = StoredTarget::from_domain(definition.target());
        let mut transaction = self.pool.begin().await.map_err(storage_error)?;
        reject_duplicate_name(&mut transaction, definition.name(), None).await?;
        reject_duplicate_target(&mut transaction, &target, None).await?;

        let now = Utc::now().timestamp();
        let id = sqlx::query_scalar::<_, i64>(
            "INSERT INTO alert_rule
             (service_type, channel_id, data_type, point_id, rule_name, warning_level,
              operator, value, enabled, description, created_at, updated_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
             RETURNING id",
        )
        .bind(&target.service_type)
        .bind(target.channel_id)
        .bind(&target.data_type)
        .bind(target.point_id)
        .bind(definition.name())
        .bind(definition.severity().get())
        .bind(definition.comparator().as_str())
        .bind(definition.threshold())
        .bind(definition.enabled())
        .bind(definition.description())
        .bind(now)
        .bind(now)
        .fetch_one(&mut *transaction)
        .await
        .map_err(storage_error)?;
        transaction.commit().await.map_err(storage_error)?;

        Ok(AlarmRuleMutationReceipt::new(
            alarm_rule_id(id)?,
            AlarmRuleMutationKind::Create,
        ))
    }

    async fn update(
        &self,
        rule_id: AlarmRuleId,
        patch: aether_ports::AlarmRulePatch,
    ) -> PortResult<AlarmRuleMutationReceipt> {
        let id = sqlite_rule_id(rule_id)?;
        let mut transaction = self.pool.begin().await.map_err(storage_error)?;
        let existing = find_rule(&mut transaction, id).await?;
        let target = patch.target().map_or_else(
            || StoredTarget::from_stored(&existing),
            StoredTarget::from_domain,
        );
        let name = patch.name().unwrap_or(&existing.rule_name).to_string();
        let warning_level = patch
            .severity()
            .map_or(existing.warning_level, aether_domain::AlarmSeverity::get);
        let operator = patch.comparator().map_or_else(
            || existing.operator.clone(),
            |value| value.as_str().to_string(),
        );
        let threshold = patch.threshold().unwrap_or(existing.value);
        let enabled = patch.enabled().unwrap_or(existing.enabled);
        let description = patch.description().map_or_else(
            || existing.description.clone(),
            |replacement| replacement.map(str::to_string),
        );

        reject_duplicate_name(&mut transaction, &name, Some(id)).await?;
        reject_duplicate_target(&mut transaction, &target, Some(id)).await?;

        sqlx::query(
            "UPDATE alert_rule SET
                service_type = ?, channel_id = ?, data_type = ?, point_id = ?,
                rule_name = ?, warning_level = ?, operator = ?, value = ?,
                enabled = ?, description = ?, updated_at = ?
             WHERE id = ?",
        )
        .bind(&target.service_type)
        .bind(target.channel_id)
        .bind(&target.data_type)
        .bind(target.point_id)
        .bind(&name)
        .bind(warning_level)
        .bind(&operator)
        .bind(threshold)
        .bind(enabled)
        .bind(description.as_deref())
        .bind(Utc::now().timestamp())
        .bind(id)
        .execute(&mut *transaction)
        .await
        .map_err(storage_error)?;
        let resolved = if enabled {
            Vec::new()
        } else {
            resolve_alerts(&mut transaction, id).await?
        };
        transaction.commit().await.map_err(storage_error)?;

        if !resolved.is_empty() {
            let updated = AlertRule {
                id,
                service_type: target.service_type,
                channel_id: target.channel_id,
                data_type: target.data_type,
                point_id: target.point_id,
                rule_name: name,
                warning_level,
                operator,
                value: threshold,
                enabled,
                description,
                created_at: existing.created_at,
                updated_at: Utc::now().timestamp(),
            };
            self.broadcast_resolved(&updated, &resolved, "规则被禁用")
                .await;
        }

        Ok(AlarmRuleMutationReceipt::new(
            rule_id,
            AlarmRuleMutationKind::Update,
        ))
    }

    async fn set_enabled(
        &self,
        rule_id: AlarmRuleId,
        enabled: bool,
    ) -> PortResult<AlarmRuleMutationReceipt> {
        let id = sqlite_rule_id(rule_id)?;
        let mut transaction = self.pool.begin().await.map_err(storage_error)?;
        let mut rule = find_rule(&mut transaction, id).await?;
        sqlx::query("UPDATE alert_rule SET enabled = ?, updated_at = ? WHERE id = ?")
            .bind(enabled)
            .bind(Utc::now().timestamp())
            .bind(id)
            .execute(&mut *transaction)
            .await
            .map_err(storage_error)?;
        let resolved = if enabled {
            Vec::new()
        } else {
            resolve_alerts(&mut transaction, id).await?
        };
        transaction.commit().await.map_err(storage_error)?;

        rule.enabled = enabled;
        if !resolved.is_empty() {
            self.broadcast_resolved(&rule, &resolved, "规则被禁用")
                .await;
        }
        let kind = if enabled {
            AlarmRuleMutationKind::Enable
        } else {
            AlarmRuleMutationKind::Disable
        };
        Ok(AlarmRuleMutationReceipt::new(rule_id, kind))
    }

    async fn delete(&self, rule_id: AlarmRuleId) -> PortResult<AlarmRuleMutationReceipt> {
        let id = sqlite_rule_id(rule_id)?;
        let mut transaction = self.pool.begin().await.map_err(storage_error)?;
        let rule = find_rule(&mut transaction, id).await?;
        let resolved = resolve_alerts(&mut transaction, id).await?;
        sqlx::query("DELETE FROM alert_rule WHERE id = ?")
            .bind(id)
            .execute(&mut *transaction)
            .await
            .map_err(storage_error)?;
        transaction.commit().await.map_err(storage_error)?;

        if !resolved.is_empty() {
            self.broadcast_resolved(&rule, &resolved, "规则被删除")
                .await;
        }
        Ok(AlarmRuleMutationReceipt::new(
            rule_id,
            AlarmRuleMutationKind::Delete,
        ))
    }

    async fn broadcast_resolved(&self, rule: &AlertRule, alerts: &[Alert], reason: &str) {
        for alert in alerts {
            self.broadcaster
                .send_alarm_recovery(alert.id, rule, None, reason)
                .await;
        }
        if let Ok(counts) = db::get_active_alarm_counts(&self.pool).await {
            self.broadcaster.send_alarm_count(&counts).await;
        }
    }
}

#[async_trait]
impl AlarmRuleMutator for SqliteAlarmRuleMutator {
    async fn mutate(&self, mutation: AlarmRuleMutation) -> PortResult<AlarmRuleMutationReceipt> {
        match mutation {
            AlarmRuleMutation::Create { definition } => self.create(definition).await,
            AlarmRuleMutation::Update { rule_id, patch } => self.update(rule_id, patch).await,
            AlarmRuleMutation::SetEnabled { rule_id, enabled } => {
                self.set_enabled(rule_id, enabled).await
            },
            AlarmRuleMutation::Delete { rule_id } => self.delete(rule_id).await,
        }
    }
}

#[async_trait]
impl AlertResolver for SqliteAlarmRuleMutator {
    async fn resolve(&self, alert_id: AlertId) -> PortResult<AlertResolutionReceipt> {
        let id = sqlite_alert_id(alert_id)?;
        let mut transaction = self.pool.begin().await.map_err(storage_error)?;
        let alert = sqlx::query_as::<_, Alert>("DELETE FROM alert WHERE id = ? RETURNING *")
            .bind(id)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(storage_error)?
            .ok_or_else(|| PortError::new(PortErrorKind::NotFound, "active alert not found"))?;
        let resolved_at_seconds = Utc::now().timestamp();
        sqlx::query(
            "INSERT INTO alert_event
                (rule_id, rule_snapshot, service_type, channel_id, data_type, point_id,
                 rule_name, warning_level, operator, threshold_value,
                 trigger_value, recovery_value, event_type,
                 triggered_at, recovered_at, duration)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 'recovery', ?, ?, ?)",
        )
        .bind(alert.rule_id)
        .bind(&alert.rule_snapshot)
        .bind(&alert.service_type)
        .bind(alert.channel_id)
        .bind(&alert.data_type)
        .bind(alert.point_id)
        .bind(&alert.rule_name)
        .bind(alert.warning_level)
        .bind(&alert.operator)
        .bind(alert.threshold_value)
        .bind(alert.current_value)
        .bind(alert.current_value)
        .bind(alert.triggered_at)
        .bind(resolved_at_seconds)
        .bind(resolved_at_seconds - alert.triggered_at)
        .execute(&mut *transaction)
        .await
        .map_err(storage_error)?;
        transaction.commit().await.map_err(storage_error)?;

        let rule = AlertRule {
            id: alert.rule_id,
            service_type: alert.service_type.clone(),
            channel_id: alert.channel_id,
            data_type: alert.data_type.clone(),
            point_id: alert.point_id,
            rule_name: alert.rule_name.clone(),
            warning_level: alert.warning_level,
            operator: alert.operator.clone(),
            value: alert.threshold_value,
            enabled: true,
            description: None,
            created_at: alert.triggered_at,
            updated_at: resolved_at_seconds,
        };
        self.broadcaster
            .send_alarm_recovery(
                alert.id,
                &rule,
                Some(alert.current_value),
                "manually resolved",
            )
            .await;
        if let Ok(counts) = db::get_active_alarm_counts(&self.pool).await {
            self.broadcaster.send_alarm_count(&counts).await;
        }

        let rule_id = u64::try_from(alert.rule_id)
            .map(AlarmRuleId::new)
            .map_err(|_| {
                PortError::new(
                    PortErrorKind::InvalidData,
                    "active alert contains a negative alarm rule id",
                )
            })?;
        let resolved_at_ms = u64::try_from(resolved_at_seconds)
            .ok()
            .and_then(|seconds| seconds.checked_mul(1_000))
            .map(TimestampMs::new)
            .ok_or_else(|| {
                PortError::new(
                    PortErrorKind::InvalidData,
                    "alert resolution timestamp is outside the supported range",
                )
            })?;
        Ok(AlertResolutionReceipt::new(
            alert_id,
            rule_id,
            resolved_at_ms,
        ))
    }
}

struct StoredTarget {
    service_type: String,
    channel_id: i64,
    data_type: String,
    point_id: i64,
}

impl StoredTarget {
    fn from_domain(target: &AlarmRuleTarget) -> Self {
        match target {
            AlarmRuleTarget::Point {
                service_type,
                channel_id,
                data_type,
                point_id,
            } => Self {
                service_type: service_type.clone(),
                channel_id: i64::from(channel_id.get()),
                data_type: data_type.clone(),
                point_id: i64::from(point_id.get()),
            },
            AlarmRuleTarget::ChannelOnline { channel_id } => Self {
                service_type: "io".to_string(),
                channel_id: i64::from(channel_id.get()),
                data_type: AlertRule::CHANNEL_ONLINE_DATA_TYPE.to_string(),
                point_id: 0,
            },
        }
    }

    fn from_stored(rule: &AlertRule) -> Self {
        Self {
            service_type: rule.service_type.clone(),
            channel_id: rule.channel_id,
            data_type: rule.data_type.clone(),
            point_id: rule.point_id,
        }
    }
}

async fn find_rule(transaction: &mut Transaction<'_, Sqlite>, id: i64) -> PortResult<AlertRule> {
    sqlx::query_as::<_, AlertRule>("SELECT * FROM alert_rule WHERE id = ?")
        .bind(id)
        .fetch_optional(&mut **transaction)
        .await
        .map_err(storage_error)?
        .ok_or_else(|| PortError::new(PortErrorKind::NotFound, "alarm rule not found"))
}

async fn reject_duplicate_name(
    transaction: &mut Transaction<'_, Sqlite>,
    name: &str,
    excluding_id: Option<i64>,
) -> PortResult<()> {
    let duplicate = sqlx::query_scalar::<_, i64>(
        "SELECT id FROM alert_rule
         WHERE LOWER(rule_name) = LOWER(?) AND (? IS NULL OR id != ?) LIMIT 1",
    )
    .bind(name)
    .bind(excluding_id)
    .bind(excluding_id)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(storage_error)?;
    if duplicate.is_some() {
        return Err(PortError::new(
            PortErrorKind::Conflict,
            "an alarm rule with this name already exists",
        ));
    }
    Ok(())
}

async fn reject_duplicate_target(
    transaction: &mut Transaction<'_, Sqlite>,
    target: &StoredTarget,
    excluding_id: Option<i64>,
) -> PortResult<()> {
    let duplicate = sqlx::query_scalar::<_, i64>(
        "SELECT id FROM alert_rule
         WHERE service_type = ? AND channel_id = ? AND data_type = ? AND point_id = ?
           AND (? IS NULL OR id != ?) LIMIT 1",
    )
    .bind(&target.service_type)
    .bind(target.channel_id)
    .bind(&target.data_type)
    .bind(target.point_id)
    .bind(excluding_id)
    .bind(excluding_id)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(storage_error)?;
    if duplicate.is_some() {
        return Err(PortError::new(
            PortErrorKind::Conflict,
            "an alarm rule already monitors this target",
        ));
    }
    Ok(())
}

async fn resolve_alerts(
    transaction: &mut Transaction<'_, Sqlite>,
    rule_id: i64,
) -> PortResult<Vec<Alert>> {
    let alerts = sqlx::query_as::<_, Alert>("SELECT * FROM alert WHERE rule_id = ?")
        .bind(rule_id)
        .fetch_all(&mut **transaction)
        .await
        .map_err(storage_error)?;
    let now = Utc::now().timestamp();
    for alert in &alerts {
        sqlx::query(
            "INSERT INTO alert_event
                (rule_id, rule_snapshot, service_type, channel_id, data_type, point_id,
                 rule_name, warning_level, operator, threshold_value,
                 trigger_value, recovery_value, event_type,
                 triggered_at, recovered_at, duration)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, NULL, 'recovery', ?, ?, ?)",
        )
        .bind(alert.rule_id)
        .bind(&alert.rule_snapshot)
        .bind(&alert.service_type)
        .bind(alert.channel_id)
        .bind(&alert.data_type)
        .bind(alert.point_id)
        .bind(&alert.rule_name)
        .bind(alert.warning_level)
        .bind(&alert.operator)
        .bind(alert.threshold_value)
        .bind(alert.current_value)
        .bind(alert.triggered_at)
        .bind(now)
        .bind(now - alert.triggered_at)
        .execute(&mut **transaction)
        .await
        .map_err(storage_error)?;
        sqlx::query("DELETE FROM alert WHERE id = ?")
            .bind(alert.id)
            .execute(&mut **transaction)
            .await
            .map_err(storage_error)?;
    }
    Ok(alerts)
}

fn sqlite_rule_id(rule_id: AlarmRuleId) -> PortResult<i64> {
    i64::try_from(rule_id.get()).map_err(|_| {
        PortError::new(
            PortErrorKind::InvalidData,
            "alarm rule id exceeds SQLite INTEGER range",
        )
    })
}

fn sqlite_alert_id(alert_id: AlertId) -> PortResult<i64> {
    i64::try_from(alert_id.get()).map_err(|_| {
        PortError::new(
            PortErrorKind::InvalidData,
            "alert id exceeds SQLite INTEGER range",
        )
    })
}

fn alarm_rule_id(id: i64) -> PortResult<AlarmRuleId> {
    u64::try_from(id).map(AlarmRuleId::new).map_err(|_| {
        PortError::new(
            PortErrorKind::InvalidData,
            "SQLite returned a negative alarm rule id",
        )
    })
}

fn storage_error(error: sqlx::Error) -> PortError {
    PortError::new(
        PortErrorKind::Unavailable,
        format!("local alarm storage unavailable: {error}"),
    )
}

#[cfg(test)]
mod tests {
    use aether_domain::{
        AlarmComparator, AlarmRuleDefinition, AlarmRuleId, AlarmRuleTarget, AlarmSeverity,
        ChannelId, PointId,
    };
    use aether_ports::{
        AlarmRuleMutation, AlarmRuleMutationKind, AlarmRuleMutator, AlarmRulePatch, PortErrorKind,
    };

    use super::SqliteAlarmRuleMutator;
    use crate::{broadcast::Broadcaster, db};

    async fn adapter() -> (SqliteAlarmRuleMutator, sqlx::SqlitePool) {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("in-memory alarm database");
        sqlx::query("PRAGMA foreign_keys = ON")
            .execute(&pool)
            .await
            .expect("enable production foreign-key behavior");
        db::create_tables(&pool).await.expect("alarm schema");
        let broadcaster = Broadcaster::new(
            reqwest::Client::new(),
            "http://127.0.0.1:9".to_string(),
            "http://127.0.0.1:9".to_string(),
        );
        (SqliteAlarmRuleMutator::new(pool.clone(), broadcaster), pool)
    }

    fn definition(name: &str, point_id: u32) -> AlarmRuleDefinition {
        AlarmRuleDefinition::new(
            AlarmRuleTarget::point("io", ChannelId::new(7), "T", PointId::new(point_id))
                .expect("target"),
            name,
            AlarmSeverity::new(2).expect("severity"),
            AlarmComparator::GreaterThan,
            80.0,
            true,
            None,
        )
        .expect("definition")
    }

    #[tokio::test]
    async fn adapter_conforms_for_create_update_disable_and_delete() {
        let (adapter, pool) = adapter().await;
        let created = adapter
            .mutate(AlarmRuleMutation::create(definition("temperature", 3)))
            .await
            .expect("create rule");
        assert_eq!(created.kind(), AlarmRuleMutationKind::Create);

        let id = i64::try_from(created.rule_id().get()).expect("SQLite id");
        let stored = db::get_rule_by_id(&pool, id)
            .await
            .expect("query rule")
            .expect("stored rule");
        assert_eq!(stored.rule_name, "temperature");

        let patch = AlarmRulePatch::new(
            None,
            Some("high temperature".to_string()),
            None,
            None,
            Some(90.0),
            Some(false),
            None,
        )
        .expect("patch");
        adapter
            .mutate(AlarmRuleMutation::update(created.rule_id(), patch))
            .await
            .expect("update rule");
        let updated = db::get_rule_by_id(&pool, id)
            .await
            .expect("query updated rule")
            .expect("updated rule");
        assert_eq!(updated.rule_name, "high temperature");
        assert_eq!(updated.value, 90.0);
        assert!(!updated.enabled);

        adapter
            .mutate(AlarmRuleMutation::delete(created.rule_id()))
            .await
            .expect("delete rule");
        assert!(
            db::get_rule_by_id(&pool, id)
                .await
                .expect("query deletion")
                .is_none()
        );
    }

    #[tokio::test]
    async fn adapter_reports_conflict_and_not_found_with_stable_port_kinds() {
        let (adapter, _pool) = adapter().await;
        adapter
            .mutate(AlarmRuleMutation::create(definition("first", 3)))
            .await
            .expect("first rule");

        let duplicate = adapter
            .mutate(AlarmRuleMutation::create(definition("second", 3)))
            .await
            .expect_err("duplicate target must fail");
        assert_eq!(duplicate.kind(), PortErrorKind::Conflict);

        let missing = adapter
            .mutate(AlarmRuleMutation::set_enabled(AlarmRuleId::new(404), false))
            .await
            .expect_err("missing rule must fail");
        assert_eq!(missing.kind(), PortErrorKind::NotFound);
    }

    #[tokio::test]
    async fn disabling_a_rule_reconciles_its_active_alert_in_the_same_acceptance() {
        let (adapter, pool) = adapter().await;
        let receipt = adapter
            .mutate(AlarmRuleMutation::create(definition("temperature", 3)))
            .await
            .expect("create rule");
        let id = i64::try_from(receipt.rule_id().get()).expect("SQLite id");
        sqlx::query(
            "INSERT INTO alert
             (rule_id, rule_snapshot, service_type, channel_id, data_type, point_id,
              rule_name, warning_level, operator, threshold_value, current_value,
              status, triggered_at)
             VALUES (?, '{}', 'io', 7, 'T', 3, 'temperature', 2, '>', 80, 95, 'active', 1)",
        )
        .bind(id)
        .execute(&pool)
        .await
        .expect("active alert");

        adapter
            .mutate(AlarmRuleMutation::set_enabled(receipt.rule_id(), false))
            .await
            .expect("disable rule");

        let active: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM alert WHERE rule_id = ?")
            .bind(id)
            .fetch_one(&pool)
            .await
            .expect("active count");
        let recoveries: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM alert_event WHERE rule_id = ? AND event_type = 'recovery'",
        )
        .bind(id)
        .fetch_one(&pool)
        .await
        .expect("recovery count");
        assert_eq!((active, recoveries), (0, 1));

        adapter
            .mutate(AlarmRuleMutation::delete(receipt.rule_id()))
            .await
            .expect("delete rule while retaining history");
        let retained: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM alert_event WHERE rule_id = ? AND event_type = 'recovery'",
        )
        .bind(id)
        .fetch_one(&pool)
        .await
        .expect("retained recovery count");
        let foreign_keys: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM pragma_foreign_key_list('alert_event')")
                .fetch_one(&pool)
                .await
                .expect("historical foreign keys");
        assert_eq!((retained, foreign_keys), (1, 0));
    }
}
