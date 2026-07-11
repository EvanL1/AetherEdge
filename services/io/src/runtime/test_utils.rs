//! SHM-only test utilities for the I/O runtime.

use std::collections::BTreeMap;
use std::sync::Arc;

use aether_model::PointType;
use aether_routing::RoutingCache;
use aether_rtdb_shm::{
    ChannelPointCounts, ChannelToSlotIndex, SharedConfig, ShmHandle, SlotIo, UnifiedWriter,
};

/// Creates an empty but available SHM layout suitable for manager/API tests.
pub fn create_test_shm_handle() -> Arc<ShmHandle> {
    create_test_shm_handle_with_points(BTreeMap::new())
}

/// Creates an available SHM layout with explicit per-channel point counts.
pub fn create_test_shm_handle_with_points(points: BTreeMap<u32, [u32; 4]>) -> Arc<ShmHandle> {
    let directory = tempfile::Builder::new()
        .prefix("aether-io-shm-test-")
        .tempdir()
        .expect("create test SHM directory")
        .keep();
    let config = SharedConfig::default()
        .with_path(directory.join("io.shm"))
        .with_max_slots(65_536);
    let points = ChannelPointCounts::from_map(points);
    let writer = UnifiedWriter::create(&config, &points).expect("create test SHM writer");
    let index = ChannelToSlotIndex::from_unified_writer(&writer);
    Arc::new(ShmHandle::new(config, writer, index))
}

/// Creates an empty in-memory routing cache.
pub fn create_test_routing_cache() -> Arc<RoutingCache> {
    Arc::new(RoutingCache::new())
}

/// Verifies one channel point directly from the authoritative SHM slot.
#[allow(clippy::float_cmp)]
pub fn assert_channel_value(
    handle: &ShmHandle,
    channel_id: u32,
    point_type: PointType,
    point_id: u32,
    expected_value: f64,
) {
    let layout = handle.layout_arc().expect("test SHM layout");
    let slot = layout
        .index
        .lookup(channel_id, point_type, point_id)
        .expect("channel point slot");
    let sample = layout.writer.read_slot(slot).expect("channel point sample");
    assert_eq!(sample.value, expected_value);
}
