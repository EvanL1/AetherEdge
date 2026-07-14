use std::sync::Arc;

use aether_domain::{
    AcquiredPointSample, ChannelId, ChannelPointAddress, PointId, PointKind, PointQuality,
    TimestampMs,
};
use aether_io::store::SqliteShmTopologyProjector;
use aether_shm_bridge::{
    ChannelHealthManifest, PhysicalPointAddress, ShmChannelHealthReader,
    ShmChannelHealthWriterHandle, ShmClientConfig, ShmRuntimeConfig, ShmWriterHandle,
    commit_topology_publication,
};
use aether_store_local::load_sqlite_shm_topology;
use sqlx::sqlite::SqlitePoolOptions;

async fn pool() -> sqlx::SqlitePool {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .expect("in-memory SQLite");
    common::test_utils::schema::init_io_schema(&pool)
        .await
        .expect("I/O schema");
    pool
}

async fn load_channel_point_manifest(
    pool: &sqlx::SqlitePool,
) -> anyhow::Result<aether_shm_bridge::ChannelPointManifest> {
    Ok(load_sqlite_shm_topology(pool)
        .await?
        .point_manifest()
        .clone())
}

async fn insert_channel(pool: &sqlx::SqlitePool, channel_id: i64) {
    sqlx::query(
        "INSERT INTO channels (channel_id, name, protocol, enabled, config, revision) \
         VALUES (?, ?, 'virtual', 1, '{}', 1)",
    )
    .bind(channel_id)
    .bind(format!("channel-{channel_id}"))
    .execute(pool)
    .await
    .expect("channel");
}

async fn insert_telemetry(pool: &sqlx::SqlitePool, channel_id: i64, point_id: i64) {
    sqlx::query(
        "INSERT INTO telemetry_points \
         (channel_id, point_id, signal_name, data_type) VALUES (?, ?, ?, 'f64')",
    )
    .bind(channel_id)
    .bind(point_id)
    .bind(format!("t-{channel_id}-{point_id}"))
    .execute(pool)
    .await
    .expect("telemetry point");
}

fn sample(channel_id: u32, point_id: u32, value: f64) -> AcquiredPointSample {
    let address = ChannelPointAddress::new(
        ChannelId::new(channel_id),
        PointKind::Telemetry,
        PointId::new(point_id),
    )
    .expect("acquisition address");
    AcquiredPointSample::new(
        address,
        value,
        value,
        TimestampMs::new(100),
        PointQuality::Good,
    )
    .expect("finite sample")
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock after epoch")
        .as_millis() as u64
}

#[tokio::test]
async fn io_manifest_hash_matches_canonical_snapshot_for_virtual_measurements() {
    let pool = pool().await;
    insert_channel(&pool, 7).await;
    insert_telemetry(&pool, 7, 2).await;
    sqlx::query(
        "INSERT INTO signal_points \
         (channel_id, point_id, signal_name) VALUES (7, 1, 'virtual-status')",
    )
    .execute(&pool)
    .await
    .expect("virtual signal point");

    let io_manifest = load_channel_point_manifest(&pool)
        .await
        .expect("io compatibility manifest");
    let canonical = load_sqlite_shm_topology(&pool)
        .await
        .expect("canonical topology snapshot");

    assert_eq!(
        io_manifest.layout_hash(),
        canonical.point_manifest().layout_hash()
    );
    assert_eq!(io_manifest.counts().get(&7), Some(&[3, 2, 0, 0]));
    assert_eq!(
        canonical
            .health_manifest()
            .channel_ids()
            .collect::<Vec<_>>(),
        vec![7]
    );
}

