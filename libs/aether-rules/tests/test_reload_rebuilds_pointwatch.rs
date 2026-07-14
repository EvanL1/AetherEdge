//! Integration test: rule reload and PointWatch publication are separate,
//! generation-pinned operations.
//!
//! The host must supply the measurement bindings and manifest from one pinned
//! service topology. The scheduler never consults an independently mutable
//! routing cache or manifest source.

#![allow(clippy::disallowed_methods)]

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use aether_domain::PointKind;
use aether_rules::{
    MeasurementRouteBinding, MemoryRuleLiveState, PointWatchDispatcher, RuleScheduler,
};
use aether_shm_bridge::{ChannelPointManifest, PhysicalPointAddress, SubscriptionBitmap};
use sqlx::SqlitePool;

async fn setup_pool() -> SqlitePool {
    let pool = SqlitePool::connect("sqlite::memory:")
        .await
        .expect("connect in-memory sqlite");

    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS rules (
            id INTEGER PRIMARY KEY,
            name TEXT NOT NULL,
            description TEXT,
            enabled INTEGER NOT NULL DEFAULT 1,
            priority INTEGER NOT NULL DEFAULT 100,
            cooldown_ms INTEGER NOT NULL DEFAULT 0,
            trigger_config TEXT,
            format TEXT NOT NULL DEFAULT 'vue-flow',
            flow_json TEXT NOT NULL,
            nodes_json TEXT NOT NULL,
            created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
            updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
        )
        "#,
    )
    .execute(&pool)
    .await
    .expect("create rules table");

    pool
}

async fn insert_onchange_rule(pool: &SqlitePool, id: i64, instance: u32, point: u32) {
    let trigger = format!(
        r#"{{"type":"on_change","point_refs":[{{"instance":{},"point_type":"measurement","point":{}}}],"time_deadband_ms":null,"value_deadband":null}}"#,
        instance, point
    );
    // nodes_json must deserialize into `RuleFlow { start_node, nodes }`.
    // A minimal valid flow has a single "start" node referenced as start_node.
    let nodes_json =
        r#"{"start_node":"start","nodes":{"start":{"type":"start","wires":{"default":[]}}}}"#;
    sqlx::query(
        r#"INSERT INTO rules
            (id, name, enabled, priority, cooldown_ms, trigger_config, flow_json, nodes_json)
           VALUES (?, ?, 1, 100, 0, ?, '{}', ?)"#,
    )
    .bind(id)
    .bind(format!("rule_{id}"))
    .bind(trigger)
    .bind(nodes_json)
    .execute(pool)
    .await
    .expect("insert rule");
}

fn measurement_bindings() -> Vec<MeasurementRouteBinding> {
    vec![MeasurementRouteBinding::new(
        5,
        10,
        PhysicalPointAddress::from_legacy_raw(1001, PointKind::Telemetry, 0),
    )]
}

fn make_manifest() -> ChannelPointManifest {
    ChannelPointManifest::from_entries([(1001, [1, 0, 0, 0])])
}

#[tokio::test]
async fn pinned_topology_rebuilds_subscription_index_after_rule_reload() {
    let pool = setup_pool().await;
    insert_onchange_rule(&pool, 1, 5, 10).await;

    let live_state = Arc::new(MemoryRuleLiveState::new());
    let mut scheduler =
        RuleScheduler::new(live_state, pool.clone(), 100, PathBuf::from("logs/test"));

    let (dispatcher, _watch_rx) = PointWatchDispatcher::new();
    let dispatcher = Arc::new(Mutex::new(dispatcher));
    let bitmap = Arc::new(SubscriptionBitmap::new_in_memory().expect("bitmap"));

    scheduler.set_point_watch_rebuild_handles(Arc::clone(&dispatcher), Arc::clone(&bitmap));

    // Sanity: nothing subscribed yet.
    assert_eq!(dispatcher.lock().unwrap().subscription_count(), 0);

    let count = scheduler.reload_rules().await.expect("reload_rules");
    assert_eq!(count, 1, "should have loaded one rule");
    assert_eq!(
        dispatcher.lock().unwrap().subscription_count(),
        0,
        "rule reload alone must not publish from an unpinned route projection"
    );
    assert!(
        scheduler
            .rebuild_point_watch(&measurement_bindings(), &make_manifest())
            .await
    );

    // After reload, the rule's (channel=1001, point=0) should be in sub_index.
    assert_eq!(
        dispatcher.lock().unwrap().subscription_count(),
        1,
        "the pinned route+manifest pair must rebuild the subscription index"
    );
}

#[tokio::test]
async fn reload_rules_no_rebuild_when_handles_unset() {
    let pool = setup_pool().await;
    insert_onchange_rule(&pool, 2, 5, 10).await;

    let live_state = Arc::new(MemoryRuleLiveState::new());
    let scheduler = RuleScheduler::new(live_state, pool, 100, PathBuf::from("logs/test"));

    // No handles set — reload should still succeed but not touch any dispatcher.
    let count = scheduler.reload_rules().await.expect("reload_rules");
    assert_eq!(count, 1);
    assert!(
        !scheduler
            .rebuild_point_watch(&measurement_bindings(), &make_manifest())
            .await
    );
}

#[tokio::test]
async fn reload_rules_after_rule_added_picks_up_new_subscription() {
    // Simulates the production flow: an admin uploads new rules via
    // aether sync / API PUT, then triggers POST /api/scheduler/reload.
    let pool = setup_pool().await;

    let live_state = Arc::new(MemoryRuleLiveState::new());
    let mut scheduler =
        RuleScheduler::new(live_state, pool.clone(), 100, PathBuf::from("logs/test"));

    let (dispatcher, _watch_rx) = PointWatchDispatcher::new();
    let dispatcher = Arc::new(Mutex::new(dispatcher));
    let bitmap = Arc::new(SubscriptionBitmap::new_in_memory().expect("bitmap"));
    scheduler.set_point_watch_rebuild_handles(Arc::clone(&dispatcher), Arc::clone(&bitmap));

    // First reload: empty DB → no subscriptions.
    let count = scheduler.reload_rules().await.expect("reload empty");
    assert_eq!(count, 0);
    scheduler
        .rebuild_point_watch(&measurement_bindings(), &make_manifest())
        .await;
    assert_eq!(dispatcher.lock().unwrap().subscription_count(), 0);

    // Admin adds a rule.
    insert_onchange_rule(&pool, 3, 5, 10).await;

    // Second reload: subscription index should grow.
    let count = scheduler.reload_rules().await.expect("reload after insert");
    assert_eq!(count, 1);
    scheduler
        .rebuild_point_watch(&measurement_bindings(), &make_manifest())
        .await;
    assert_eq!(
        dispatcher.lock().unwrap().subscription_count(),
        1,
        "newly-added rule's subscription must be visible after reload_rules"
    );
}
