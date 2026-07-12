//! SQLite-backed projection of desired point/channel topology into SHM.

use std::sync::Arc;

use aether_ports::{PortError, PortErrorKind, PortResult};
use aether_shm_bridge::{
    ChannelHealthManifest, ChannelPointManifest, ShmChannelHealthWriterHandle, ShmWriterHandle,
};
use aether_store_local::load_sqlite_shm_topology;
use sqlx::SqlitePool;
use tokio::sync::Mutex;

/// Sanitized outcome of one topology projection attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShmTopologyProjectionReceipt {
    current: bool,
    changed: bool,
    live_state_generation: Option<u64>,
    channel_health_generation: Option<u64>,
}

impl ShmTopologyProjectionReceipt {
    /// Returns whether both writer planes match the same observed SQLite snapshot.
    #[must_use]
    pub const fn is_current(self) -> bool {
        self.current
    }

    /// Returns whether this attempt requested at least one generation change.
    #[must_use]
    pub const fn changed(self) -> bool {
        self.changed
    }

    /// Returns the published live-point generation, when available.
    #[must_use]
    pub const fn live_state_generation(self) -> Option<u64> {
        self.live_state_generation
    }

    /// Returns the published channel-health generation, when available.
    #[must_use]
    pub const fn channel_health_generation(self) -> Option<u64> {
        self.channel_health_generation
    }
}

/// Projects one coherent SQLite topology snapshot into the two SHM writer planes.
///
/// Point values are deliberately not migrated across topology generations. The
/// health handle owns its narrower intersection-state migration policy.
pub struct SqliteShmTopologyProjector {
    pool: SqlitePool,
    live_state: Arc<ShmWriterHandle>,
    channel_health: Arc<ShmChannelHealthWriterHandle>,
    gate: Mutex<()>,
}

impl SqliteShmTopologyProjector {
    /// Creates the single process-local topology projection owner.
    #[must_use]
    pub fn new(
        pool: SqlitePool,
        live_state: Arc<ShmWriterHandle>,
        channel_health: Arc<ShmChannelHealthWriterHandle>,
    ) -> Self {
        Self {
            pool,
            live_state,
            channel_health,
            gate: Mutex::new(()),
        }
    }

    /// Publishes fresh generations when the desired topology changed.
    ///
    /// Capacity and SQLite snapshot failures happen before any writer changes
    /// and are returned as port errors. Once publication begins, a partial
    /// failure is an accepted degraded receipt rather than a retryable error.
    pub async fn project(&self) -> PortResult<ShmTopologyProjectionReceipt> {
        let _guard = self.gate.lock().await;
        let desired = load_topology_snapshot(&self.pool).await?;
        self.preflight(&desired)?;

        let point_changed = self.live_state.generation().is_none_or(|generation| {
            generation.manifest().layout_hash() != desired.points.layout_hash()
                || generation.manifest().slot_count() != desired.points.slot_count()
        });
        let health_changed = self.channel_health.manifest().is_none_or(|manifest| {
            manifest.layout_hash() != desired.health.layout_hash()
                || manifest.slot_count() != desired.health.slot_count()
        });
        let changed = point_changed || health_changed;

        if !changed {
            return Ok(self.receipt(true, false));
        }

        let live_state = Arc::clone(&self.live_state);
        let channel_health = Arc::clone(&self.channel_health);
        let point_manifest = Arc::clone(&desired.points);
        let health_manifest = Arc::clone(&desired.health);
        let publication = tokio::task::spawn_blocking(move || {
            if point_changed && let Err(error) = live_state.rebuild(point_manifest) {
                return Err(error);
            }
            if health_changed && let Err(error) = channel_health.rebuild(health_manifest) {
                return Err(error);
            }
            Ok(())
        })
        .await;

        let publication_succeeded = match publication {
            Ok(Ok(())) => true,
            Ok(Err(error)) => {
                tracing::error!(
                    error_kind = ?error.kind(),
                    "SHM topology publication is degraded after it began"
                );
                false
            },
            Err(error) => {
                tracing::error!("SHM topology publication worker failed: {error}");
                false
            },
        };
        if !publication_succeeded {
            return Ok(self.receipt(false, true));
        }

        let stable = match load_topology_snapshot(&self.pool).await {
            Ok(latest) => {
                latest.points.layout_hash() == desired.points.layout_hash()
                    && latest.points.slot_count() == desired.points.slot_count()
                    && latest.health.layout_hash() == desired.health.layout_hash()
                    && latest.health.slot_count() == desired.health.slot_count()
                    && self.matches(&desired)
            },
            Err(error) => {
                tracing::error!(
                    error_kind = ?error.kind(),
                    "SHM topology authority could not be re-observed after publication"
                );
                false
            },
        };
        Ok(self.receipt(stable, true))
    }

    fn preflight(&self, desired: &TopologySnapshot) -> PortResult<()> {
        if desired.points.slot_count() > self.live_state.config().max_slots() as usize {
            return Err(PortError::new(
                PortErrorKind::InvalidData,
                format!(
                    "desired live-state slot count {} exceeds configured capacity {}",
                    desired.points.slot_count(),
                    self.live_state.config().max_slots()
                ),
            ));
        }
        u32::try_from(desired.health.slot_count()).map_err(|_| {
            PortError::new(
                PortErrorKind::InvalidData,
                "desired channel-health topology exceeds u32 capacity",
            )
        })?;
        Ok(())
    }

    fn matches(&self, desired: &TopologySnapshot) -> bool {
        let points_match = self.live_state.generation().is_some_and(|generation| {
            generation.manifest().layout_hash() == desired.points.layout_hash()
                && generation.manifest().slot_count() == desired.points.slot_count()
        });
        let health_matches = self.channel_health.manifest().is_some_and(|manifest| {
            manifest.layout_hash() == desired.health.layout_hash()
                && manifest.slot_count() == desired.health.slot_count()
        });
        points_match && health_matches
    }

    fn receipt(&self, current: bool, changed: bool) -> ShmTopologyProjectionReceipt {
        ShmTopologyProjectionReceipt {
            current,
            changed,
            live_state_generation: self
                .live_state
                .generation()
                .map(|generation| generation.generation()),
            channel_health_generation: self.channel_health.generation(),
        }
    }
}

struct TopologySnapshot {
    points: Arc<ChannelPointManifest>,
    health: Arc<ChannelHealthManifest>,
}

async fn load_topology_snapshot(pool: &SqlitePool) -> PortResult<TopologySnapshot> {
    let (points, health) = load_sqlite_shm_topology(pool).await?.into_manifests();
    Ok(TopologySnapshot {
        points: Arc::new(points),
        health: Arc::new(health),
    })
}