#[tokio::test]
async fn unchanged_topology_is_a_noop_and_preserves_live_values() {
    let pool = pool().await;
    insert_channel(&pool, 1).await;
    insert_telemetry(&pool, 1, 0).await;
    let dir = tempfile::tempdir().expect("temporary SHM directory");
    let point_manifest = Arc::new(
        load_channel_point_manifest(&pool)
            .await
            .expect("point manifest"),
    );
    let points = Arc::new(
        ShmWriterHandle::create_published_at_epoch(
            ShmRuntimeConfig::new(dir.path().join("points.shm"), 64),
            Arc::clone(&point_manifest),
            None,
            10,
        )
        .expect("point writer"),
    );
    let health_manifest = Arc::new(ChannelHealthManifest::from_channel_ids([1]));
    let health_path = dir.path().join("health.shm");
    let health = Arc::new(
        ShmChannelHealthWriterHandle::create_at_epoch(
            &health_path,
            Arc::clone(&health_manifest),
            10,
        )
        .expect("health writer"),
    );
    commit_topology_publication(points.config().path(), &health_path, 10)
        .expect("commit initial topology");
    points
        .generation()
        .expect("point generation")
        .acquisition_writer()
        .commit_batch(&[sample(1, 0, 12.5)])
        .expect("point value");
    let health_timestamp = now_ms();
    health
        .set_online(1, true, health_timestamp)
        .expect("health value");
    let point_generation = points.generation().expect("point generation").generation();
    let health_generation = health.generation().expect("health generation");
    let projector = SqliteShmTopologyProjector::new(pool, Arc::clone(&points), Arc::clone(&health));

    let receipt = projector.project().await.expect("unchanged projection");

    assert!(receipt.is_current());
    assert!(!receipt.changed());
    assert_eq!(receipt.live_state_generation(), Some(point_generation));
    assert_eq!(receipt.channel_health_generation(), Some(health_generation));
    assert_eq!(receipt.publication_epoch(), Some(10));
    assert_eq!(
        points
            .generation()
            .expect("same point generation")
            .read_slot(
                point_manifest
                    .slot_for(PhysicalPointAddress::from_legacy_raw(
                        1,
                        PointKind::Telemetry,
                        0,
                    ))
                    .expect("slot")
            )
            .expect("point slot")
            .value,
        12.5
    );
    let reader = ShmChannelHealthReader::new(
        ShmClientConfig::new(&health_path, health_manifest.layout_hash()),
        health_manifest,
    );
    assert!(
        reader
            .read_channel(1)
            .expect("health read")
            .expect("health sample")
            .online()
    );
}

#[tokio::test]
async fn topology_change_publishes_both_planes_and_preserves_only_health_intersection() {
    let pool = pool().await;
    insert_channel(&pool, 1).await;
    insert_telemetry(&pool, 1, 0).await;
    let dir = tempfile::tempdir().expect("temporary SHM directory");
    let initial_points = Arc::new(load_channel_point_manifest(&pool).await.expect("manifest"));
    let points = Arc::new(
        ShmWriterHandle::create_published_at_epoch(
            ShmRuntimeConfig::new(dir.path().join("points.shm"), 64),
            Arc::clone(&initial_points),
            None,
            20,
        )
        .expect("point writer"),
    );
    points
        .generation()
        .expect("point generation")
        .acquisition_writer()
        .commit_batch(&[sample(1, 0, 8.0)])
        .expect("old point value");
    let initial_health = Arc::new(ChannelHealthManifest::from_channel_ids([1]));
    let health_path = dir.path().join("health.shm");
    let health = Arc::new(
        ShmChannelHealthWriterHandle::create_at_epoch(&health_path, initial_health, 20)
            .expect("health writer"),
    );
    commit_topology_publication(points.config().path(), &health_path, 20)
        .expect("commit initial topology");
    let health_timestamp = now_ms();
    health
        .set_online(1, true, health_timestamp)
        .expect("old health value");
    let old_point_generation = points.generation().expect("point generation").generation();
    let old_health_generation = health.generation().expect("health generation");
    let projector =
        SqliteShmTopologyProjector::new(pool.clone(), Arc::clone(&points), Arc::clone(&health));
    insert_telemetry(&pool, 1, 1).await;
    insert_channel(&pool, 2).await;

    let receipt = projector.project().await.expect("changed projection");

    assert!(receipt.is_current());
    assert!(receipt.changed());
    assert_ne!(receipt.live_state_generation(), Some(old_point_generation));
    assert_ne!(
        receipt.channel_health_generation(),
        Some(old_health_generation)
    );
    assert!(receipt.publication_epoch().is_some_and(|epoch| epoch != 20));
    let current_points = load_channel_point_manifest(&pool)
        .await
        .expect("current manifest");
    let current_generation = points.generation().expect("current point generation");
    let old_slot = current_points
        .slot_for(PhysicalPointAddress::from_legacy_raw(
            1,
            PointKind::Telemetry,
            0,
        ))
        .expect("old address remains allocated");
    assert!(
        current_generation
            .read_slot(old_slot)
            .expect("fresh old-address slot")
            .value
            .is_nan(),
        "business point values must not cross topology generations"
    );
    current_generation
        .acquisition_writer()
        .commit_batch(&[sample(1, 1, 9.0)])
        .expect("new point is writable");

    let current_health = Arc::new(ChannelHealthManifest::from_channel_ids([1, 2]));
    let health_reader = ShmChannelHealthReader::new(
        ShmClientConfig::new(&health_path, current_health.layout_hash()),
        current_health,
    );
    let retained = health_reader
        .read_channel(1)
        .expect("retained health read")
        .expect("retained health sample");
    assert!(retained.online());
    assert_eq!(retained.timestamp_ms(), health_timestamp);
    health
        .set_online(2, false, health_timestamp + 1)
        .expect("new channel health");
}

