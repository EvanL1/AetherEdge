//! PointWatch rebuilds consume one immutable automation topology generation.

#![allow(clippy::disallowed_methods)]

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use aether_automation::infra::runtime_topology::AutomationTopologyHandle;
use aether_rules::{MemoryRuleLiveState, PointWatchDispatcher, PointWatchHint, RuleScheduler};
use aether_shm_bridge::{ShmDeviceCommandSink, SubscriptionBitmap};

async fn topology_pool() -> sqlx::SqlitePool {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .expect("topology database");
    common::test_utils::schema::init_automation_schema(&pool)
        .await
        .expect("automation schema");
    common::test_utils::schema::init_io_schema(&pool)
        .await
        .expect("IO schema");
    common::test_utils::schema::init_rules_schema(&pool)
        .await
        .expect("rules schema");

    for statement in [
        "INSERT INTO instances (instance_id, instance_name, product_name) VALUES (5, 'device', 'fixture')",
        "INSERT INTO channels (channel_id, name, protocol, enabled) VALUES (10, 'old', 'virtual', 1)",
        "INSERT INTO channels (channel_id, name, protocol, enabled) VALUES (20, 'new', 'virtual', 1)",
        "INSERT INTO telemetry_points (channel_id, point_id, signal_name) VALUES (10, 0, 'old-temperature')",
        "INSERT INTO telemetry_points (channel_id, point_id, signal_name) VALUES (20, 0, 'new-temperature')",
        "INSERT INTO measurement_routing (instance_id, instance_name, channel_id, channel_type, channel_point_id, measurement_id, enabled) VALUES (5, 'device', 10, 'T', 0, 10, 1)",
        r#"INSERT INTO rules (id, name, enabled, priority, cooldown_ms, trigger_config, nodes_json, flow_json)
           VALUES (43, 'temperature-change', 1, 100, 0,
             '{"type":"on_change","point_refs":[{"instance":5,"point_type":"measurement","point":10}]}',
             '{"start_node":"start","nodes":{"start":{"type":"start","wires":{"default":[]}}}}',
             '{}')"#,
    ] {
        sqlx::query(statement)
            .execute(&pool)
            .await
            .expect("seed topology");
    }
    pool
}

#[tokio::test]
async fn rebuild_uses_the_route_and_manifest_of_one_pinned_service_generation() {
    let pool = topology_pool().await;
    let snapshot = aether_store_local::load_sqlite_live_topology(&pool)
        .await
        .expect("initial topology");
    let topology = AutomationTopologyHandle::new_lazy(
        "/not-opened/point.shm",
        "/not-opened/health.shm",
        snapshot,
        Arc::new(ShmDeviceCommandSink::new()),
    )
    .expect("lazy service topology");

    let mut scheduler = RuleScheduler::new(
        Arc::new(MemoryRuleLiveState::new()),
        pool.clone(),
        100,
        PathBuf::from("logs/test-point-watch-generation"),
    );
    let (dispatcher, mut events) = PointWatchDispatcher::new();
    let dispatcher = Arc::new(Mutex::new(dispatcher));
    let bitmap = Arc::new(SubscriptionBitmap::new_in_memory().expect("bitmap"));
    scheduler.set_point_watch_rebuild_handles(Arc::clone(&dispatcher), Arc::clone(&bitmap));
    scheduler.reload_rules().await.expect("rule reload");

    let original = topology.load();
    assert!(original.rebuild_point_watch(&scheduler).await);
    dispatcher
        .lock()
        .expect("dispatcher")
        .dispatch(PointWatchHint::new(10, 0, 1.0, 1.0, 1));
    assert_eq!(
        events.try_recv().expect("original route").rule_ids,
        vec![43]
    );

    sqlx::query(
        "UPDATE measurement_routing SET channel_id = 20 \
         WHERE instance_id = 5 AND measurement_id = 10",
    )
    .execute(&pool)
    .await
    .expect("route-only mutation");
    assert!(topology.refresh(&pool).await.expect("route publication"));
    let replacement = topology.load();
    assert!(!Arc::ptr_eq(&original, &replacement));

    // A retained generation remains internally coherent even after the handle
    // publishes its replacement: it rebuilds the old route with the old view.
    assert!(original.rebuild_point_watch(&scheduler).await);
    dispatcher
        .lock()
        .expect("dispatcher")
        .dispatch(PointWatchHint::new(20, 0, 2.0, 2.0, 2));
    assert!(events.try_recv().is_err());
    dispatcher
        .lock()
        .expect("dispatcher")
        .dispatch(PointWatchHint::new(10, 0, 1.0, 1.0, 3));
    assert_eq!(
        events.try_recv().expect("retained route").rule_ids,
        vec![43]
    );

    // Publishing the replacement generation clears the old subscription and
    // installs only the replacement binding; no RoutingCache head is read.
    assert!(replacement.rebuild_point_watch(&scheduler).await);
    dispatcher
        .lock()
        .expect("dispatcher")
        .dispatch(PointWatchHint::new(10, 0, 1.0, 1.0, 4));
    assert!(events.try_recv().is_err());
    dispatcher
        .lock()
        .expect("dispatcher")
        .dispatch(PointWatchHint::new(20, 0, 2.0, 2.0, 5));
    assert_eq!(
        events.try_recv().expect("replacement route").rule_ids,
        vec![43]
    );
}
