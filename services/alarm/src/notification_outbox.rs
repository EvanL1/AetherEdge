//! Durable delivery of committed alarm state transitions.
//!
//! Each transition is enqueued in the same SQLite transaction that changes
//! alarm state. API and uplink destinations have independent rows so one
//! successful destination is never replayed merely because the other failed.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use chrono::Utc;
use sqlx::{FromRow, Sqlite, SqlitePool, Transaction};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use crate::notification::{AlarmNotification, AlarmNotificationDestination, AlarmNotifier};

const DISPATCH_BATCH_SIZE: i64 = 32;
const MAX_LAST_ERROR_CHARS: usize = 1_024;

#[derive(Debug, Clone, Copy)]
pub struct NotificationDispatcherConfig {
    pub poll_interval: Duration,
    pub retry_initial: Duration,
    pub retry_max: Duration,
    pub shutdown_drain: Duration,
}

impl NotificationDispatcherConfig {
    #[must_use]
    pub fn from_alarm_config(config: &crate::config::AlarmConfig) -> Self {
        Self {
            poll_interval: Duration::from_millis(
                config.notification_outbox_poll_interval_ms.max(10),
            ),
            retry_initial: Duration::from_millis(config.notification_retry_initial_ms.max(1)),
            retry_max: Duration::from_millis(
                config
                    .notification_retry_max_ms
                    .max(config.notification_retry_initial_ms.max(1)),
            ),
            shutdown_drain: Duration::from_millis(config.notification_shutdown_drain_ms.max(1)),
        }
    }
}

#[derive(Debug, Clone, Copy, serde::Serialize)]
pub struct NotificationOutboxStats {
    pub pending: i64,
    /// Rows that have failed at least one delivery attempt and remain pending.
    pub failed: i64,
    /// Unix epoch milliseconds for the oldest still-pending destination row.
    pub oldest_pending_at: Option<i64>,
}

#[derive(Debug, FromRow)]
struct OutboxRecord {
    id: i64,
    event_id: String,
    destination: String,
    payload: String,
    attempt_count: i64,
}

pub async fn create_table(pool: &SqlitePool) -> Result<()> {
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS alarm_notification_outbox (
            id              INTEGER PRIMARY KEY AUTOINCREMENT,
            event_id        TEXT    NOT NULL,
            destination     TEXT    NOT NULL CHECK (destination IN ('api', 'uplink')),
            payload         TEXT    NOT NULL,
            created_at      INTEGER NOT NULL,
            attempt_count   INTEGER NOT NULL DEFAULT 0,
            next_attempt_at INTEGER NOT NULL DEFAULT 0,
            last_error      TEXT,
            UNIQUE (event_id, destination)
        )
        "#,
    )
    .execute(pool)
    .await
    .context("create alarm notification outbox")?;
    sqlx::query(
        "CREATE INDEX IF NOT EXISTS idx_alarm_notification_outbox_due
         ON alarm_notification_outbox (next_attempt_at, id)",
    )
    .execute(pool)
    .await
    .context("create alarm notification outbox due index")?;
    Ok(())
}

/// Adds both destination deliveries to an already-open alarm-state transaction.
pub async fn enqueue_alarm(
    transaction: &mut Transaction<'_, Sqlite>,
    notification: &AlarmNotification,
) -> Result<()> {
    let payload = serde_json::to_string(notification).context("serialize alarm notification")?;
    let created_at = Utc::now().timestamp_millis();
    for destination in AlarmNotificationDestination::ALL {
        sqlx::query(
            "INSERT INTO alarm_notification_outbox
                (event_id, destination, payload, created_at, next_attempt_at)
             VALUES (?, ?, ?, ?, ?)
             ON CONFLICT(event_id, destination) DO NOTHING",
        )
        .bind(&notification.event_id)
        .bind(destination.as_str())
        .bind(&payload)
        .bind(created_at)
        .bind(created_at)
        .execute(&mut **transaction)
        .await
        .context("enqueue alarm notification")?;
    }
    Ok(())
}

pub async fn stats(pool: &SqlitePool) -> Result<NotificationOutboxStats> {
    let row = sqlx::query_as::<_, (i64, i64, Option<i64>)>(
        "SELECT COUNT(*),
                COALESCE(SUM(CASE WHEN attempt_count > 0 THEN 1 ELSE 0 END), 0),
                MIN(created_at)
         FROM alarm_notification_outbox",
    )
    .fetch_one(pool)
    .await
    .context("read alarm notification outbox statistics")?;
    Ok(NotificationOutboxStats {
        pending: row.0,
        failed: row.1,
        oldest_pending_at: row.2,
    })
}

