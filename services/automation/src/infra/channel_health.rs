//! SHM-backed channel connectivity gate for M2C commands.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use aether_shm_bridge::{
    ChannelHealthManifest, ShmChannelHealthReader, ShmClientConfig, channel_health_path_from_shm,
};
use sqlx::SqlitePool;

/// Builds a lazy, self-healing reader for the channel-health SHM segment.
///
/// Channel identifiers come from authoritative SQLite configuration. The
/// resulting manifest hash must match the one published by aether-io.
pub async fn build_reader(
    pool: &SqlitePool,
    live_state_path: &Path,
) -> anyhow::Result<ShmChannelHealthReader> {
    let raw_ids =
        sqlx::query_scalar::<_, i64>("SELECT channel_id FROM channels ORDER BY channel_id")
            .fetch_all(pool)
            .await?;
    let channel_ids = raw_ids
        .into_iter()
        .map(u32::try_from)
        .collect::<Result<Vec<_>, _>>()?;
    let manifest = Arc::new(ChannelHealthManifest::from_channel_ids(channel_ids));
    let health_path = std::env::var("AETHER_CHANNEL_HEALTH_SHM_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|_| channel_health_path_from_shm(live_state_path));
    let client = ShmClientConfig::new(health_path, manifest.layout_hash())
        .with_writer_stale_after(Duration::from_secs(30));
    Ok(ShmChannelHealthReader::new(client, manifest))
}
