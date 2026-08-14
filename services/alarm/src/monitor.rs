//! Alarm monitoring engine
//!
//! Polls enabled rules every `data_fetch_interval` seconds, reads current
//! values from SHM and creates/resolves alerts accordingly.

use std::collections::HashSet;
use std::future::Future;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use aether_shm_bridge::{
    PointWatchEvent, PointWatchEventListener, SubscriptionBitmap, bitmap_path_for_consumer,
};
use chrono::Utc;
use futures::StreamExt;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

use crate::db;
use crate::models::{Alert, AlertRule};
use crate::notification::AlarmCountSnapshot;
use crate::state::AppState;

const MAX_CONCURRENT_RULE_CHECKS: usize = 16;

#[derive(Debug, Clone, Copy, Default)]
struct RuleCheckOutcome {
    active_count_changed: bool,
}

/// Turn the configured poll interval into a tick period.
///
/// `tokio::time::interval` panics on a zero period and the workspace release
/// profile sets `panic = "abort"`, so an unclamped `DATA_FETCH_INTERVAL=0` would
/// take the whole alarm service down at startup.
fn monitor_interval(configured_secs: u64) -> Duration {
    Duration::from_secs(configured_secs.max(1))
}

pub async fn run_monitor(state: Arc<AppState>, shutdown: CancellationToken) {
    let interval = monitor_interval(state.config.data_fetch_interval);
    info!(
        "Alarm monitor started (interval={}s)",
        state.config.data_fetch_interval
    );

    // Mark as running
    {
        let mut ms = state.monitor_status.write().await;
        ms.running = true;
    }

    let (listener, mut event_rx) =
        PointWatchEventListener::new(&state.config.point_watch_socket, shutdown.clone());
    let mut listener_task = tokio::spawn(async move {
        if let Err(error) = listener.run().await {
            warn!(
                "Alarm PointWatch listener unavailable; polling fallback remains active: {error}"
            );
        }
    });
    let mut ticker = tokio::time::interval(interval);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut events_open = true;

    loop {
        tokio::select! {
            _ = shutdown.cancelled() => {
                info!("Alarm monitor shutting down");
                break;
            }
            _ = ticker.tick() => {
                check_all_rules(&state).await;
            }
            event = event_rx.recv(), if events_open => {
                match event {
                    Some(event) => check_event_batch(&state, &mut event_rx, event, &shutdown).await,
                    None => events_open = false,
                }
            }
        }
    }

    if tokio::time::timeout(Duration::from_secs(2), &mut listener_task)
        .await
        .is_err()
    {
        warn!("Alarm PointWatch listener exceeded its shutdown deadline; aborting it");
        listener_task.abort();
        let _ = listener_task.await;
    }

    let mut ms = state.monitor_status.write().await;
    ms.running = false;
}

pub async fn run_alarm_count_broadcaster(state: Arc<AppState>, shutdown: CancellationToken) {
    info!("Alarm count broadcast task started (interval=30s)");
    loop {
        tokio::select! {
            _ = shutdown.cancelled() => break,
            _ = tokio::time::sleep(Duration::from_secs(30)) => {
                send_alarm_count_broadcast(&state).await;
            }
        }
    }
}

async fn check_all_rules(state: &Arc<AppState>) {
    let rules = match db::get_all_enabled_rules(&state.db).await {
        Ok(r) => r,
        Err(e) => {
            error!("Failed to load enabled rules: {}", e);
            return;
        },
    };

    if rules.is_empty() {
        debug!("No enabled rules to check");
        reconcile_point_watch_subscriptions(state, &rules);
        state.monitor_status.write().await.last_check_time = Some(Utc::now().timestamp());
        return;
    }

    debug!("Checking {} enabled rules", rules.len());
    reconcile_point_watch_subscriptions(state, &rules);
    process_rules(state, rules).await;

    state.monitor_status.write().await.last_check_time = Some(Utc::now().timestamp());
}

async fn process_rules(state: &Arc<AppState>, rules: Vec<AlertRule>) {
    if rules.is_empty() {
        return;
    }

    let active_alerts = match db::get_active_alerts_by_rule_id(&state.db).await {
        Ok(alerts) => alerts,
        Err(error) => {
            error!("Failed to load active alerts for monitor pass: {error}");
            return;
        },
    };
    let outcomes = collect_bounded(rules, MAX_CONCURRENT_RULE_CHECKS, |rule| {
        let existing_alert = active_alerts.get(&rule.id).cloned();
        let state = Arc::clone(state);
        async move { check_single_rule(state, rule, existing_alert).await }
    })
    .await;

    if outcomes.iter().any(|outcome| outcome.active_count_changed) {
        // Alarm transitions are already durable in the SQLite outbox. Counts
        // are a reconstructable best-effort snapshot and can be coalesced once
        // after all rule state changes commit.
        send_alarm_count_broadcast(state).await;
    }
}

