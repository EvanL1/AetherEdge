//! SHM-backed channel connectivity gate for M2C commands.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use aether_shm_bridge::{
    ChannelHealthManifest, ShmChannelHealthReader, ShmClientConfig, channel_health_path_from_shm,
};

/// Builds a lazy, self-healing reader for the channel-health SHM segment.
///
/// The manifest comes from the same canonical SQLite snapshot as the point
/// manifest, so its hash matches the generation published by aether-io.
pub fn build_reader(
    manifest: Arc<ChannelHealthManifest>,
    live_state_path: &Path,
) -> ShmChannelHealthReader {
    let health_path = std::env::var("AETHER_CHANNEL_HEALTH_SHM_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|_| channel_health_path_from_shm(live_state_path));
    let client = ShmClientConfig::new(health_path, manifest.layout_hash())
        .with_writer_stale_after(Duration::from_secs(30));
    ShmChannelHealthReader::new(client, manifest)
}
