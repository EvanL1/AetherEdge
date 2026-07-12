use std::collections::BTreeMap;
use std::sync::Arc;

use aether_dataplane::{SlotIo, SlotWriter};
use aether_domain::PointKind;
use aether_io::ShmDataStore;
use aether_io::protocols::core::data::{DataBatch, DataPoint};
use aether_io::protocols::core::error::GatewayError;
use aether_routing::RoutingCache;
use aether_shm_bridge::{
    ChannelPointManifest, ShmAcquisitionStateWriter, ShmRuntimeConfig, ShmWriterHandle,
};

fn create_test_handle() -> (tempfile::TempDir, Arc<ShmWriterHandle>) {
    let directory = tempfile::tempdir().expect("create temp SHM directory");
    let config = ShmRuntimeConfig::new(directory.path().join("io.shm"), 16);
    let manifest = Arc::new(ChannelPointManifest::from_map(BTreeMap::from([(
        7,
        [2, 1, 0, 0],
    )])));
    (
        directory,
        Arc::new(
            ShmWriterHandle::create_published(config, manifest, None)
                .expect("compose typed SHM layout"),
        ),
    )
}

#[tokio::test]
async fn shm_store_writes_poll_data_to_the_authoritative_slot() {
    let (_directory, handle) = create_test_handle();
    let store = ShmDataStore::new(Arc::clone(&handle), Arc::new(RoutingCache::default()))
        .expect("available SHM must construct the store");

    let mut batch = DataBatch::default();
    batch.add(DataPoint::telemetry(1, 42.5));
    store.write_batch(7, batch).await.expect("write SHM batch");

    let layout = handle.generation().expect("active layout");
    let slot = layout
        .manifest()
        .slot(7, PointKind::Telemetry, 1)
        .expect("telemetry slot");
    let sample = layout.read_slot(slot).expect("slot sample");
    assert_eq!(sample.value, 42.5);
}

#[test]
fn shm_store_rejects_an_unavailable_layout() {
    let directory = tempfile::tempdir().expect("create temp SHM directory");
    let handle = Arc::new(ShmWriterHandle::empty(ShmRuntimeConfig::new(
        directory.path().join("missing.shm"),
        16,
    )));

    let result = ShmDataStore::new(handle, Arc::new(RoutingCache::default()));
    assert!(
        result.is_err(),
        "missing authoritative SHM must fail closed"
    );
}

#[test]
fn production_store_source_does_not_call_legacy_batch_direct() {
    let source = include_str!("../src/store/shm_store.rs");
    assert!(
        !source.contains("write_channel_batch_direct"),
        "production ShmDataStore must call the typed acquisition writer"
    );
}

#[tokio::test]
async fn unknown_c2c_target_rejects_source_before_any_production_write() {
    use std::collections::HashMap;

    let (_directory, handle) = create_test_handle();
    let routing = Arc::new(RoutingCache::from_maps(
        HashMap::new(),
        HashMap::new(),
        HashMap::from([("7:T:0".to_string(), "99:T:0".to_string())]),
    ));
    let store = ShmDataStore::new(Arc::clone(&handle), routing).expect("production SHM store");
    let mut batch = DataBatch::default();
    batch.add(DataPoint::telemetry(0, 55.0));

    assert!(store.write_batch(7, batch).await.is_err());

    let layout = handle.generation().expect("active layout");
    let source_slot = layout
        .manifest()
        .slot(7, PointKind::Telemetry, 0)
        .expect("source slot");
    assert!(
        layout
            .read_slot(source_slot)
            .expect("source sample")
            .value
            .is_nan(),
        "route expansion must finish before the one typed port call"
    );
}

#[tokio::test]
async fn c2c_expansion_deduplicates_targets_before_the_typed_port_call() {
    use std::collections::HashMap;

    let directory = tempfile::tempdir().expect("create temp SHM directory");
    let config = ShmRuntimeConfig::new(directory.path().join("c2c.shm"), 16);
    let manifest = Arc::new(ChannelPointManifest::from_map(BTreeMap::from([
        (7, [2, 0, 0, 0]),
        (8, [1, 0, 0, 0]),
    ])));
    let handle = Arc::new(
        ShmWriterHandle::create_published(config, manifest, None)
            .expect("compose typed C2C SHM generation"),
    );
    let routing = Arc::new(RoutingCache::from_maps(
        HashMap::new(),
        HashMap::new(),
        HashMap::from([
            ("7:T:0".to_string(), "8:T:0".to_string()),
            ("7:T:1".to_string(), "8:T:0".to_string()),
        ]),
    ));
    let store = ShmDataStore::new(Arc::clone(&handle), routing).expect("production SHM store");
    let mut batch = DataBatch::default();
    batch.add(DataPoint::telemetry(0, 11.0));
    batch.add(DataPoint::telemetry(1, 22.0));

    store
        .write_batch(7, batch)
        .await
        .expect("deduplicated C2C batch");

    let layout = handle.generation().expect("active layout");
    let target_slot = layout
        .manifest()
        .slot(8, PointKind::Telemetry, 0)
        .expect("C2C target slot");
    assert_eq!(
        layout
            .read_slot(target_slot)
            .expect("C2C target sample")
            .value,
        11.0,
        "the first source deterministically wins a route-route collision"
    );
}