async fn collect_bounded<T, U, F, Fut>(items: Vec<T>, max_concurrency: usize, task: F) -> Vec<U>
where
    F: FnMut(T) -> Fut,
    Fut: Future<Output = U>,
{
    futures::stream::iter(items)
        .map(task)
        .buffer_unordered(max_concurrency.max(1))
        .collect()
        .await
}

async fn check_event_batch(
    state: &Arc<AppState>,
    event_rx: &mut tokio::sync::mpsc::Receiver<PointWatchEvent>,
    first: PointWatchEvent,
    shutdown: &CancellationToken,
) {
    let mut events = vec![first];
    tokio::select! {
        _ = shutdown.cancelled() => return,
        _ = tokio::time::sleep(Duration::from_millis(state.config.point_watch_debounce_ms)) => {}
    }
    while let Ok(event) = event_rx.try_recv() {
        events.push(event);
    }
    let mut addresses = HashSet::new();
    for event in events {
        match state.live_values.validate_point_watch(event) {
            Ok(Some(validated)) => {
                addresses.insert(validated.address());
            },
            Ok(None) => {},
            Err(error) => warn!(
                channel_id = event.channel_id(),
                point_id = event.point_id(),
                slot = event.slot_index(),
                "Alarm PointWatch SHM re-read rejected: {error}"
            ),
        }
    }
    if addresses.is_empty() {
        return;
    }

    let rules = match db::get_all_enabled_rules(&state.db).await {
        Ok(rules) => rules,
        Err(error) => {
            error!("Failed to load enabled rules for PointWatch event: {error}");
            return;
        },
    };
    let matching = rules
        .into_iter()
        .filter(|rule| match state.live_values.watched_address(rule) {
            Ok(Some(address)) => addresses.contains(&address),
            Ok(None) => false,
            Err(error) => {
                warn!(
                    "Cannot resolve PointWatch address for rule '{}': {error}",
                    rule.rule_name
                );
                false
            },
        })
        .collect::<Vec<_>>();
    if !matching.is_empty() {
        debug!("PointWatch woke {} alarm rule(s)", matching.len());
        process_rules(state, matching).await;
    }
}

fn reconcile_point_watch_subscriptions(state: &AppState, rules: &[crate::models::AlertRule]) {
    let bitmap_path = bitmap_path_for_consumer(Path::new(&state.config.shm_path), "alarm");
    let bitmap = match SubscriptionBitmap::open_or_create(&bitmap_path, state.point_watch_capacity)
    {
        Ok(bitmap) => bitmap,
        Err(error) => {
            debug!(
                "Alarm PointWatch bitmap not available at {}: {error}",
                bitmap_path.display()
            );
            return;
        },
    };
    bitmap.clear_all();
    for rule in rules {
        match state.live_values.watched_slot(rule) {
            Ok(Some(slot)) => {
                if let Err(error) = bitmap.set_watched(slot) {
                    warn!(
                        "Cannot subscribe alarm rule '{}' to PointWatch slot {slot}: {error}",
                        rule.rule_name
                    );
                }
            },
            Ok(None) => {},
            Err(error) => warn!(
                "Cannot subscribe alarm rule '{}' to PointWatch: {error}",
                rule.rule_name
            ),
        }
    }
    debug!(
        "Alarm PointWatch subscriptions reconciled: {} slot(s)",
        bitmap.subscription_count()
    );
}

async fn check_single_rule(
    state: Arc<AppState>,
    rule: AlertRule,
    existing_alert: Option<Alert>,
) -> RuleCheckOutcome {
    let current_value = match state.live_values.read_rule(&rule) {
        Ok(Some(sample)) => sample.value(),
        Ok(None) => {
            debug!(
                "No live SHM data for rule '{}' at logical_key={} point_id={}",
                rule.rule_name,
                rule.logical_key(),
                rule.point_id
            );
            return RuleCheckOutcome::default();
        },
        Err(e) => {
            warn!(
                retryable = e.is_retryable(),
                "SHM read failed for rule '{}': {}", rule.rule_name, e
            );
            return RuleCheckOutcome::default();
        },
    };

    let is_triggered = rule.evaluate(current_value);

    if is_triggered {
        if let Some(alert) = existing_alert {
            // Already active – just update current value
            if let Err(e) = db::update_alert_value(&state.db, alert.id, current_value).await {
                error!("Failed to update alert value: {}", e);
            }
            debug!(
                "Updated alert '{}': value={}",
                rule.rule_name, current_value
            );
        } else {
            // New alarm triggered
            match db::insert_alert(&state.db, &rule, current_value).await {
                Ok(Some(_alert_id)) => {
                    warn!(
                        "ALARM TRIGGERED: rule='{}' value={} {} {}",
                        rule.rule_name, current_value, rule.operator, rule.value
                    );
                    return RuleCheckOutcome {
                        active_count_changed: true,
                    };
                },
                Ok(None) => debug!(
                    "Skipped stale or overlapping alert creation for rule '{}'",
                    rule.rule_name
                ),
                Err(e) => {
                    error!(
                        "Failed to insert alert for rule '{}': {}",
                        rule.rule_name, e
                    );
                },
            }
        }
    } else if let Some(alert) = existing_alert {
        // Alarm recovered
        match db::resolve_alert(&state.db, &alert, current_value).await {
            Ok(Some(_event_id)) => {
                info!(
                    "ALARM RECOVERED: rule='{}' value={}",
                    rule.rule_name, current_value
                );
                return RuleCheckOutcome {
                    active_count_changed: true,
                };
            },
            Ok(None) => debug!(
                "Alert '{}' was already resolved by a concurrent path",
                rule.rule_name
            ),
            Err(e) => {
                error!(
                    "Failed to resolve alert for rule '{}': {}",
                    rule.rule_name, e
                );
            },
        }
    }
    RuleCheckOutcome::default()
}

