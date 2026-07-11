use std::collections::BTreeMap;
use std::sync::Arc;

use aether_io::ShmDataStore;
use aether_io::protocols::core::data::{DataBatch, DataPoint};
use aether_model::PointType;
use aether_routing::RoutingCache;
use aether_rtdb_shm::{
    ChannelPointCounts, ChannelToSlotIndex, SharedConfig, ShmHandle, SlotIo, UnifiedWriter,
};

fn create_test_handle() -> (tempfile::TempDir, Arc<ShmHandle>) {
    let directory = tempfile::tempdir().expect("create temp SHM directory");
    let config = SharedConfig::default()
        .with_path(directory.path().join("io.shm"))
        .with_max_slots(16);
    let points = ChannelPointCounts::from_map(BTreeMap::from([(7, [2, 1, 0, 0])]));
    let writer = UnifiedWriter::create(&config, &points).expect("create test SHM");
    let index = ChannelToSlotIndex::from_unified_writer(&writer);
    (directory, Arc::new(ShmHandle::new(config, writer, index)))
}

#[tokio::test]
async fn shm_store_writes_poll_data_to_the_authoritative_slot() {
    let (_directory, handle) = create_test_handle();
    let store = ShmDataStore::new(Arc::clone(&handle), Arc::new(RoutingCache::default()))
        .expect("available SHM must construct the store");

    let mut batch = DataBatch::default();
    batch.add(DataPoint::telemetry(1, 42.5));
    store.write_batch(7, batch).await.expect("write SHM batch");

    let layout = handle.layout_arc().expect("active layout");
    let slot = layout
        .index
        .lookup(7, PointType::Telemetry, 1)
        .expect("telemetry slot");
    let sample = layout.writer.read_slot(slot).expect("slot sample");
    assert_eq!(sample.value, 42.5);
}

#[test]
fn shm_store_rejects_an_unavailable_layout() {
    let directory = tempfile::tempdir().expect("create temp SHM directory");
    let handle = Arc::new(ShmHandle::empty(
        SharedConfig::default().with_path(directory.path().join("missing.shm")),
    ));

    let result = ShmDataStore::new(handle, Arc::new(RoutingCache::default()));
    assert!(
        result.is_err(),
        "missing authoritative SHM must fail closed"
    );
}