#[tokio::test]
async fn shm_store_composes_the_typed_acquisition_writer_atomically() {
    let directory = tempfile::tempdir().expect("create temp SHM directory");
    let manifest = ChannelPointManifest::from_entries([(7, [2, 1, 0, 0])]);
    let writer = Arc::new(
        SlotWriter::create(
            directory.path().join("typed-io.shm"),
            16,
            manifest.slot_count(),
            manifest.layout_hash(),
        )
        .expect("create typed acquisition writer"),
    );
    let acquisition_writer = Arc::new(ShmAcquisitionStateWriter::new(
        Arc::clone(&writer),
        Arc::new(manifest),
    ));
    let store = ShmDataStore::from_acquisition_writer(
        acquisition_writer,
        Arc::new(RoutingCache::default()),
    );

    let mut valid_batch = DataBatch::default();
    let mut telemetry = DataPoint::telemetry(1, 42.5);
    telemetry.timestamp = chrono::DateTime::from_timestamp_millis(1_001).expect("timestamp");
    valid_batch.add(telemetry);
    let mut status = DataPoint::signal(0, true);
    status.source_timestamp =
        Some(chrono::DateTime::from_timestamp_millis(1_002).expect("source timestamp"));
    valid_batch.add(status);

    store
        .write_batch(7, valid_batch)
        .await
        .expect("write typed acquisition batch");

    let telemetry = writer.read_slot(1).expect("telemetry slot");
    assert_eq!(telemetry.value, 42.5);
    assert_eq!(telemetry.raw, 42.5);
    assert_eq!(telemetry.timestamp_ms, 1_001);
    let status = writer.read_slot(2).expect("status slot");
    assert_eq!(status.value, 1.0);
    assert_eq!(status.raw, 1.0);
    assert_eq!(status.timestamp_ms, 1_002);

    let mut invalid_batch = DataBatch::default();
    invalid_batch.add(DataPoint::telemetry(0, 99.0));
    invalid_batch.add(DataPoint::telemetry(9, 123.0));
    let error = store
        .write_batch(7, invalid_batch)
        .await
        .expect_err("unknown physical point must fail closed");
    assert!(matches!(error, GatewayError::PointNotFound(_)));
    assert_eq!(store.slot_miss_count(), 1);
    assert!(
        writer
            .read_slot(0)
            .expect("known untouched slot")
            .value
            .is_nan(),
        "unknown second address must prevent the first point from being written"
    );
}

#[tokio::test]
async fn duplicate_batch_is_invalid_data_without_polluting_slot_miss_metric() {
    let directory = tempfile::tempdir().expect("create temp SHM directory");
    let manifest = ChannelPointManifest::from_entries([(7, [1, 0, 0, 0])]);
    let writer = Arc::new(
        SlotWriter::create(
            directory.path().join("duplicate-io.shm"),
            16,
            manifest.slot_count(),
            manifest.layout_hash(),
        )
        .expect("create typed acquisition writer"),
    );
    let store = ShmDataStore::from_acquisition_writer(
        Arc::new(ShmAcquisitionStateWriter::new(
            Arc::clone(&writer),
            Arc::new(manifest),
        )),
        Arc::new(RoutingCache::default()),
    );
    let mut batch = DataBatch::default();
    batch.add(DataPoint::telemetry(0, 11.0));
    batch.add(DataPoint::telemetry(0, 22.0));

    let error = store
        .write_batch(7, batch)
        .await
        .expect_err("duplicate address must reject the batch");

    assert!(matches!(error, GatewayError::InvalidData(_)));
    assert_eq!(store.slot_miss_count(), 0);
    assert!(writer.read_slot(0).expect("known slot").value.is_nan());
}

#[tokio::test]
async fn generation_conflict_is_retryable_without_polluting_slot_miss_metric() {
    let directory = tempfile::tempdir().expect("create temp SHM directory");
    let manifest = ChannelPointManifest::from_entries([(7, [1, 0, 0, 0])]);
    let writer = Arc::new(
        SlotWriter::create(
            directory.path().join("conflict-io.shm"),
            16,
            manifest.slot_count(),
            manifest.layout_hash() ^ 1,
        )
        .expect("create mismatched acquisition writer"),
    );
    let store = ShmDataStore::from_acquisition_writer(
        Arc::new(ShmAcquisitionStateWriter::new(
            Arc::clone(&writer),
            Arc::new(manifest),
        )),
        Arc::new(RoutingCache::default()),
    );
    let mut batch = DataBatch::default();
    batch.add(DataPoint::telemetry(0, 11.0));

    let error = store
        .write_batch(7, batch)
        .await
        .expect_err("generation conflict must fail closed");

    assert!(matches!(error, GatewayError::Connection(_)));
    assert!(error.is_retryable());
    assert_eq!(store.slot_miss_count(), 0);
    assert!(writer.read_slot(0).expect("known slot").value.is_nan());
}
