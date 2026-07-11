//! Shared SHM-only fixtures for I/O integration tests.

use std::sync::Arc;

use aether_rtdb_shm::{
    ChannelPointCounts, ChannelToSlotIndex, SharedConfig, ShmHandle, UnifiedWriter,
};

pub fn create_test_shm_handle() -> Arc<ShmHandle> {
    let directory = tempfile::Builder::new()
        .prefix("aether-io-integration-shm-")
        .tempdir()
        .expect("create test SHM directory")
        .keep();
    let config = SharedConfig::default()
        .with_path(directory.join("io.shm"))
        .with_max_slots(65_536);
    let writer = UnifiedWriter::create(&config, &ChannelPointCounts::new())
        .expect("create integration-test SHM writer");
    let index = ChannelToSlotIndex::from_unified_writer(&writer);
    Arc::new(ShmHandle::new(config, writer, index))
}