async fn send_alarm_count_broadcast(state: &Arc<AppState>) {
    match db::get_active_alarm_counts(&state.db).await {
        Ok(counts) => {
            state
                .notifier
                .publish_counts(AlarmCountSnapshot::from(&counts))
                .await;
        },
        Err(e) => {
            error!("Failed to get alarm counts: {}", e);
        },
    }
}

/// Manual rule check (for the `/monitor/check-rule/{id}` endpoint).
#[derive(Debug)]
pub enum ManualCheckError {
    RuleNotFound,
    Internal(anyhow::Error),
}

impl std::fmt::Display for ManualCheckError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::RuleNotFound => formatter.write_str("Rule not found"),
            Self::Internal(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for ManualCheckError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::RuleNotFound => None,
            Self::Internal(error) => Some(error.as_ref()),
        }
    }
}

impl From<anyhow::Error> for ManualCheckError {
    fn from(error: anyhow::Error) -> Self {
        Self::Internal(error)
    }
}

pub async fn manual_check_rule(
    state: &Arc<AppState>,
    rule_id: i64,
) -> Result<serde_json::Value, ManualCheckError> {
    let rule = db::get_rule_by_id(&state.db, rule_id)
        .await?
        .ok_or(ManualCheckError::RuleNotFound)?;

    if !rule.enabled {
        return Ok(serde_json::json!({
            "success": false,
            "message": "Rule is disabled",
            "data": {},
        }));
    }

    let sample = state
        .live_values
        .read_rule(&rule)
        .map_err(|error| anyhow::anyhow!(error))?;

    let Some(sample) = sample else {
        return Ok(serde_json::json!({
            "success": false,
            "message": "Failed to retrieve live data from SHM",
            "data": {
                "logical_key": rule.logical_key(),
                "point_id": rule.point_id,
                "data_source": "shm",
            },
        }));
    };

    let current_value = sample.value();
    let is_triggered = rule.evaluate(current_value);
    let has_active = db::get_alert_by_rule_id(&state.db, rule.id)
        .await?
        .is_some();

    Ok(serde_json::json!({
        "success": true,
        "message": "Manual check completed",
        "data": {
            "rule_name": rule.rule_name,
            "current_value": current_value,
            "threshold_value": rule.value,
            "operator": rule.operator,
            "is_triggered": is_triggered,
            "has_active_alert": has_active,
            "logical_key": rule.logical_key(),
            "point_id": rule.point_id,
            "data_source": "shm",
            "sample_timestamp_ms": sample.timestamp_ms(),
            "check_time": chrono::Utc::now().to_rfc3339(),
        },
    }))
}

