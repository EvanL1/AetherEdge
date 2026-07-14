#![cfg(feature = "sqlite-topology")]

use aether_domain::PointKind;
use aether_ports::PortErrorKind;
use aether_store_local::load_sqlite_live_topology;
use sqlx::sqlite::SqlitePoolOptions;

async fn live_topology_pool() -> sqlx::SqlitePool {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .expect("in-memory live-topology database");
    for statement in [
        "CREATE TABLE channels (channel_id INTEGER PRIMARY KEY, protocol TEXT NOT NULL)",
        "CREATE TABLE telemetry_points (channel_id INTEGER, point_id INTEGER)",
        "CREATE TABLE signal_points (channel_id INTEGER, point_id INTEGER)",
        "CREATE TABLE control_points (channel_id INTEGER, point_id INTEGER)",
        "CREATE TABLE adjustment_points (channel_id INTEGER, point_id INTEGER)",
        "CREATE TABLE measurement_routing (instance_id INTEGER, channel_id INTEGER, channel_type TEXT, channel_point_id INTEGER, measurement_id INTEGER, enabled INTEGER)",
        "CREATE TABLE action_routing (instance_id INTEGER, channel_id INTEGER, channel_type TEXT, channel_point_id INTEGER, action_id INTEGER, enabled INTEGER)",
    ] {
        sqlx::query(statement)
            .execute(&pool)
            .await
            .expect("live-topology schema statement");
    }
    pool
}

async fn insert_channel_points(pool: &sqlx::SqlitePool) {
    sqlx::query("INSERT INTO channels (channel_id, protocol) VALUES (7, 'virtual')")
        .execute(pool)
        .await
        .expect("configured channel");
    for (table, max_point_id) in [
        ("telemetry_points", 2_i64),
        ("signal_points", 1),
        ("control_points", 0),
        ("adjustment_points", 3),
    ] {
        for point_id in 0..=max_point_id {
            sqlx::query(&format!(
                "INSERT INTO {table} (channel_id, point_id) VALUES (7, ?)"
            ))
            .bind(point_id)
            .execute(pool)
            .await
            .expect("configured physical point");
        }
    }
}

#[tokio::test]
async fn live_snapshot_contains_validated_point_health_and_logical_routes() {
    let pool = live_topology_pool().await;
    insert_channel_points(&pool).await;
    sqlx::query("INSERT INTO measurement_routing VALUES (10, 7, 'T', 2, 4, 1)")
        .execute(&pool)
        .await
        .expect("measurement route");
    sqlx::query("INSERT INTO action_routing VALUES (10, 7, 'A', 3, 5, 1)")
        .execute(&pool)
        .await
        .expect("action route");

    let snapshot = load_sqlite_live_topology(&pool)
        .await
        .expect("coherent live topology");

    let measurement = snapshot
        .measurement_route(10, 4)
        .expect("measurement route target");
    assert_eq!(measurement.channel_id().get(), 7);
    assert_eq!(measurement.kind(), PointKind::Telemetry);
    assert_eq!(measurement.point_id().get(), 2);
    let action = snapshot.action_route(10, 5).expect("action route target");
    assert_eq!(action.kind(), PointKind::Action);
    assert_eq!(
        snapshot.health_manifest().channel_ids().collect::<Vec<_>>(),
        vec![7]
    );
    assert_ne!(snapshot.digest(), 0);
}

