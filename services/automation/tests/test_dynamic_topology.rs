//! Coherent automation runtime-topology publication contracts.

use std::sync::Arc;

use aether_automation::infra::action_routing::SqliteActionRoutingMutator;
use aether_automation::infra::runtime_topology::AutomationTopologyHandle;
use aether_automation::{InstanceManager, ProductLoader};
use aether_domain::{
    AcquiredPointSample, ChannelId, ChannelPointAddress, InstanceId, PointId, PointKind,
    PointQuality, TimestampMs,
};
use aether_ports::{
    ActionRouteKey, AutomationActionRoutingMutator, LogicalRoutingRevision, PortErrorKind,
    RevisionedActionRoutingMutation,
};
use aether_shm_bridge::{
    PointWatchEvent, ShmChannelHealthWriterHandle, ShmDeviceCommandSink, ShmRuntimeConfig,
    ShmWriterHandle, commit_topology_publication,
};

async fn create_topology_pool() -> sqlx::SqlitePool {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .expect("open topology database");
    common::test_utils::schema::init_automation_schema(&pool)
        .await
        .expect("automation schema");
    common::test_utils::schema::init_io_schema(&pool)
        .await
        .expect("IO schema");
    for statement in [
        "INSERT INTO instances (instance_id, instance_name, product_name) VALUES (100, 'device', 'fixture')",
        "INSERT INTO channels (channel_id, name, protocol, enabled) VALUES (10, 'old', 'virtual', 1)",
        "INSERT INTO telemetry_points (channel_id, point_id, signal_name) VALUES (10, 0, 'power')",
        "INSERT INTO adjustment_points (channel_id, point_id, signal_name, min_value, max_value, step) VALUES (10, 0, 'setpoint', 0.0, 100.0, 1.0)",
        "INSERT INTO measurement_routing (instance_id, instance_name, channel_id, channel_type, channel_point_id, measurement_id, enabled) VALUES (100, 'device', 10, 'T', 0, 5, 1)",
        "INSERT INTO action_routing (instance_id, instance_name, action_id, channel_id, channel_type, channel_point_id, enabled) VALUES (100, 'device', 1, 10, 'A', 0, 1)",
    ] {
        sqlx::query(statement)
            .execute(&pool)
            .await
            .expect("seed initial topology");
    }
    pool
}

fn sample(channel_id: u32, value: f64, timestamp_ms: u64) -> AcquiredPointSample {
    AcquiredPointSample::new(
        ChannelPointAddress::new(
            ChannelId::new(channel_id),
            PointKind::Telemetry,
            PointId::new(0),
        )
        .expect("physical address"),
        value,
        value,
        TimestampMs::new(timestamp_ms),
        PointQuality::Good,
    )
    .expect("finite sample")
}