#[cfg(test)]
mod interval_tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use aether_domain::PointQuality;
    use aether_ports::PortResult;
    use aether_shm_bridge::SlotSnapshot;

    use super::*;
    use crate::notification::{AlarmNotification, AlarmNotificationDestination};

    struct StaticAlarmValues;

    impl crate::live_values::AlarmValueSource for StaticAlarmValues {
        fn read_rule(&self, _rule: &AlertRule) -> PortResult<Option<SlotSnapshot>> {
            Ok(Some(SlotSnapshot::new(
                100.0,
                aether_shm_bridge::timestamp_ms(),
                PointQuality::Good,
            )))
        }

        fn watched_slot(&self, _rule: &AlertRule) -> PortResult<Option<usize>> {
            Ok(None)
        }
    }

    #[derive(Default)]
    struct RecordingNotifier {
        alarms: AtomicUsize,
        counts: AtomicUsize,
        events: std::sync::Mutex<Vec<&'static str>>,
    }

    #[async_trait::async_trait]
    impl crate::notification::AlarmNotifier for RecordingNotifier {
        async fn deliver_alarm(
            &self,
            _destination: AlarmNotificationDestination,
            _notification: &AlarmNotification,
        ) -> anyhow::Result<()> {
            self.alarms.fetch_add(1, Ordering::SeqCst);
            self.events.lock().expect("event lock").push("alarm");
            Ok(())
        }

        async fn publish_counts(&self, _counts: AlarmCountSnapshot) {
            self.counts.fetch_add(1, Ordering::SeqCst);
            self.events.lock().expect("event lock").push("counts");
        }
    }

    async fn monitor_state(notifier: Arc<RecordingNotifier>) -> Arc<AppState> {
        let db = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("in-memory alarm database");
        db::create_tables(&db).await.expect("alarm schema");
        let notifier_port: Arc<dyn crate::notification::AlarmNotifier> = notifier;
        let alarm_store = Arc::new(crate::alarm_rule_mutation::SqliteAlarmRuleMutator::new(
            db.clone(),
            Arc::clone(&notifier_port),
        ));
        let audit: Arc<dyn aether_ports::AuditSink> =
            Arc::new(aether_store_local::MemoryAuditSink::new());
        let config = Arc::new(crate::config::AlarmConfig::default());
        Arc::new(AppState {
            db,
            live_values: Arc::new(StaticAlarmValues),
            config: Arc::clone(&config),
            point_watch_capacity: 128,
            notifier: notifier_port,
            monitor_status: Arc::new(tokio::sync::RwLock::new(crate::models::MonitorStatus {
                running: false,
                last_check_time: None,
                check_interval: config.data_fetch_interval,
            })),
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
                aether_auth_jwt::AccessTokenAuthenticator::new(
                    "alarm-monitor-test-secret-at-least-32-bytes",
                )
                .expect("valid test JWT secret"),
            ),
        })
    }

    #[test]
    fn a_zero_configured_interval_is_clamped_instead_of_panicking() {
        // `tokio::time::interval` panics on a zero period, and the release profile
        // sets `panic = "abort"`, so `DATA_FETCH_INTERVAL=0` killed the service.
        assert_eq!(monitor_interval(0), Duration::from_secs(1));
    }

    #[test]
    fn a_configured_interval_is_preserved() {
        assert_eq!(monitor_interval(30), Duration::from_secs(30));
    }

    #[tokio::test]
    async fn rule_checks_never_exceed_the_hard_concurrency_limit() {
        const LIMIT: usize = 3;
        let active = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));

        let completed = collect_bounded((0..24).collect(), LIMIT, |_| {
            let active = Arc::clone(&active);
            let peak = Arc::clone(&peak);
            async move {
                let running = active.fetch_add(1, Ordering::SeqCst) + 1;
                peak.fetch_max(running, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(2)).await;
                active.fetch_sub(1, Ordering::SeqCst);
            }
        })
        .await;

        assert_eq!(completed.len(), 24);
        assert_eq!(peak.load(Ordering::SeqCst), LIMIT);
    }

    #[tokio::test]
    async fn one_monitor_pass_persists_notifications_and_broadcasts_counts_once() {
        let notifier = Arc::new(RecordingNotifier::default());
        let state = monitor_state(Arc::clone(&notifier)).await;
        let now = Utc::now().timestamp();
        for point_id in [1_i64, 2] {
            sqlx::query(
                "INSERT INTO alert_rule
                 (service_type, channel_id, data_type, point_id, rule_name,
                  warning_level, operator, value, enabled, created_at, updated_at)
                 VALUES ('io', 7, 'T', ?, ?, 2, '>', 80, 1, ?, ?)",
            )
            .bind(point_id)
            .bind(format!("temperature-{point_id}"))
            .bind(now)
            .bind(now)
            .execute(&state.db)
            .await
            .expect("insert alarm rule");
        }
        let rules = db::get_all_enabled_rules(&state.db)
            .await
            .expect("load rules");

        process_rules(&state, rules).await;

        assert_eq!(notifier.alarms.load(Ordering::SeqCst), 0);
        assert_eq!(
            notifier.counts.load(Ordering::SeqCst),
            1,
            "a pass with multiple state changes emits one aggregate count"
        );
        assert_eq!(
            *notifier.events.lock().expect("event lock"),
            ["counts"],
            "durable alert delivery belongs to the independent outbox dispatcher"
        );
        assert_eq!(
            crate::notification_outbox::stats(&state.db)
                .await
                .expect("notification outbox stats")
                .pending,
            4,
            "two alarm transitions each enqueue API and uplink destinations"
        );
        let active: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM alert")
            .fetch_one(&state.db)
            .await
            .expect("active alert count");
        assert_eq!(active, 2);
    }
}