#[tokio::test]
async fn live_snapshot_exposes_exact_ordered_configured_physical_points() {
    let pool = live_topology_pool().await;
    sqlx::query("INSERT INTO channels (channel_id, protocol) VALUES (7, 'virtual')")
        .execute(&pool)
        .await
        .expect("virtual channel");
    for (table, point_ids) in [
        ("telemetry_points", &[2_i64, 0][..]),
        ("signal_points", &[3][..]),
        ("control_points", &[4, 1][..]),
        ("adjustment_points", &[2][..]),
    ] {
        for point_id in point_ids {
            sqlx::query(&format!(
                "INSERT INTO {table} (channel_id, point_id) VALUES (7, ?)"
            ))
            .bind(point_id)
            .execute(&pool)
            .await
            .expect("sparse configured physical point");
        }
    }

    let snapshot = load_sqlite_live_topology(&pool)
        .await
        .expect("live topology with sparse virtual points");
    let configured = snapshot
        .configured_physical_points()
        .iter()
        .map(|address| {
            (
                address.channel_id().get(),
                address.kind(),
                address.point_id().get(),
            )
        })
        .collect::<Vec<_>>();

    assert_eq!(
        configured,
        vec![
            (7, PointKind::Telemetry, 0),
            (7, PointKind::Telemetry, 2),
            (7, PointKind::Status, 3),
            (7, PointKind::Command, 1),
            (7, PointKind::Command, 4),
            (7, PointKind::Action, 2),
        ]
    );
}

#[tokio::test]
async fn enabled_but_fully_unbound_routes_are_not_physical_routes() {
    let pool = live_topology_pool().await;
    insert_channel_points(&pool).await;
    sqlx::query("INSERT INTO measurement_routing VALUES (10, NULL, NULL, NULL, 4, 1)")
        .execute(&pool)
        .await
        .expect("unbound measurement route");
    sqlx::query("INSERT INTO action_routing VALUES (10, NULL, NULL, NULL, 5, 1)")
        .execute(&pool)
        .await
        .expect("unbound action route");

    let snapshot = load_sqlite_live_topology(&pool)
        .await
        .expect("fully unbound rows are valid configuration placeholders");

    assert!(snapshot.measurement_route(10, 4).is_none());
    assert!(snapshot.action_route(10, 5).is_none());
}

#[tokio::test]
async fn partially_bound_routes_fail_closed_as_invalid_topology() {
    let pool = live_topology_pool().await;
    insert_channel_points(&pool).await;
    sqlx::query("INSERT INTO measurement_routing VALUES (10, 7, NULL, 2, 4, 1)")
        .execute(&pool)
        .await
        .expect("partially bound route row");

    let error = load_sqlite_live_topology(&pool)
        .await
        .expect_err("partial physical bindings must not be silently ignored");

    assert_eq!(error.kind(), PortErrorKind::InvalidData);
}

#[tokio::test]
async fn route_target_missing_from_the_same_manifest_fails_closed() {
    let pool = live_topology_pool().await;
    insert_channel_points(&pool).await;
    sqlx::query("INSERT INTO measurement_routing VALUES (10, 7, 'T', 99, 4, 1)")
        .execute(&pool)
        .await
        .expect("orphan route row");

    let error = load_sqlite_live_topology(&pool)
        .await
        .expect_err("orphan physical target must be rejected");

    assert_eq!(error.kind(), PortErrorKind::InvalidData);
}

#[tokio::test]
async fn route_target_in_a_sparse_manifest_hole_fails_closed() {
    let pool = live_topology_pool().await;
    sqlx::query("INSERT INTO channels (channel_id, protocol) VALUES (7, 'virtual')")
        .execute(&pool)
        .await
        .expect("configured channel");
    for point_id in [0_i64, 2] {
        sqlx::query("INSERT INTO telemetry_points (channel_id, point_id) VALUES (7, ?)")
            .bind(point_id)
            .execute(&pool)
            .await
            .expect("sparse configured point");
    }
    sqlx::query("INSERT INTO measurement_routing VALUES (10, 7, 'T', 1, 4, 1)")
        .execute(&pool)
        .await
        .expect("route into sparse hole");

    let error = load_sqlite_live_topology(&pool)
        .await
        .expect_err("a route must target an actually configured physical point");

    assert_eq!(error.kind(), PortErrorKind::InvalidData);
}

