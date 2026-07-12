//! Shared SHM-only fixtures for I/O integration tests.

use std::sync::Arc;

use aether_shm_bridge::{ChannelPointManifest, ShmRuntimeConfig, ShmWriterHandle};

pub fn create_test_shm_handle() -> Arc<ShmWriterHandle> {
    let directory = tempfile::Builder::new()
        .prefix("aether-io-integration-shm-")
        .tempdir()
        .expect("create test SHM directory")
        .keep();
    let config = ShmRuntimeConfig::new(directory.join("io.shm"), 65_536);
    Arc::new(
        ShmWriterHandle::create_published(config, Arc::new(ChannelPointManifest::default()), None)
            .expect("compose typed SHM layout"),
    )
}