#[tokio::test]
async fn partial_physical_publication_retains_the_previous_service_generation() {
    let pool = create_topology_pool().await;
    let directory = tempfile::tempdir().expect("temporary SHM directory");
    let point_path = directory.path().join("live.shm");
    let health_path = directory.path().join("health.shm");
    let first = aether_store_local::load_sqlite_live_topology(&pool)
        .await
        .expect("initial topology snapshot");
    let point_writer = ShmWriterHandle::create_published_at_epoch(
        ShmRuntimeConfig::new(&point_path, 256),
        Arc::new(first.point_manifest().clone()),
        None,
        100,
    )
    .expect("publish initial point plane");
    let health_writer = ShmChannelHealthWriterHandle::create_at_epoch(
        &health_path,
        Arc::new(first.health_manifest().clone()),
        100,
    )
    .expect("publish initial health plane");
    commit_topology_publication(&point_path, &health_path, 100).expect("commit initial topology");
    let first_timestamp = aether_shm_bridge::timestamp_ms();
    point_writer
        .generation()
        .expect("initial point generation")
        .acquisition_writer()
        .commit_batch(&[sample(10, 10.0, first_timestamp)])
        .expect("write initial point");
    health_writer
        .set_online(10, true, first_timestamp)
        .expect("write initial health");

    let command_sink = Arc::new(ShmDeviceCommandSink::new());
    let topology = AutomationTopologyHandle::new_lazy(
        point_path.clone(),
        health_path.clone(),
        first,
        Arc::clone(&command_sink),
    )
    .expect("compose lazy automation topology");
    assert!(
        topology
            .refresh(&pool)
            .await
            .expect("open initial topology")
    );

    let initial = topology.load();
    let initial_digest = initial.digest();
    assert_eq!(
        initial.read_instance_point(100, false, 5).unwrap(),
        Some((10.0, first_timestamp))
    );
    assert!(
        initial
            .channel_health(10)
            .expect("read health")
            .expect("health sample")
            .online()
    );
    let old_event = PointWatchEvent::new(10, PointKind::Telemetry, 0, 0, 10.0, 10.0, 1_000, 1);
    assert!(initial.accepts_point_watch_event(old_event));
    assert!(initial.accepts_ready_point_watch_event(old_event, initial.sequence()));
    assert!(
        !initial.accepts_ready_point_watch_event(old_event, initial.sequence().wrapping_add(1))
    );
    assert_eq!(
        initial
            .action_route(100, 1)
            .expect("initial command mapping")
            .channel_id()
            .get(),
        10
    );

    for statement in [
        "INSERT INTO channels (channel_id, name, protocol, enabled) VALUES (5, 'new', 'virtual', 1)",
        "INSERT INTO telemetry_points (channel_id, point_id, signal_name) VALUES (5, 0, 'power')",
        "INSERT INTO adjustment_points (channel_id, point_id, signal_name, min_value, max_value, step) VALUES (5, 0, 'setpoint', 0.0, 100.0, 1.0)",
        "UPDATE measurement_routing SET channel_id = 5 WHERE instance_id = 100 AND measurement_id = 5",
        "UPDATE action_routing SET channel_id = 5 WHERE instance_id = 100 AND action_id = 1",
    ] {
        sqlx::query(statement)
            .execute(&pool)
            .await
            .expect("mutate replacement topology");
    }
    let second = aether_store_local::load_sqlite_live_topology(&pool)
        .await
        .expect("replacement topology snapshot");
    point_writer
        .rebuild_for_publication(Arc::new(second.point_manifest().clone()), 101)
        .expect("publish only replacement point plane");

    let error = topology
        .refresh(&pool)
        .await
        .expect_err("mixed point/health publication must fail closed");
    assert!(error.is_retryable());
    assert_eq!(topology.load().digest(), initial_digest);
    assert!(topology.load().accepts_point_watch_event(old_event));

    health_writer
        .rebuild_for_publication(Arc::new(second.health_manifest().clone()), 101)
        .expect("publish replacement health plane");
    let second_timestamp = aether_shm_bridge::timestamp_ms();
    health_writer
        .set_online(5, false, second_timestamp)
        .expect("write replacement health");
    point_writer
        .generation()
        .expect("replacement point generation")
        .acquisition_writer()
        .commit_batch(&[sample(5, 50.0, second_timestamp)])
        .expect("write replacement point");
    commit_topology_publication(&point_path, &health_path, 101)
        .expect("commit replacement topology");

    assert!(
        topology
            .refresh(&pool)
            .await
            .expect("publish complete topology")
    );
    let current = topology.load();
    assert_ne!(current.digest(), initial_digest);
    assert_eq!(
        current.read_instance_point(100, false, 5).unwrap(),
        Some((50.0, second_timestamp))
    );
    assert!(
        !current
            .channel_health(5)
            .expect("read replacement health")
            .expect("replacement health sample")
            .online()
    );
    assert!(!current.accepts_point_watch_event(old_event));
    assert!(current.accepts_point_watch_event(PointWatchEvent::new(
        5,
        PointKind::Telemetry,
        0,
        0,
        50.0,
        50.0,
        2_000,
        2,
    )));
    assert_eq!(
        current
            .action_route(100, 1)
            .expect("replacement command mapping")
            .channel_id()
            .get(),
        5
    );
    assert_eq!(
        command_sink
            .manifest()
            .expect("command sink generation")
            .layout_hash(),
        current.point_manifest().layout_hash()
    );
}

