//! Canonical SQLite projection into the two SHM topology manifests.

use std::collections::BTreeMap;

use aether_ports::{PortError, PortErrorKind, PortResult};
use aether_shm_bridge::{ChannelHealthManifest, ChannelPointManifest};
use sqlx::SqlitePool;

const POINT_COUNT_QUERIES: [(&str, &str, usize); 4] = [
    (
        "SELECT channel_id, MAX(point_id) + 1 FROM telemetry_points GROUP BY channel_id",
        "telemetry_points",
        0,
    ),
    (
        "SELECT channel_id, MAX(point_id) + 1 FROM signal_points GROUP BY channel_id",
        "signal_points",
        1,
    ),
    (
        "SELECT channel_id, MAX(point_id) + 1 FROM control_points GROUP BY channel_id",
        "control_points",
        2,
    ),
    (
        "SELECT channel_id, MAX(point_id) + 1 FROM adjustment_points GROUP BY channel_id",
        "adjustment_points",
        3,
    ),
];

/// Point and channel-health manifests observed from one SQLite read transaction.
#[derive(Debug, Clone)]
pub struct SqliteShmTopologySnapshot {
    point_manifest: ChannelPointManifest,
    health_manifest: ChannelHealthManifest,
}

impl SqliteShmTopologySnapshot {
    /// Returns the deterministic T/S/C/A slot manifest.
    #[must_use]
    pub const fn point_manifest(&self) -> &ChannelPointManifest {
        &self.point_manifest
    }

    /// Returns the configured-channel manifest for the health plane.
    #[must_use]
    pub const fn health_manifest(&self) -> &ChannelHealthManifest {
        &self.health_manifest
    }

    /// Splits the snapshot into owned manifests without another database read.
    #[must_use]
    pub fn into_manifests(self) -> (ChannelPointManifest, ChannelHealthManifest) {
        (self.point_manifest, self.health_manifest)
    }
}

/// Loads point and channel-health topology from one authoritative SQLite snapshot.
///
/// All configured point rows participate, including telemetry and signal rows
/// owned by virtual channels. This keeps every SHM client on the writer's exact
/// layout hash.
pub async fn load_sqlite_shm_topology(pool: &SqlitePool) -> PortResult<SqliteShmTopologySnapshot> {
    let mut transaction = pool.begin().await.map_err(topology_unavailable)?;
    let mut counts = BTreeMap::<u32, [u32; 4]>::new();

    for (query, table, kind_index) in POINT_COUNT_QUERIES {
        let rows = sqlx::query_as::<_, (i64, i64)>(query)
            .fetch_all(&mut *transaction)
            .await
            .map_err(topology_unavailable)?;
        for (raw_channel_id, raw_count) in rows {
            let channel_id = stored_u32(raw_channel_id, "channel_id", table)?;
            let count = stored_u32(raw_count, "point count", table)?;
            counts.entry(channel_id).or_insert([0; 4])[kind_index] = count;
        }
    }

    let raw_channel_ids =
        sqlx::query_scalar::<_, i64>("SELECT channel_id FROM channels ORDER BY channel_id")
            .fetch_all(&mut *transaction)
            .await
            .map_err(topology_unavailable)?;
    let channel_ids = raw_channel_ids
        .into_iter()
        .map(|channel_id| stored_u32(channel_id, "channel_id", "channels"))
        .collect::<PortResult<Vec<_>>>()?;

    transaction.commit().await.map_err(topology_unavailable)?;
    Ok(SqliteShmTopologySnapshot {
        point_manifest: ChannelPointManifest::from_map(counts),
        health_manifest: ChannelHealthManifest::from_channel_ids(channel_ids),
    })
}

fn stored_u32(value: i64, field: &str, table: &str) -> PortResult<u32> {
    u32::try_from(value).map_err(|_| {
        PortError::new(
            PortErrorKind::InvalidData,
            format!("stored {field} in {table} is outside the u32 range"),
        )
    })
}

fn topology_unavailable(_error: sqlx::Error) -> PortError {
    PortError::new(
        PortErrorKind::Unavailable,
        "authoritative SQLite topology is unavailable",
    )
}
