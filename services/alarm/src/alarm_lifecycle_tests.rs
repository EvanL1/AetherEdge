use super::check_single_rule;
use std::sync::{Arc, Mutex};

use aether_domain::{
    AlarmComparator, AlarmRuleDefinition, AlarmRuleTarget, AlarmSeverity, ChannelId, PointId,
    PointQuality,
};
use aether_ports::{AlarmRuleMutation, AlarmRuleMutator, PortResult};
use aether_shm_bridge::SlotSnapshot;
use async_trait::async_trait;

use crate::alarm_rule_mutation::SqliteAlarmRuleMutator;
use crate::live_values::AlarmValueSource;
use crate::models::{AlertEvent, AlertRule, MonitorStatus};
use crate::notification::{AlarmCountSnapshot, AlarmNotification, AlarmNotifier};
use crate::{db, state::AppState};

struct FixedValue(f64);

impl AlarmValueSource for FixedValue {
    fn read_rule(&self, _rule: &AlertRule) -> PortResult<Option<SlotSnapshot>> {
        Ok(Some(SlotSnapshot::new(self.0, 1_000, PointQuality::Good)))
    }

    fn watched_slot(&self, _rule: &AlertRule) -> PortResult<Option<usize>> {
        Ok(None)
    }
}

#[derive(Default)]
struct Notifications {
    statuses: Mutex<Vec<u8>>,
    counts: Mutex<Vec<i64>>,
}

#[async_trait]
impl AlarmNotifier for Notifications {
    async fn publish_alarm(&self, notification: AlarmNotification) {
        self.statuses
            .lock()
            .expect("statuses")
            .push(notification.status);
    }

    async fn replay_alarm(&self, _notification: AlarmNotification) {}

    async fn publish_counts(&self, counts: AlarmCountSnapshot) {
        self.counts.lock().expect("counts").push(counts.total);
    }
}

async fn fixture() -> (
    Arc<AppState>,
    Arc<SqliteAlarmRuleMutator>,
    Arc<Notifications>,
) {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .expect("database");
    db::create_tables(&pool).await.expect("schema");
    let notifications = Arc::new(Notifications::default());
    let notifier: Arc<dyn AlarmNotifier> = notifications.clone();
    let adapter = Arc::new(SqliteAlarmRuleMutator::new(pool.clone(), notifier.clone()));
    let audit: Arc<dyn aether_ports::AuditSink> =
        Arc::new(aether_store_local::MemoryAuditSink::new());
    let config = Arc::new(crate::config::AlarmConfig::default());
    let state = Arc::new(AppState {
        db: pool,
        live_values: Arc::new(FixedValue(95.0)),
        config,
        point_watch_capacity: 128,
        notifier,
        monitor_status: Arc::new(tokio::sync::RwLock::new(MonitorStatus {
            running: false,
            last_check_time: None,
            check_interval: 5,
        })),
        rule_application: Arc::new(aether_application::AlarmRuleApplication::new(
            adapter.clone(),
            audit.clone(),
            aether_application::SafetyPolicy,
        )),
        alert_resolution_application: Arc::new(
            aether_application::AlertResolutionApplication::new(
                adapter.clone(),
                audit,
                aether_application::SafetyPolicy,
            ),
        ),
        access_authenticator: Arc::new(
            aether_auth_jwt::AccessTokenAuthenticator::new("test-only-alarm-jwt-secret-32-bytes")
                .expect("test authenticator"),
        ),
    });
    let definition = AlarmRuleDefinition::new(
        AlarmRuleTarget::point("io", ChannelId::new(7), "T", PointId::new(3)).expect("target"),
        "temperature",
        AlarmSeverity::new(2).expect("severity"),
        AlarmComparator::GreaterThan,
        80.0,
        true,
        None,
    )
    .expect("definition");
    adapter
        .mutate(AlarmRuleMutation::create(definition))
        .await
        .expect("rule");
    (state, adapter, notifications)
}

