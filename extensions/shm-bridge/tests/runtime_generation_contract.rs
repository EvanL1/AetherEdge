use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use aether_domain::{
    AcquiredPointSample, ChannelId, ChannelPointAddress, PointId, PointKind, PointQuality,
    TimestampMs,
};
use aether_shm_bridge::{
    ChannelPointManifest, ShmChannelReader, ShmRuntimeConfig, ShmWriterHandle,
};

fn manifest(channel_id: u32) -> Arc<ChannelPointManifest> {
    Arc::new(ChannelPointManifest::from_map(BTreeMap::from([(
        channel_id,
        [1, 1, 1, 1],
    )])))
}

fn sample(channel_id: u32, value: f64, timestamp_ms: u64) -> AcquiredPointSample {
    let address = ChannelPointAddress::new(
        ChannelId::new(channel_id),
        PointKind::Telemetry,
        PointId::new(0),
    )
    .expect("telemetry is acquisition-owned");
    AcquiredPointSample::new(
        address,
        value,
        value * 10.0,
        TimestampMs::new(timestamp_ms),
        PointQuality::Good,
    )
    .expect("finite sample")
}

#[test]
fn published_generation_is_readable_through_the_typed_channel_adapter() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let path = directory.path().join("aether.shm");
    let active_manifest = manifest(17);
    let handle = ShmWriterHandle::create_published(
        ShmRuntimeConfig::new(&path, 64),
        Arc::clone(&active_manifest),
        None,
    )
    .expect("publish writer generation");

    let generation = handle.generation().expect("current generation");
    generation
        .acquisition_writer()
        .commit_batch(&[sample(17, 12.5, 100)])
        .expect("commit typed acquisition sample");

    let reader = ShmChannelReader::open(&path, active_manifest).expect("open typed channel reader");
    let value = reader
        .read_channel(17, PointKind::Telemetry, 0)
        .expect("read slot")
        .expect("written value");
    assert_eq!(value.value(), 12.5);
    assert_eq!(value.raw(), 125.0);
    assert_eq!(value.timestamp_ms(), 100);
}

#[test]
fn canonical_rebuild_invalidates_retained_writers_and_publishes_one_coherent_generation() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let path = directory.path().join("aether.shm");
    let handle =
        ShmWriterHandle::create_published(ShmRuntimeConfig::new(&path, 64), manifest(17), None)
            .expect("publish initial generation");
    let stale = handle.generation().expect("initial generation");

    handle.rebuild(manifest(23)).expect("rebuild generation");

    let stale_error = stale
        .acquisition_writer()
        .commit_batch(&[sample(17, 1.0, 200)])
        .expect_err("retained writer must not mutate the replaced inode");
    assert!(stale_error.is_retryable());

    let current = handle.generation().expect("replacement generation");
    assert_eq!(
        current
            .manifest()
            .counts()
            .keys()
            .copied()
            .collect::<Vec<_>>(),
        vec![23]
    );
    current
        .acquisition_writer()
        .commit_batch(&[sample(23, 7.0, 201)])
        .expect("replacement writer accepts its own manifest");
    assert_ne!(stale.generation(), current.generation());
}

#[test]
fn retained_generation_cannot_overwrite_snapshot_after_rebuild() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let canonical = directory.path().join("aether.shm");
    let snapshot = directory.path().join("aether.snapshot");
    let active_manifest = manifest(17);
    let handle = ShmWriterHandle::create_published(
        ShmRuntimeConfig::new(&canonical, 64),
        Arc::clone(&active_manifest),
        None,
    )
    .expect("publish initial generation");
    let stale = handle.generation().expect("initial generation");

    handle
        .rebuild(Arc::clone(&active_manifest))
        .expect("rebuild generation");
    handle
        .generation()
        .expect("current generation")
        .save_snapshot(&snapshot)
        .expect("save current snapshot");

    let error = stale
        .save_snapshot(&snapshot)
        .expect_err("retained generation must not replace the current snapshot");
    assert!(error.is_retryable());
}

#[test]
fn exact_manifest_snapshot_restores_without_relaxing_layout_identity() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let canonical = directory.path().join("aether.shm");
    let snapshot = directory.path().join("aether.snapshot");
    let active_manifest = manifest(17);
    let first = ShmWriterHandle::create_published(
        ShmRuntimeConfig::new(&canonical, 64),
        Arc::clone(&active_manifest),
        None,
    )
    .expect("publish initial generation");
    let generation = first.generation().expect("initial generation");
    generation
        .acquisition_writer()
        .commit_batch(&[sample(17, 3.25, 300)])
        .expect("write snapshot source");
    generation.save_snapshot(&snapshot).expect("save snapshot");
    drop(first);

    let restored = ShmWriterHandle::create_published(
        ShmRuntimeConfig::new(&canonical, 64),
        Arc::clone(&active_manifest),
        Some(&snapshot),
    )
    .expect("restore exact manifest snapshot");
    let reader = ShmChannelReader::open(&canonical, active_manifest).expect("open restored reader");
    assert_eq!(
        reader
            .read_channel(17, PointKind::Telemetry, 0)
            .expect("read restored slot")
            .expect("restored value")
            .value(),
        3.25
    );
    assert!(restored.generation().is_some());

    let mismatch = ShmWriterHandle::create_published(
        ShmRuntimeConfig::new(directory.path().join("other.shm"), 64),
        manifest(99),
        Some(&snapshot),
    );
    assert!(
        mismatch.is_err(),
        "snapshot layout mismatch must fail closed"
    );
}

#[test]
fn runtime_configuration_rejects_capacity_smaller_than_the_manifest() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let path = directory.path().join("aether.shm");
    let error =
        ShmWriterHandle::create_published(ShmRuntimeConfig::new(path, 1), manifest(17), None)
            .expect_err("manifest contains aligned T/S/C/A slots beyond capacity");
    assert!(error.to_string().contains("exceeds"));
}

#[test]
fn reader_reports_writer_liveness_from_the_physical_header() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let path = directory.path().join("aether.shm");
    let active_manifest = manifest(17);
    let handle = ShmWriterHandle::create_published(
        ShmRuntimeConfig::new(&path, 64),
        Arc::clone(&active_manifest),
        None,
    )
    .expect("publish generation");
    handle
        .generation()
        .expect("generation")
        .acquisition_writer()
        .update_heartbeat(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or(Duration::ZERO)
                .as_millis() as u64,
        );

    let reader = ShmChannelReader::open(path, active_manifest).expect("open reader");
    assert!(reader.is_writer_alive(Duration::from_secs(1)));
}
