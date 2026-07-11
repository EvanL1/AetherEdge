//! Integration test: `reload_rules` rebuilds the PointWatch subscription
//! index when rebuild handles have been wired in.
//!
//! This protects the property that a `POST /api/scheduler/reload` call
//! (or any equivalent runtime path that calls `reload_rules`) propagates
//! rule subscription changes to the SHM `SubscriptionBitmap` and
//! `PointWatchDispatcher` sub-index without a service restart.

#![allow(clippy::disallowed_methods)]

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use aether_routing::RoutingCache;
use aether_rtdb_shm::{ChannelToSlotIndex, SubscriptionBitmap};
use aether_rules::{MemoryRuleLiveState, PointWatchDispatcher, RuleScheduler};
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

fn make_routing() -> Arc<RoutingCache> {
    // C2M route: channel=1001, T, point=0 → instance=5, point=10
    let mut c2m = HashMap::new();
    c2m.insert("1001:T:0".to_string(), "5:M:10".to_string());
    Arc::new(RoutingCache::from_maps(c2m, HashMap::new(), HashMap::new()))
}

#[tokio::test]
async fn reload_rules_rebuilds_subscription_index_when_handles_set() {
    let pool = setup_pool().await;
    insert_onchange_rule(&pool, 1, 5, 10).await;

    let live_state = Arc::new(MemoryRuleLiveState::new());
    let routing = make_routing();

    let mut scheduler = RuleScheduler::new(
        live_state,
        Arc::clone(&routing),
        pool.clone(),
        100,
        PathBuf::from("logs/test"),
    );

    let (dispatcher, _watch_rx) = PointWatchDispatcher::new();
    let dispatcher = Arc::new(Mutex::new(dispatcher));
    let slot_idx = Arc::new(ChannelToSlotIndex::empty_for_test());
    let bitmap = Arc::new(SubscriptionBitmap::new_in_memory().expect("bitmap"));

    scheduler.set_point_watch_rebuild_handles(
        Arc::clone(&dispatcher),
        Arc::clone(&routing),
        Arc::clone(&slot_idx),
        Arc::clone(&bitmap),
    );

    // Sanity: nothing subscribed yet.
    assert_eq!(dispatcher.lock().unwrap().subscription_count(), 0);

    let count = scheduler.reload_rules().await.expect("reload_rules");
    assert_eq!(count, 1, "should have loaded one rule");

    // After reload, the rule's (channel=1001, point=0) should be in sub_index.
    assert_eq!(
        dispatcher.lock().unwrap().subscription_count(),
        1,
        "reload_rules must rebuild the PointWatch subscription index"
    );
}

#[tokio::test]
async fn reload_rules_no_rebuild_when_handles_unset() {
    let pool = setup_pool().await;
    insert_onchange_rule(&pool, 2, 5, 10).await;

    let live_state = Arc::new(MemoryRuleLiveState::new());
    let routing = make_routing();

    let scheduler = RuleScheduler::new(
        live_state,
        Arc::clone(&routing),
        pool,
        100,
        PathBuf::from("logs/test"),
    );

    // No handles set — reload should still succeed but not touch any dispatcher.
    let count = scheduler.reload_rules().await.expect("reload_rules");
    assert_eq!(count, 1);
}

#[tokio::test]
async fn reload_rules_after_rule_added_picks_up_new_subscription() {
    // Simulates the production flow: an admin uploads new rules via
    // aether sync / API PUT, then triggers POST /api/scheduler/reload.
    let pool = setup_pool().await;

    let live_state = Arc::new(MemoryRuleLiveState::new());
    let routing = make_routing();

    let mut scheduler = RuleScheduler::new(
        live_state,
        Arc::clone(&routing),
        pool.clone(),
        100,
        PathBuf::from("logs/test"),
    );

    let (dispatcher, _watch_rx) = PointWatchDispatcher::new();
    let dispatcher = Arc::new(Mutex::new(dispatcher));
    let slot_idx = Arc::new(ChannelToSlotIndex::empty_for_test());
    let bitmap = Arc::new(SubscriptionBitmap::new_in_memory().expect("bitmap"));
    scheduler.set_point_watch_rebuild_handles(
        Arc::clone(&dispatcher),
        Arc::clone(&routing),
        Arc::clone(&slot_idx),
        Arc::clone(&bitmap),
    );

    // First reload: empty DB → no subscriptions.
    let count = scheduler.reload_rules().await.expect("reload empty");
    assert_eq!(count, 0);
    assert_eq!(dispatcher.lock().unwrap().subscription_count(), 0);

    // Admin adds a rule.
    insert_onchange_rule(&pool, 3, 5, 10).await;

    // Second reload: subscription index should grow.
    let count = scheduler.reload_rules().await.expect("reload after insert");
    assert_eq!(count, 1);
    assert_eq!(
        dispatcher.lock().unwrap().subscription_count(),
        1,
        "newly-added rule's subscription must be visible after reload_rules"
    );
}