#[tokio::test]
async fn tick_loaded_before_disable_cannot_create_a_zombie_alarm_or_notify() {
    let (state, adapter, notifications) = fixture().await;
    // This is the exact race ordering: the monitor loads enabled rules, then
    // disable commits before its in-flight rule check can insert an alert.
    let mut tick_rules = db::get_all_enabled_rules(&state.db)
        .await
        .expect("tick snapshot");
    let rule = tick_rules.pop().expect("enabled rule");
    adapter
        .mutate(AlarmRuleMutation::set_enabled(
            aether_domain::AlarmRuleId::new(rule.id as u64),
            false,
        ))
        .await
        .expect("disable commits");

    check_single_rule(state.clone(), rule).await;

    assert_eq!(
        db::get_active_alarm_counts(&state.db)
            .await
            .expect("counts")
            .total,
        0
    );
    let events: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM alert_event")
        .fetch_one(&state.db)
        .await
        .expect("events");
    assert_eq!(events, 0);
    assert!(notifications.statuses.lock().expect("statuses").is_empty());
    assert!(notifications.counts.lock().expect("counts").is_empty());
}

#[tokio::test]
async fn trigger_history_is_written_once_and_retained_after_recovery() {
    let (state, _adapter, notifications) = fixture().await;
    let rule = db::get_all_enabled_rules(&state.db)
        .await
        .expect("rules")
        .pop()
        .expect("rule");
    check_single_rule(state.clone(), rule.clone()).await;
    check_single_rule(state.clone(), rule.clone()).await;

    let events: Vec<AlertEvent> = sqlx::query_as("SELECT * FROM alert_event ORDER BY id")
        .fetch_all(&state.db)
        .await
        .expect("history");
    assert_eq!(events.len(), 1, "one trigger per alarm episode");
    let event = &events[0];
    assert_eq!(event.event_type, "trigger");
    assert_eq!(
        (event.rule_id, event.channel_id, event.point_id),
        (rule.id, 7, 3)
    );
    assert_eq!(event.trigger_value, Some(95.0));
    assert_eq!(event.recovery_value, None);
    assert_eq!(event.recovered_at, None);
    assert_eq!(event.duration, None);
    let alert = db::get_alert_by_rule_id(&state.db, rule.id)
        .await
        .expect("alert")
        .expect("active");
    assert_eq!(event.triggered_at, Some(alert.triggered_at));
    assert_eq!(event.rule_snapshot, alert.rule_snapshot);
    assert_eq!(
        db::get_statistics(&state.db).await.expect("statistics")["today_events"],
        1
    );

    let mut recovered_state = Arc::try_unwrap(state).ok().expect("sole state owner");
    recovered_state.live_values = Arc::new(FixedValue(70.0));
    let recovered_state = Arc::new(recovered_state);
    check_single_rule(recovered_state.clone(), rule).await;
    let event_types: Vec<String> =
        sqlx::query_scalar("SELECT event_type FROM alert_event ORDER BY id")
            .fetch_all(&recovered_state.db)
            .await
            .expect("episode history");
    assert_eq!(event_types, ["trigger", "recovery"]);
    assert_eq!(*notifications.statuses.lock().expect("statuses"), [1, 0]);
    assert_eq!(*notifications.counts.lock().expect("counts"), [1, 0]);
}

#[tokio::test]
async fn failed_trigger_history_rolls_back_active_alarm_without_notification() {
    let (state, _adapter, notifications) = fixture().await;
    let rule = db::get_all_enabled_rules(&state.db)
        .await
        .expect("rules")
        .pop()
        .expect("rule");
    sqlx::query(
        "CREATE TRIGGER reject_trigger BEFORE INSERT ON alert_event
        WHEN NEW.event_type = 'trigger' BEGIN SELECT RAISE(ABORT, 'history unavailable'); END",
    )
    .execute(&state.db)
    .await
    .expect("history write failure");

    check_single_rule(state.clone(), rule).await;

    assert_eq!(
        db::get_active_alarm_counts(&state.db)
            .await
            .expect("counts")
            .total,
        0
    );
    assert!(notifications.statuses.lock().expect("statuses").is_empty());
    assert!(notifications.counts.lock().expect("counts").is_empty());
}
