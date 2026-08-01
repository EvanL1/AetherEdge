//! Coherent rule-scheduler and PointWatch runtime publication.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use aether_ports::{PortError, PortErrorKind, PortResult};
use aether_rules::RuleScheduler;
use aether_shm_bridge::{ChannelPointManifestSource, PointWatchEvent, SubscriptionBitmap};
use tokio::sync::Mutex;

use crate::infra::runtime_topology::{AutomationTopologyGeneration, AutomationTopologyHandle};

/// Result of publishing one coherent rule runtime refresh.
pub enum RuleRuntimeRefresh {
    /// The scheduler and every configured publication path are current.
    Refreshed {
        /// Number of rules loaded into the scheduler.
        rule_count: usize,
        /// Exact PointWatch topology sequence, when the optional path is enabled.
        topology_sequence: Option<u64>,
    },
    /// The scheduler is current, but PointWatch remains closed.
    PointWatchGated {
        /// Number of rules loaded into the scheduler.
        rule_count: usize,
        /// Topology sequence whose PointWatch state could not be published.
        topology_sequence: u64,
        /// Typed publication failure.
        failure: PortError,
    },
}

impl RuleRuntimeRefresh {
    /// Returns the number of rules loaded into the scheduler.
    #[must_use]
    pub const fn rule_count(&self) -> usize {
        match self {
            Self::Refreshed { rule_count, .. } | Self::PointWatchGated { rule_count, .. } => {
                *rule_count
            },
        }
    }

    /// Returns the topology sequence used for PointWatch publication.
    #[must_use]
    pub const fn topology_sequence(&self) -> Option<u64> {
        match self {
            Self::Refreshed {
                topology_sequence, ..
            } => *topology_sequence,
            Self::PointWatchGated {
                topology_sequence, ..
            } => Some(*topology_sequence),
        }
    }

    /// Returns why PointWatch remains gated after a scheduler refresh.
    #[must_use]
    pub const fn point_watch_failure(&self) -> Option<&PortError> {
        match self {
            Self::Refreshed { .. } => None,
            Self::PointWatchGated { failure, .. } => Some(failure),
        }
    }
}

struct PointWatchRuntime {
    topology: Arc<AutomationTopologyHandle>,
    bitmap: Arc<SubscriptionBitmap>,
    manifest_source: ChannelPointManifestSource,
    ready_sequence: AtomicU64,
}

impl PointWatchRuntime {
    fn new(
        topology: Arc<AutomationTopologyHandle>,
        bitmap: Arc<SubscriptionBitmap>,
        manifest_source: ChannelPointManifestSource,
    ) -> Self {
        Self {
            topology,
            bitmap,
            manifest_source,
            ready_sequence: AtomicU64::new(u64::MAX),
        }
    }

    fn mark_unready(&self) {
        self.ready_sequence.store(u64::MAX, Ordering::Release);
    }

    fn mark_ready(&self, sequence: u64) {
        self.ready_sequence.store(sequence, Ordering::Release);
    }

    fn accepts(&self, generation: &AutomationTopologyGeneration, event: PointWatchEvent) -> bool {
        generation
            .accepts_ready_point_watch_event(event, self.ready_sequence.load(Ordering::Acquire))
    }
}

/// Owns the only scheduler-to-PointWatch publication path in automation.
pub struct RuleRuntimeCoordinator {
    scheduler: Arc<RuleScheduler>,
    point_watch: Option<PointWatchRuntime>,
    reload_gate: Mutex<()>,
}

impl RuleRuntimeCoordinator {
    /// Creates a scheduler-only runtime with the tick path always available.
    #[must_use]
    pub fn new(scheduler: Arc<RuleScheduler>) -> Self {
        Self {
            scheduler,
            point_watch: None,
            reload_gate: Mutex::new(()),
        }
    }

    /// Enables PointWatch only when its complete publication bundle exists.
    #[must_use]
    pub fn with_point_watch(
        mut self,
        topology: Arc<AutomationTopologyHandle>,
        bitmap: Arc<SubscriptionBitmap>,
        manifest_source: ChannelPointManifestSource,
    ) -> Self {
        self.point_watch = Some(PointWatchRuntime::new(topology, bitmap, manifest_source));
        self
    }

    /// Reloads rules and atomically publishes matching PointWatch state.
    pub async fn reload(&self) -> PortResult<RuleRuntimeRefresh> {
        let _reload = self.reload_gate.lock().await;
        let Some(point_watch) = &self.point_watch else {
            let rule_count = self
                .scheduler
                .reload_rules()
                .await
                .map_err(scheduler_reload_error)?;
            return Ok(RuleRuntimeRefresh::Refreshed {
                rule_count,
                topology_sequence: None,
            });
        };

        let view = Arc::clone(&point_watch.topology).pin_command().await;
        point_watch.mark_unready();
        let rule_count = self
            .scheduler
            .reload_rules()
            .await
            .map_err(scheduler_reload_error)?;
        let generation = view.generation();
        let point_watch_rebuilt = generation
            .rebuild_point_watch(&self.scheduler, Some(&point_watch.bitmap))
            .await;
        let manifest_matches = point_watch.manifest_source.load().is_some_and(|manifest| {
            manifest.layout_hash() == generation.point_manifest().layout_hash()
                && manifest.slot_count() == generation.point_manifest().slot_count()
        });
        let topology_sequence = generation.sequence();
        if point_watch_rebuilt && manifest_matches {
            point_watch.mark_ready(topology_sequence);
            Ok(RuleRuntimeRefresh::Refreshed {
                rule_count,
                topology_sequence: Some(topology_sequence),
            })
        } else {
            Ok(RuleRuntimeRefresh::PointWatchGated {
                rule_count,
                topology_sequence,
                failure: PortError::new(
                    PortErrorKind::Unavailable,
                    "PointWatch subscription publication does not match the active topology generation",
                ),
            })
        }
    }

    /// Subscribes only when topology changes can affect PointWatch publication.
    #[must_use]
    pub fn topology_changes(&self) -> Option<tokio::sync::watch::Receiver<u64>> {
        self.point_watch
            .as_ref()
            .map(|point_watch| point_watch.topology.subscribe())
    }

    /// Returns whether a hint belongs to the exact published PointWatch generation.
    #[must_use]
    pub fn accepts_point_watch(
        &self,
        generation: &AutomationTopologyGeneration,
        event: PointWatchEvent,
    ) -> bool {
        self.point_watch
            .as_ref()
            .is_some_and(|point_watch| point_watch.accepts(generation, event))
    }

    /// Stops scheduling after a durable mutation cannot be reflected in memory.
    pub fn stop(&self) {
        self.scheduler.stop();
    }
}

fn scheduler_reload_error(error: aether_rules::RuleError) -> PortError {
    PortError::new(
        PortErrorKind::Unavailable,
        format!("rule scheduler reload failed: {error}"),
    )
}