#[tokio::test]
async fn point_topology_without_a_configured_health_channel_fails_closed() {
    let pool = live_topology_pool().await;
    sqlx::query("INSERT INTO telemetry_points (channel_id, point_id) VALUES (7, 0)")
        .execute(&pool)
        .await
        .expect("orphan physical point");

    let error = load_sqlite_live_topology(&pool)
        .await
        .expect_err("every point-owning channel must exist in the health topology");

    assert_eq!(error.kind(), PortErrorKind::InvalidData);
}

#[tokio::test]
async fn duplicate_logical_address_fails_instead_of_overwriting_by_query_order() {
    let pool = live_topology_pool().await;
    insert_channel_points(&pool).await;
    for point_id in [1_i64, 2] {
        sqlx::query("INSERT INTO measurement_routing VALUES (10, 7, 'T', ?, 4, 1)")
            .bind(point_id)
            .execute(&pool)
            .await
            .expect("duplicate logical route row");
    }

    let error = load_sqlite_live_topology(&pool)
        .await
        .expect_err("duplicate logical route must be rejected");

    assert_eq!(error.kind(), PortErrorKind::InvalidData);
}

#[tokio::test]
async fn routing_only_change_advances_the_deterministic_digest() {
    let pool = live_topology_pool().await;
    insert_channel_points(&pool).await;
    sqlx::query("INSERT INTO measurement_routing VALUES (10, 7, 'T', 1, 4, 1)")
        .execute(&pool)
        .await
        .expect("initial route");
    let first = load_sqlite_live_topology(&pool)
        .await
        .expect("first live topology");

    sqlx::query(
        "UPDATE measurement_routing SET channel_point_id = 2 WHERE instance_id = 10 AND measurement_id = 4",
    )
    .execute(&pool)
    .await
    .expect("move route without changing SHM layout");
    let second = load_sqlite_live_topology(&pool)
        .await
        .expect("second live topology");

    assert_ne!(first.digest(), second.digest());
    assert_eq!(
        first.point_manifest().layout_hash(),
        second.point_manifest().layout_hash()
    );
}

#[tokio::test]
async fn exact_configured_point_change_advances_digest_when_layout_is_unchanged() {
    let pool = live_topology_pool().await;
    sqlx::query("INSERT INTO channels (channel_id, protocol) VALUES (7, 'virtual')")
        .execute(&pool)
        .await
        .expect("virtual channel");
    for point_id in [0_i64, 1, 3] {
        sqlx::query("INSERT INTO telemetry_points (channel_id, point_id) VALUES (7, ?)")
            .bind(point_id)
            .execute(&pool)
            .await
            .expect("initial configured point");
    }
    let first = load_sqlite_live_topology(&pool)
        .await
        .expect("first live topology");

    sqlx::query("DELETE FROM telemetry_points WHERE channel_id = 7 AND point_id = 0")
        .execute(&pool)
        .await
        .expect("remove low configured point");
    sqlx::query("INSERT INTO telemetry_points (channel_id, point_id) VALUES (7, 2)")
        .execute(&pool)
        .await
        .expect("replace low configured point");
    let second = load_sqlite_live_topology(&pool)
        .await
        .expect("second live topology");

    assert_eq!(
        first.point_manifest().layout_hash(),
        second.point_manifest().layout_hash()
    );
    assert_eq!(
        first.point_manifest().point_count(),
        second.point_manifest().point_count()
    );
    assert_ne!(first.digest(), second.digest());
}

#[tokio::test]
async fn unchanged_snapshot_preserves_the_deterministic_digest() {
    let pool = live_topology_pool().await;
    insert_channel_points(&pool).await;
    sqlx::query("INSERT INTO measurement_routing VALUES (10, 7, 'S', 1, 4, 1)")
        .execute(&pool)
        .await
        .expect("measurement route");
    sqlx::query("INSERT INTO action_routing VALUES (10, 7, 'C', 0, 5, 1)")
        .execute(&pool)
        .await
        .expect("action route");

    let first = load_sqlite_live_topology(&pool)
        .await
        .expect("first live topology");
    let second = load_sqlite_live_topology(&pool)
        .await
        .expect("unchanged live topology");

    assert_eq!(first.digest(), second.digest());
}
