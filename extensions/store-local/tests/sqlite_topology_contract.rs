#![cfg(feature = "sqlite-topology")]

use std::collections::BTreeMap;

use aether_ports::PortErrorKind;
use aether_shm_bridge::ChannelPointManifest;
use aether_store_local::load_sqlite_shm_topology;
use sqlx::sqlite::SqlitePoolOptions;

async fn topology_pool() -> sqlx::SqlitePool {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .expect("in-memory topology database");
    for statement in [
        "CREATE TABLE channels (channel_id INTEGER PRIMARY KEY, protocol TEXT NOT NULL)",
        "CREATE TABLE telemetry_points (channel_id INTEGER, point_id INTEGER)",
        "CREATE TABLE signal_points (channel_id INTEGER, point_id INTEGER)",
        "CREATE TABLE control_points (channel_id INTEGER, point_id INTEGER)",
        "CREATE TABLE adjustment_points (channel_id INTEGER, point_id INTEGER)",
    ] {
        sqlx::query(statement)
            .execute(&pool)
            .await
            .expect("topology schema statement");
    }
    pool
}

#[tokio::test]
async fn snapshot_includes_virtual_measurements_and_all_channel_health() {
    let pool = topology_pool().await;
    for (channel_id, protocol) in [(7_i64, "virtual"), (20, "modbus-tcp")] {
        sqlx::query("INSERT INTO channels (channel_id, protocol) VALUES (?, ?)")
            .bind(channel_id)
            .bind(protocol)
            .execute(&pool)
            .await
            .expect("configured channel");
    }
    for (table, point_id) in [
        ("telemetry_points", 2_i64),
        ("signal_points", 1),
        ("control_points", 0),
        ("adjustment_points", 3),
    ] {
        sqlx::query(&format!(
            "INSERT INTO {table} (channel_id, point_id) VALUES (7, ?)"
        ))
        .bind(point_id)
        .execute(&pool)
        .await
        .expect("configured point");
    }

    let snapshot = load_sqlite_shm_topology(&pool)
        .await
        .expect("canonical topology snapshot");

    let expected_points = ChannelPointManifest::from_map(BTreeMap::from([(7, [3, 2, 1, 4])]));
    assert_eq!(
        snapshot.point_manifest().layout_hash(),
        expected_points.layout_hash()
    );
    assert_eq!(snapshot.point_manifest().counts(), expected_points.counts());
    assert_eq!(
        snapshot.health_manifest().channel_ids().collect::<Vec<_>>(),
        vec![7, 20]
    );
}

#[tokio::test]
async fn snapshot_rejects_negative_stored_identifiers() {
    let pool = topology_pool().await;
    sqlx::query("INSERT INTO channels (channel_id, protocol) VALUES (-1, 'virtual')")
        .execute(&pool)
        .await
        .expect("malformed channel row");

    let error = load_sqlite_shm_topology(&pool)
        .await
        .expect_err("negative channel identity must be rejected");

    assert_eq!(error.kind(), PortErrorKind::InvalidData);
}

#[tokio::test]
async fn snapshot_rejects_negative_point_ranges() {
    let pool = topology_pool().await;
    sqlx::query("INSERT INTO channels (channel_id, protocol) VALUES (1, 'modbus-tcp')")
        .execute(&pool)
        .await
        .expect("configured channel");
    sqlx::query("INSERT INTO telemetry_points (channel_id, point_id) VALUES (1, -2)")
        .execute(&pool)
        .await
        .expect("malformed point row");

    let error = load_sqlite_shm_topology(&pool)
        .await
        .expect_err("negative point ranges must be rejected");

    assert_eq!(error.kind(), PortErrorKind::InvalidData);
}

#[tokio::test]
async fn snapshot_reports_an_unavailable_authoritative_schema() {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .expect("in-memory database without topology schema");

    let error = load_sqlite_shm_topology(&pool)
        .await
        .expect_err("missing authoritative schema must fail closed");

    assert_eq!(error.kind(), PortErrorKind::Unavailable);
}