#[tokio::test]
async fn point_capacity_preflight_leaves_both_current_generations_untouched() {
    let pool = pool().await;
    insert_channel(&pool, 1).await;
    insert_telemetry(&pool, 1, 0).await;
    let dir = tempfile::tempdir().expect("temporary SHM directory");
    let manifest = Arc::new(load_channel_point_manifest(&pool).await.expect("manifest"));
    let points = Arc::new(
        ShmWriterHandle::create_published_at_epoch(
            ShmRuntimeConfig::new(dir.path().join("points.shm"), 4),
            manifest,
            None,
            30,
        )
        .expect("point writer"),
    );
    let health = Arc::new(
        ShmChannelHealthWriterHandle::create_at_epoch(
            dir.path().join("health.shm"),
            Arc::new(ChannelHealthManifest::from_channel_ids([1])),
            30,
        )
        .expect("health writer"),
    );
    commit_topology_publication(points.config().path(), health.path(), 30)
        .expect("commit initial topology");
    let point_generation = points.generation().expect("point generation").generation();
    let health_generation = health.generation().expect("health generation");
    let projector =
        SqliteShmTopologyProjector::new(pool.clone(), Arc::clone(&points), Arc::clone(&health));
    insert_telemetry(&pool, 1, 4).await;

    let error = projector
        .project()
        .await
        .expect_err("capacity must fail before either plane changes");

    assert_eq!(error.kind(), aether_ports::PortErrorKind::InvalidData);
    assert_eq!(
        points.generation().expect("point generation").generation(),
        point_generation
    );
    assert_eq!(health.generation(), Some(health_generation));
}

#[tokio::test]
async fn uncoordinated_matching_planes_are_republished_and_committed() {
    let pool = pool().await;
    insert_channel(&pool, 1).await;
    insert_telemetry(&pool, 1, 0).await;
    let directory = tempfile::tempdir().expect("temporary SHM directory");
    let point_manifest = Arc::new(load_channel_point_manifest(&pool).await.expect("manifest"));
    let point_path = directory.path().join("points.shm");
    let health_path = directory.path().join("health.shm");
    let points = Arc::new(
        ShmWriterHandle::create_published(
            ShmRuntimeConfig::new(&point_path, 64),
            point_manifest,
            None,
        )
        .expect("legacy point writer"),
    );
    let health = Arc::new(
        ShmChannelHealthWriterHandle::create(
            &health_path,
            Arc::new(ChannelHealthManifest::from_channel_ids([1])),
        )
        .expect("legacy health writer"),
    );
    let old_point_generation = points.generation().expect("point generation").generation();
    let old_health_generation = health.generation().expect("health generation");
    let projector = SqliteShmTopologyProjector::new(pool, Arc::clone(&points), Arc::clone(&health));

    let receipt = projector
        .project()
        .await
        .expect("repair physical publication");

    assert!(receipt.is_current());
    assert!(receipt.changed());
    assert!(receipt.publication_epoch().is_some_and(|epoch| epoch != 0));
    assert_ne!(receipt.live_state_generation(), Some(old_point_generation));
    assert_ne!(
        receipt.channel_health_generation(),
        Some(old_health_generation)
    );
}
