//! SHM-only test utilities for the I/O runtime.

use std::collections::BTreeMap;
use std::sync::Arc;

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