#[tokio::test]
async fn topology_changes_notify_subscription_rebuilders_but_no_ops_do_not() {
    let pool = create_topology_pool().await;
    let directory = tempfile::tempdir().expect("temporary SHM directory");
    let point_path = directory.path().join("live.shm");
    let health_path = directory.path().join("health.shm");
    let snapshot = aether_store_local::load_sqlite_live_topology(&pool)
        .await
        .expect("initial topology snapshot");
    let _point_writer = ShmWriterHandle::create_published_at_epoch(
        ShmRuntimeConfig::new(&point_path, 256),
        Arc::new(snapshot.point_manifest().clone()),
        None,
        200,
    )
    .expect("publish point plane");
    let _health_writer = ShmChannelHealthWriterHandle::create_at_epoch(
        &health_path,
        Arc::new(snapshot.health_manifest().clone()),
        200,
    )
    .expect("publish health plane");
    commit_topology_publication(&point_path, &health_path, 200).expect("commit topology");
    let topology = Arc::new(
        AutomationTopologyHandle::new_lazy(
            point_path,
            health_path,
            snapshot,
            Arc::new(ShmDeviceCommandSink::new()),
        )
        .expect("compose topology"),
    );
    let mut changes = topology.subscribe();

    assert!(topology.refresh(&pool).await.expect("initial publication"));
    changes.changed().await.expect("initial publication signal");
    assert!(!topology.refresh(&pool).await.expect("unchanged refresh"));
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(25), changes.changed())
            .await
            .is_err()
    );

    let previously_ready = topology.load().sequence();
    let same_slot_event =
        PointWatchEvent::new(10, PointKind::Telemetry, 0, 0, 10.0, 10.0, 1_000, 9);
    // A command pins the old logical/physical generation across its awaits.
    // Publication must not expose replacement routing until that transaction
    // releases its service read lease.
    let pinned_command = Arc::clone(&topology).pin_command().await;
    assert!(
        pinned_command
            .generation()
            .measurement_route(100, 5)
            .is_some()
    );
    sqlx::query("UPDATE measurement_routing SET measurement_id = 6 WHERE instance_id = 100")
        .execute(&pool)
        .await
        .expect("mutate logical routing only");
    let refresh_topology = Arc::clone(&topology);
    let refresh_pool = pool.clone();
    let mut publication =
        tokio::spawn(async move { refresh_topology.refresh(&refresh_pool).await });
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(25), &mut publication)
            .await
            .is_err(),
        "topology publication must wait for the pinned command generation"
    );
    assert!(topology.load().measurement_route(100, 5).is_some());
    assert!(topology.load().measurement_route(100, 6).is_none());
    drop(pinned_command);
    assert!(
        publication
            .await
            .expect("publication task")
            .expect("route-only refresh")
    );
    changes.changed().await.expect("routing publication signal");
    let current = topology.load();
    assert!(current.accepts_point_watch_event(same_slot_event));
    assert!(!current.accepts_ready_point_watch_event(same_slot_event, previously_ready));
    assert!(current.measurement_route(100, 5).is_none());
    assert_eq!(
        current
            .measurement_route(100, 6)
            .expect("replacement logical route")
            .channel_id()
            .get(),
        10
    );

    // A transaction that rolls back must restore the exact pre-revocation
    // generation before reporting its NotFound result.
    let manager = Arc::new(InstanceManager::new(
        pool.clone(),
        Arc::new(ProductLoader::new(pool.clone())),
    ));
    manager
        .set_runtime_topology(Arc::clone(&topology))
        .expect("install runtime topology");
    let mutator = SqliteActionRoutingMutator::new(manager);
    let before_rollback = topology.load();
    let error = mutator
        .mutate_revisioned(RevisionedActionRoutingMutation::delete(
            ActionRouteKey::new(InstanceId::new(100), PointId::new(999)),
            LogicalRoutingRevision::new(1),
        ))
        .await
        .expect_err("missing action route");
    assert_eq!(error.kind(), PortErrorKind::NotFound);
    let restored = topology.load();
    assert!(Arc::ptr_eq(&restored, &before_rollback));
    assert!(restored.action_route(100, 1).is_some());
    changes
        .changed()
        .await
        .expect("rollback restoration signal");

    // Cancellation and early-return paths rely on the lease's Drop fallback.
    // It retains the refresh guard until the asynchronous restoration obtains
    // the command publication lock.
    let before_abandoned_mutation = topology.load();
    let abandoned_mutation = Arc::clone(&topology).begin_action_routing_mutation().await;
    assert!(topology.load().action_route(100, 1).is_none());
    drop(abandoned_mutation);
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        loop {
            if Arc::ptr_eq(&topology.load(), &before_abandoned_mutation) {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("abandoned mutation restoration");
    assert!(topology.load().action_route(100, 1).is_some());

    // A governed action-route mutation revokes commands before its SQLite
    // transaction. Periodic refresh is excluded by the retained lease until
    // the complete committed route is published.
    let action_mutation = Arc::clone(&topology).begin_action_routing_mutation().await;
    assert!(topology.load().action_route(100, 1).is_none());
    let revoked_command = Arc::clone(&topology).pin_command().await;
    assert!(revoked_command.generation().action_route(100, 1).is_none());
    drop(revoked_command);
    sqlx::query("UPDATE action_routing SET action_id = 2 WHERE instance_id = 100")
        .execute(&pool)
        .await
        .expect("mutate command routing");
    assert!(
        action_mutation
            .publish(&pool)
            .await
            .expect("publish command routing")
    );
    changes.changed().await.expect("command routing signal");
    assert!(topology.load().action_route(100, 1).is_none());
    assert!(topology.load().action_route(100, 2).is_some());

    // Once commit has started, a failed publication must remain fail-closed;
    // the lease must not restore the pre-commit route because SQLite is now the
    // durable authority and may contain the mutation.
    let committed_mutation = Arc::clone(&topology).begin_action_routing_mutation().await;
    assert!(topology.load().action_route(100, 2).is_none());
    sqlx::query("DELETE FROM adjustment_points WHERE channel_id = 10 AND point_id = 0")
        .execute(&pool)
        .await
        .expect("break committed command topology");
    let error = committed_mutation
        .publish(&pool)
        .await
        .expect_err("malformed committed topology must revoke commands");
    assert_eq!(error.kind(), aether_ports::PortErrorKind::InvalidData);
    changes.changed().await.expect("command revocation signal");
    let revoked = topology.load();
    assert!(revoked.action_route(100, 2).is_none());
    assert!(revoked.measurement_route(100, 6).is_some());
}
