//! SHM-only test utilities for the I/O runtime.

use std::collections::BTreeMap;
use std::sync::Arc;

use aether_model::PointType;
use aether_routing::RoutingCache;
use aether_shm_bridge::{ChannelPointManifest, ShmRuntimeConfig, ShmWriterHandle};

/// Creates an empty but available SHM layout suitable for manager/API tests.
pub fn create_test_shm_handle() -> Arc<ShmWriterHandle> {
    create_test_shm_handle_with_points(BTreeMap::new())
}

/// Creates an available SHM layout with explicit per-channel point counts.
pub fn create_test_shm_handle_with_points(points: BTreeMap<u32, [u32; 4]>) -> Arc<ShmWriterHandle> {
    let directory = tempfile::Builder::new()
        .prefix("aether-io-shm-test-")
        .tempdir()
        .expect("create test SHM directory")
        .keep();
    let config = ShmRuntimeConfig::new(directory.join("io.shm"), 65_536);
    let manifest = Arc::new(ChannelPointManifest::from_map(points));
    Arc::new(
        ShmWriterHandle::create_published(config, manifest, None)
            .expect("compose typed SHM layout"),
    )
}

/// Creates an empty in-memory routing cache.
pub fn create_test_routing_cache() -> Arc<RoutingCache> {
    Arc::new(RoutingCache::new())
}

/// Verifies one channel point directly from the authoritative SHM slot.
#[allow(clippy::float_cmp)]
pub fn assert_channel_value(
    handle: &ShmWriterHandle,
    channel_id: u32,
    point_type: PointType,
    point_id: u32,
    expected_value: f64,
) {
    let layout = handle.generation().expect("test SHM layout");
    let kind = match point_type {
        PointType::Telemetry => aether_domain::PointKind::Telemetry,
        PointType::Signal => aether_domain::PointKind::Status,
        PointType::Control => aether_domain::PointKind::Command,
        PointType::Adjustment => aether_domain::PointKind::Action,
    };
    let slot = layout
        .manifest()
        .slot(channel_id, kind, point_id)
        .expect("channel point slot");
    let sample = layout.read_slot(slot).expect("channel point sample");
    assert_eq!(sample.value, expected_value);
}
