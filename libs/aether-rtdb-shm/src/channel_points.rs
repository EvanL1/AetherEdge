//! Channel point counts for SHM slot allocation.
//!
//! Provides a routing-independent data source for SHM layout.
//! Slot allocation is based on channel point definitions (from SQLite point tables),
//! not on routing mappings. This decouples SHM layout from routing changes.

use std::collections::BTreeMap;
use std::hash::{Hash, Hasher};

/// Per-channel point counts: [T, S, C, A].
///
/// Each element is `max(point_id) + 1` for that type, representing
/// the number of slots needed. BTreeMap ensures deterministic iteration order.
#[derive(Debug, Clone, Default)]
pub struct ChannelPointCounts(pub BTreeMap<u32, [u32; 4]>);

impl ChannelPointCounts {
    pub fn new() -> Self {
        Self(BTreeMap::new())
    }

    /// Load channel point counts from SQLite point tables.
    ///
    /// Queries telemetry_points, signal_points, control_points, adjustment_points
    /// to determine max(point_id)+1 per channel per type.
    pub async fn load_from_db(pool: &sqlx::SqlitePool) -> anyhow::Result<Self> {
        let mut counts: BTreeMap<u32, [u32; 4]> = BTreeMap::new();

        // Query each point table for max point_id per channel
        let tables = [
            ("telemetry_points", 0usize), // T
            ("signal_points", 1),         // S
            ("control_points", 2),        // C
            ("adjustment_points", 3),     // A
        ];

        for (table, type_idx) in tables {
            // Every configured point, including a virtual channel point, must
            // have an authoritative SHM slot. There is no secondary live-value
            // store that can absorb an omitted point.
            let query = format!(
                "SELECT channel_id, MAX(point_id) + 1 AS cnt FROM {} GROUP BY channel_id",
                table
            );
            let rows: Vec<(i64, i64)> = sqlx::query_as(&query).fetch_all(pool).await?;

            for (channel_id, cnt) in rows {
                let entry = counts.entry(channel_id as u32).or_insert([0; 4]);
                entry[type_idx] = cnt as u32;
            }
        }

        Ok(Self(counts))
    }

    /// Compute a deterministic hash of the channel point layout.
    ///
    /// Same channel points → same hash → same SHM slot allocation.
    /// Used for cross-process SHM header validation.
    pub fn layout_hash(&self) -> u64 {
        let mut hasher = rustc_hash::FxHasher::default();
        // BTreeMap iterates in sorted key order — deterministic
        for (channel_id, counts) in &self.0 {
            channel_id.hash(&mut hasher);
            counts.hash(&mut hasher);
        }
        hasher.finish()
    }

    /// Build from a raw BTreeMap (for tests and non-async contexts).
    pub fn from_map(map: BTreeMap<u32, [u32; 4]>) -> Self {
        Self(map)
    }

    /// Get the inner map.
    pub fn inner(&self) -> &BTreeMap<u32, [u32; 4]> {
        &self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn virtual_channels_receive_authoritative_shm_slots() {
        let pool = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();
        sqlx::query(
            "CREATE TABLE channels (channel_id INTEGER PRIMARY KEY, protocol TEXT NOT NULL)",
        )
        .execute(&pool)
        .await
        .unwrap();
        for table in [
            "telemetry_points",
            "signal_points",
            "control_points",
            "adjustment_points",
        ] {
            sqlx::query(&format!(
                "CREATE TABLE {table} (channel_id INTEGER NOT NULL, point_id INTEGER NOT NULL)"
            ))
            .execute(&pool)
            .await
            .unwrap();
        }
        sqlx::query("INSERT INTO channels (channel_id, protocol) VALUES (17, 'virtual')")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO telemetry_points (channel_id, point_id) VALUES (17, 3)")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO signal_points (channel_id, point_id) VALUES (17, 1)")
            .execute(&pool)
            .await
            .unwrap();

        let counts = ChannelPointCounts::load_from_db(&pool).await.unwrap();

        assert_eq!(counts.inner().get(&17), Some(&[4, 2, 0, 0]));
    }
}