pub async fn run_notification_dispatcher(
    pool: SqlitePool,
    notifier: Arc<dyn AlarmNotifier>,
    config: NotificationDispatcherConfig,
    shutdown: CancellationToken,
) {
    match stats(&pool).await {
        Ok(snapshot) => info!(
            pending = snapshot.pending,
            failed = snapshot.failed,
            "Alarm notification outbox dispatcher started"
        ),
        Err(error) => warn!("Cannot read alarm notification outbox at startup: {error}"),
    }

    loop {
        let dispatch = dispatch_due_batch(&pool, notifier.as_ref(), config);
        let examined = tokio::select! {
            _ = shutdown.cancelled() => break,
            result = dispatch => match result {
                Ok(examined) => examined,
                Err(error) => {
                    warn!("Alarm notification outbox dispatch failed: {error}");
                    0
                },
            },
        };

        if examined >= DISPATCH_BATCH_SIZE {
            continue;
        }
        tokio::select! {
            _ = shutdown.cancelled() => break,
            _ = tokio::time::sleep(config.poll_interval) => {},
        }
    }

    let drain = async {
        loop {
            match dispatch_due_batch(&pool, notifier.as_ref(), config).await {
                Ok(0) => break,
                Ok(_) => {},
                Err(error) => {
                    warn!("Alarm notification shutdown drain stopped after storage error: {error}");
                    break;
                },
            }
        }
    };
    if tokio::time::timeout(config.shutdown_drain, drain)
        .await
        .is_err()
    {
        warn!(
            drain_timeout_ms = config.shutdown_drain.as_millis(),
            "Alarm notification shutdown drain reached its deadline; pending rows remain durable"
        );
    }
    info!("Alarm notification outbox dispatcher stopped");
}

async fn dispatch_due_batch(
    pool: &SqlitePool,
    notifier: &dyn AlarmNotifier,
    config: NotificationDispatcherConfig,
) -> Result<i64> {
    let now = Utc::now().timestamp_millis();
    let records = sqlx::query_as::<_, OutboxRecord>(
        "SELECT id, event_id, destination, payload, attempt_count
         FROM alarm_notification_outbox
         WHERE next_attempt_at <= ?
         ORDER BY next_attempt_at ASC, id ASC
         LIMIT ?",
    )
    .bind(now)
    .bind(DISPATCH_BATCH_SIZE)
    .fetch_all(pool)
    .await
    .context("load due alarm notifications")?;
    let examined = i64::try_from(records.len()).unwrap_or(DISPATCH_BATCH_SIZE);

    for record in records {
        let delivery = parse_record(&record).and_then(|(destination, notification)| {
            if notification.event_id != record.event_id {
                anyhow::bail!(
                    "outbox event id mismatch: row={} payload={}",
                    record.event_id,
                    notification.event_id
                );
            }
            Ok((destination, notification))
        });
        let result = match delivery {
            Ok((destination, notification)) => {
                notifier.deliver_alarm(destination, &notification).await
            },
            Err(error) => Err(error),
        };

        match result {
            Ok(()) => {
                sqlx::query("DELETE FROM alarm_notification_outbox WHERE id = ?")
                    .bind(record.id)
                    .execute(pool)
                    .await
                    .context("acknowledge delivered alarm notification")?;
                debug!(
                    event_id = record.event_id,
                    destination = record.destination,
                    "Alarm notification outbox row acknowledged"
                );
            },
            Err(error) => {
                let attempts = record.attempt_count.saturating_add(1);
                let delay = retry_delay(config, attempts);
                let next_attempt_at = Utc::now()
                    .timestamp_millis()
                    .saturating_add(i64::try_from(delay.as_millis()).unwrap_or(i64::MAX));
                let last_error = error
                    .to_string()
                    .chars()
                    .take(MAX_LAST_ERROR_CHARS)
                    .collect::<String>();
                sqlx::query(
                    "UPDATE alarm_notification_outbox
                     SET attempt_count = ?, next_attempt_at = ?, last_error = ?
                     WHERE id = ?",
                )
                .bind(attempts)
                .bind(next_attempt_at)
                .bind(&last_error)
                .bind(record.id)
                .execute(pool)
                .await
                .context("record failed alarm notification delivery")?;
                warn!(
                    event_id = record.event_id,
                    destination = record.destination,
                    attempt = attempts,
                    retry_in_ms = delay.as_millis(),
                    error = last_error,
                    "Alarm notification delivery will retry"
                );
            },
        }
    }
    Ok(examined)
}

fn parse_record(
    record: &OutboxRecord,
) -> Result<(AlarmNotificationDestination, AlarmNotification)> {
    let destination = AlarmNotificationDestination::parse(&record.destination)?;
    let notification = serde_json::from_str(&record.payload)
        .with_context(|| format!("decode alarm outbox event {}", record.event_id))?;
    Ok((destination, notification))
}

