//! Compatibility entry point for the canonical SQLite SHM topology adapter.

use aether_shm_bridge::ChannelPointManifest;

/// Loads the deterministic T/S/C/A slot manifest from configured point tables.
pub async fn load_channel_point_manifest(
    pool: &sqlx::SqlitePool,
) -> anyhow::Result<ChannelPointManifest> {
    let (points, _health) = aether_store_local::load_sqlite_shm_topology(pool)
        .await?
        .into_manifests();
    Ok(points)
}
