use aether_domain::PointKind;
use aether_ports::PortErrorKind;
use aether_shm_bridge::{ChannelPointManifest, PhysicalPointAddress};
use aether_sqlite_topology::{load_sqlite_shm_capacity, load_sqlite_shm_topology};
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
async fn snapshot_includes_sparse_measurements_and_all_channel_health() {
    let pool = topology_pool().await;
    for (channel_id, protocol) in [(7_i64, "modbus_tcp"), (20, "modbus-tcp")] {
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

    let expected_addresses = [
        PhysicalPointAddress::from_raw_ids(7, PointKind::Telemetry, 2),
        PhysicalPointAddress::from_raw_ids(7, PointKind::Status, 1),
        PhysicalPointAddress::from_raw_ids(7, PointKind::Command, 0),
        PhysicalPointAddress::from_raw_ids(7, PointKind::Action, 3),
    ];
    let expected_points = ChannelPointManifest::compile(expected_addresses, 4)
        .expect("expected exact point manifest");
    assert_eq!(
        snapshot.point_manifest().layout_hash(),
        expected_points.layout_hash()
    );
    assert_eq!(snapshot.point_manifest().slot_count(), 4);
    assert_eq!(
        snapshot
            .point_manifest()
            .iter_physical_points()
            .map(|(_, address)| address)
            .collect::<Vec<_>>(),
        expected_points
            .iter_physical_points()
            .map(|(_, address)| address)
            .collect::<Vec<_>>()
    );
    assert_eq!(
        snapshot.health_manifest().channel_ids().collect::<Vec<_>>(),
        vec![7, 20]
    );
}

#[tokio::test]
async fn snapshot_rejects_negative_stored_identifiers() {
    let pool = topology_pool().await;
    sqlx::query("INSERT INTO channels (channel_id, protocol) VALUES (-1, 'modbus_tcp')")
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
async fn snapshot_rejects_a_negative_point_hidden_below_a_valid_maximum() {
    let pool = topology_pool().await;
    sqlx::query("INSERT INTO channels (channel_id, protocol) VALUES (1, 'modbus-tcp')")
        .execute(&pool)
        .await
        .expect("configured channel");
    for point_id in [-2_i64, 2] {
        sqlx::query("INSERT INTO telemetry_points (channel_id, point_id) VALUES (1, ?)")
            .bind(point_id)
            .execute(&pool)
            .await
            .expect("mixed point range");
    }

    let error = load_sqlite_shm_topology(&pool)
        .await
        .expect_err("a valid maximum must not hide a negative point id");

    assert_eq!(error.kind(), PortErrorKind::InvalidData);
}

#[tokio::test]
async fn snapshot_compacts_sparse_nonzero_point_identifiers_without_creating_holes() {
    let pool = topology_pool().await;
    sqlx::query("CREATE TABLE service_config (service_name TEXT, key TEXT, value TEXT)")
        .execute(&pool)
        .await
        .expect("service config schema");
    sqlx::query(
        "INSERT INTO service_config (service_name, key, value) \
         VALUES ('global', 'shared_memory.max_slots', '3')",
    )
    .execute(&pool)
    .await
    .expect("configured capacity");
    sqlx::query("INSERT INTO channels (channel_id, protocol) VALUES (1, 'modbus-tcp')")
        .execute(&pool)
        .await
        .expect("configured channel");
    for point_id in [2_i64, 100] {
        sqlx::query("INSERT INTO telemetry_points (channel_id, point_id) VALUES (1, ?)")
            .bind(point_id)
            .execute(&pool)
            .await
            .expect("sparse configured point");
    }

    let snapshot = load_sqlite_shm_topology(&pool)
        .await
        .expect("sparse point ids compile into an exact physical topology");
    let point_watch_capacity = load_sqlite_shm_capacity(&pool)
        .await
        .expect("shared PointWatch capacity");

    assert_eq!(snapshot.max_slots(), 3);
    assert_eq!(point_watch_capacity, 3);
    assert_eq!(snapshot.point_manifest().slot_count(), 2);
    assert_eq!(
        snapshot
            .point_manifest()
            .point_ids(1, PointKind::Telemetry)
            .collect::<Vec<_>>(),
        vec![2, 100]
    );
    assert!(
        snapshot
            .point_manifest()
            .slot_for(PhysicalPointAddress::from_raw_ids(
                1,
                PointKind::Telemetry,
                3,
            ))
            .is_none()
    );
}

#[tokio::test]
async fn snapshot_rejects_capacity_overflow_before_publication() {
    let pool = topology_pool().await;
    sqlx::query("CREATE TABLE service_config (service_name TEXT, key TEXT, value TEXT)")
        .execute(&pool)
        .await
        .expect("service config schema");
    sqlx::query(
        "INSERT INTO service_config (service_name, key, value) \
         VALUES ('global', 'shared_memory.max_slots', '1')",
    )
    .execute(&pool)
    .await
    .expect("configured capacity");
    sqlx::query("INSERT INTO channels (channel_id, protocol) VALUES (1, 'modbus-tcp')")
        .execute(&pool)
        .await
        .expect("configured channel");
    for point_id in [2_i64, 100] {
        sqlx::query("INSERT INTO telemetry_points (channel_id, point_id) VALUES (1, ?)")
            .bind(point_id)
            .execute(&pool)
            .await
            .expect("configured point");
    }

    let error = load_sqlite_shm_topology(&pool)
        .await
        .expect_err("configured capacity must fail closed");

    assert_eq!(error.kind(), PortErrorKind::InvalidData);
}

#[tokio::test]
async fn snapshot_rejects_duplicate_point_identifiers() {
    let pool = topology_pool().await;
    sqlx::query("INSERT INTO channels (channel_id, protocol) VALUES (1, 'modbus-tcp')")
        .execute(&pool)
        .await
        .expect("configured channel");
    for _ in 0..2 {
        sqlx::query("INSERT INTO telemetry_points (channel_id, point_id) VALUES (1, 7)")
            .execute(&pool)
            .await
            .expect("duplicate point row");
    }

    let error = load_sqlite_shm_topology(&pool)
        .await
        .expect_err("duplicate point identities must not collapse into one SHM slot");

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