fn retry_delay(config: NotificationDispatcherConfig, attempt_count: i64) -> Duration {
    let exponent = u32::try_from(attempt_count.saturating_sub(1).clamp(0, 20)).unwrap_or(20);
    config
        .retry_initial
        .saturating_mul(1_u32.checked_shl(exponent).unwrap_or(u32::MAX))
        .min(config.retry_max)
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;

    use super::*;
    use crate::models::AlertRule;
    use crate::notification::{AlarmCountSnapshot, AlarmNotificationDestination};

    #[derive(Default)]
    struct RecordingNotifier {
        attempts: AtomicUsize,
        fail_uplink_once: AtomicUsize,
        delivered: Mutex<Vec<(AlarmNotificationDestination, String)>>,
    }

    #[async_trait]
    impl AlarmNotifier for RecordingNotifier {
        async fn deliver_alarm(
            &self,
            destination: AlarmNotificationDestination,
            notification: &AlarmNotification,
        ) -> Result<()> {
            self.attempts.fetch_add(1, Ordering::SeqCst);
            if destination == AlarmNotificationDestination::Uplink
                && self.fail_uplink_once.fetch_add(1, Ordering::SeqCst) == 0
            {
                anyhow::bail!("synthetic uplink failure");
            }
            self.delivered
                .lock()
                .expect("delivery lock")
                .push((destination, notification.event_id.clone()));
            Ok(())
        }

        async fn publish_counts(&self, _counts: AlarmCountSnapshot) {}
    }

    struct HangingNotifier {
        started: tokio::sync::Notify,
    }

    #[async_trait]
    impl AlarmNotifier for HangingNotifier {
        async fn deliver_alarm(
            &self,
            _destination: AlarmNotificationDestination,
            _notification: &AlarmNotification,
        ) -> Result<()> {
            self.started.notify_one();
            std::future::pending().await
        }

        async fn publish_counts(&self, _counts: AlarmCountSnapshot) {}
    }

    fn test_config() -> NotificationDispatcherConfig {
        NotificationDispatcherConfig {
            poll_interval: Duration::from_millis(1),
            retry_initial: Duration::from_millis(1),
            retry_max: Duration::from_millis(4),
            shutdown_drain: Duration::from_millis(25),
        }
    }

    async fn file_pool(path: &std::path::Path) -> SqlitePool {
        sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(2)
            .connect_with(common::bootstrap_database::sqlite_connect_options(
                path.to_str().expect("UTF-8 database path"),
            ))
            .await
            .expect("file-backed alarm database")
    }

    async fn insert_rule(pool: &SqlitePool) -> AlertRule {
        let now = Utc::now().timestamp();
        let id = sqlx::query_scalar::<_, i64>(
            "INSERT INTO alert_rule
             (service_type, channel_id, data_type, point_id, rule_name,
              warning_level, operator, value, enabled, created_at, updated_at)
             VALUES ('io', 7, 'T', 3, 'temperature', 2, '>', 80, 1, ?, ?)
             RETURNING id",
        )
        .bind(now)
        .bind(now)
        .fetch_one(pool)
        .await
        .expect("insert alarm rule");
        crate::db::get_rule_by_id(pool, id)
            .await
            .expect("load rule")
            .expect("stored rule")
    }

    #[tokio::test]
    async fn committed_notification_survives_pool_restart() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("alarm.db");
        let pool = file_pool(&path).await;
        crate::db::create_tables(&pool).await.expect("alarm schema");
        let rule = insert_rule(&pool).await;
        crate::db::insert_alert(&pool, &rule, 95.0)
            .await
            .expect("insert alert")
            .expect("alert accepted");
        assert_eq!(stats(&pool).await.expect("outbox stats").pending, 2);
        pool.close().await;

        let reopened = file_pool(&path).await;
        crate::db::create_tables(&reopened)
            .await
            .expect("migrate existing alarm database");
        let notifier = RecordingNotifier::default();
        notifier.fail_uplink_once.store(1, Ordering::SeqCst);
        assert_eq!(
            dispatch_due_batch(&reopened, &notifier, test_config())
                .await
                .expect("dispatch restored rows"),
            2
        );
        assert_eq!(stats(&reopened).await.expect("outbox drained").pending, 0);
        let delivered = notifier.delivered.lock().expect("delivery lock");
        assert_eq!(delivered.len(), 2);
        assert_eq!(delivered[0].1, delivered[1].1);
    }

    #[tokio::test]
    async fn destinations_ack_independently_and_failures_retry() {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("in-memory database");
        crate::db::create_tables(&pool).await.expect("alarm schema");
        let rule = insert_rule(&pool).await;
        crate::db::insert_alert(&pool, &rule, 95.0)
            .await
            .expect("insert alert")
            .expect("alert accepted");
        let notifier = RecordingNotifier::default();

        dispatch_due_batch(&pool, &notifier, test_config())
            .await
            .expect("first dispatch");
        let first = stats(&pool).await.expect("stats after failure");
        assert_eq!((first.pending, first.failed), (1, 1));
        tokio::time::sleep(Duration::from_millis(2)).await;
        dispatch_due_batch(&pool, &notifier, test_config())
            .await
            .expect("retry dispatch");

        assert_eq!(stats(&pool).await.expect("drained stats").pending, 0);
        let delivered = notifier.delivered.lock().expect("delivery lock");
        assert_eq!(
            delivered
                .iter()
                .filter(|(destination, _)| *destination == AlarmNotificationDestination::Api)
                .count(),
            1,
            "the successful API destination is not replayed with uplink"
        );
        assert_eq!(notifier.attempts.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn concurrent_transition_winner_enqueues_one_event_per_destination() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let pool = file_pool(&directory.path().join("race.db")).await;
        crate::db::create_tables(&pool).await.expect("alarm schema");
        let rule = insert_rule(&pool).await;

        let (first, second) = tokio::join!(
            crate::db::insert_alert(&pool, &rule, 95.0),
            crate::db::insert_alert(&pool, &rule, 96.0),
        );
        let winners = [first.expect("first insert"), second.expect("second insert")]
            .into_iter()
            .filter(Option::is_some)
            .count();

        assert_eq!(winners, 1);
        assert_eq!(stats(&pool).await.expect("outbox stats").pending, 2);
        let distinct_events: i64 =
            sqlx::query_scalar("SELECT COUNT(DISTINCT event_id) FROM alarm_notification_outbox")
                .fetch_one(&pool)
                .await
                .expect("distinct events");
        assert_eq!(distinct_events, 1);
    }

    #[tokio::test]
    async fn transition_rolls_back_when_outbox_enqueue_is_unavailable() {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("in-memory database");
        crate::db::create_tables(&pool).await.expect("alarm schema");
        let rule = insert_rule(&pool).await;
        sqlx::query("DROP TABLE alarm_notification_outbox")
            .execute(&pool)
            .await
            .expect("remove outbox");

        crate::db::insert_alert(&pool, &rule, 95.0)
            .await
            .expect_err("trigger cannot commit without durable notification");
        let active: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM alert")
            .fetch_one(&pool)
            .await
            .expect("active count after rollback");
        assert_eq!(active, 0);

        create_table(&pool).await.expect("restore outbox");
        let alert_id = crate::db::insert_alert(&pool, &rule, 95.0)
            .await
            .expect("insert alert")
            .expect("alert accepted");
        let alert = crate::db::get_alert_by_id(&pool, alert_id)
            .await
            .expect("read alert")
            .expect("active alert");
        sqlx::query("DROP TABLE alarm_notification_outbox")
            .execute(&pool)
            .await
            .expect("remove outbox before recovery");

        crate::db::resolve_alert(&pool, &alert, 70.0)
            .await
            .expect_err("recovery cannot commit without durable notification");
        let active: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM alert")
            .fetch_one(&pool)
            .await
            .expect("active count after recovery rollback");
        let recoveries: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM alert_event WHERE event_type = 'recovery'")
                .fetch_one(&pool)
                .await
                .expect("recovery history after rollback");
        assert_eq!((active, recoveries), (1, 0));
    }

    #[tokio::test]
    async fn shutdown_drain_is_bounded_when_transport_hangs() {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("in-memory database");
        crate::db::create_tables(&pool).await.expect("alarm schema");
        let rule = insert_rule(&pool).await;
        crate::db::insert_alert(&pool, &rule, 95.0)
            .await
            .expect("insert alert")
            .expect("alert accepted");
        let notifier = Arc::new(HangingNotifier {
            started: tokio::sync::Notify::new(),
        });
        let started = notifier.started.notified();
        let notifier_port: Arc<dyn AlarmNotifier> = notifier.clone();
        let shutdown = CancellationToken::new();
        let task = tokio::spawn(run_notification_dispatcher(
            pool.clone(),
            notifier_port,
            test_config(),
            shutdown.clone(),
        ));
        started.await;
        shutdown.cancel();

        tokio::time::timeout(Duration::from_millis(200), task)
            .await
            .expect("dispatcher respects bounded drain")
            .expect("dispatcher task");
        assert_eq!(
            stats(&pool).await.expect("pending rows retained").pending,
            2
        );
    }
}
