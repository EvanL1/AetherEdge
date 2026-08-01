//! Coherent runtime publication of automation's physical and logical topology.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use aether_domain::ChannelPointAddress;
use aether_ports::{ChannelHealthObservation, PortError, PortErrorKind, PortResult};
use aether_rules::{MeasurementRouteBinding, RuleScheduler};
use aether_shm_bridge::{
    ChannelPointManifest, PhysicalPointAddress, PointWatchEvent, ShmClientConfig,
    ShmDeviceCommandSink, ShmReadTopologyGeneration, SlotSource, SubscriptionBitmap,
};
use aether_sqlite_topology::{
    LogicalCommandRoutes, LogicalPointRoutes, RoutedCommandTarget, SqliteLiveTopologySnapshot,
};
use arc_swap::ArcSwap;
use sqlx::SqlitePool;
use tokio::sync::{Mutex, OwnedMutexGuard, OwnedRwLockReadGuard, RwLock, watch};

const WRITER_STALE_AFTER: Duration = Duration::from_secs(30);

struct CandidateParts {
    point_manifest: Arc<ChannelPointManifest>,
    health_manifest: Arc<aether_shm_bridge::ChannelHealthManifest>,
    measurement_routes: Arc<LogicalPointRoutes>,
    action_routes: Arc<LogicalCommandRoutes>,
    digest: u64,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum RefreshMode {
    ValidatePhysical,
    PublishConfiguration,
}

#[derive(Clone, Copy)]
enum RoutingPlane {
    Measurement,
    Action,
}

impl RoutingPlane {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Measurement => "measurement",
            Self::Action => "action",
        }
    }
}

impl CandidateParts {
    fn from_snapshot(snapshot: SqliteLiveTopologySnapshot) -> Self {
        let digest = snapshot.digest();
        let (point_manifest, health_manifest, measurements, actions) = snapshot.into_parts();
        Self {
            point_manifest: Arc::new(point_manifest),
            health_manifest: Arc::new(health_manifest),
            measurement_routes: Arc::new(measurements),
            action_routes: Arc::new(actions),
            digest,
        }
    }
}

/// One immutable automation view of point state, channel health, and routing.
pub struct AutomationTopologyGeneration {
    read: Arc<ShmReadTopologyGeneration>,
    measurement_routes: Arc<LogicalPointRoutes>,
    action_routes: Arc<LogicalCommandRoutes>,
    digest: u64,
    sequence: u64,
    physical_validated: bool,
    measurement_routes_revoked: bool,
    action_routes_revoked: bool,
}

impl std::fmt::Debug for AutomationTopologyGeneration {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AutomationTopologyGeneration")
            .field("read", &self.read)
            .field("measurement_routes", &self.measurement_routes.len())
            .field("action_routes", &self.action_routes.len())
            .field("digest", &self.digest)
            .field("sequence", &self.sequence)
            .field("physical_validated", &self.physical_validated)
            .field(
                "measurement_routes_revoked",
                &self.measurement_routes_revoked,
            )
            .field("action_routes_revoked", &self.action_routes_revoked)
            .finish()
    }
}

impl AutomationTopologyGeneration {
    fn compose(
        read: Arc<ShmReadTopologyGeneration>,
        parts: CandidateParts,
        physical_validated: bool,
        sequence: u64,
    ) -> Self {
        Self {
            read,
            measurement_routes: parts.measurement_routes,
            action_routes: parts.action_routes,
            digest: parts.digest,
            sequence,
            physical_validated,
            measurement_routes_revoked: false,
            action_routes_revoked: false,
        }
    }

    /// Returns the deterministic SQLite topology digest.
    #[must_use]
    pub const fn digest(&self) -> u64 {
        self.digest
    }

    /// Returns the service-local publication sequence used by PointWatch.
    #[must_use]
    pub const fn sequence(&self) -> u64 {
        self.sequence
    }

    /// Returns the number of enabled logical routes in this generation.
    #[must_use]
    pub fn route_count(&self) -> usize {
        self.measurement_routes.len() + self.action_routes.len()
    }

    /// Returns the point manifest pinned to this complete service generation.
    #[must_use]
    pub fn point_manifest(&self) -> &Arc<ChannelPointManifest> {
        self.read.point_manifest()
    }

    /// Resolves one logical measurement point in this generation.
    #[must_use]
    pub fn measurement_route(
        &self,
        instance_id: u32,
        point_id: u32,
    ) -> Option<PhysicalPointAddress> {
        self.measurement_routes
            .get(&(instance_id, point_id))
            .copied()
    }

    /// Resolves one logical action point in this generation.
    #[must_use]
    pub fn action_route(&self, instance_id: u32, point_id: u32) -> Option<PhysicalPointAddress> {
        self.command_route(instance_id, point_id)
            .map(RoutedCommandTarget::physical_target)
    }

    /// Resolves one command route with constraints from this exact generation.
    #[must_use]
    pub fn command_route(&self, instance_id: u32, point_id: u32) -> Option<RoutedCommandTarget> {
        self.action_routes.get(&(instance_id, point_id)).copied()
    }

    /// Copies the exact logical measurement bindings pinned to this generation.
    pub fn measurement_route_bindings(&self) -> PortResult<Vec<MeasurementRouteBinding>> {
        self.measurement_routes
            .iter()
            .map(|(&(instance_id, point_id), &target)| {
                let target =
                    ChannelPointAddress::new(target.channel_id(), target.kind(), target.point_id())
                        .map_err(|_| {
                            PortError::new(
                                PortErrorKind::InvalidData,
                                "measurement route targets a command-owned channel point",
                            )
                        })?;
                Ok(MeasurementRouteBinding::new(instance_id, point_id, target))
            })
            .collect()
    }

    /// Rebuilds the transport-neutral rule index and publishes its canonical
    /// points through the service-owned SHM bitmap.
    pub async fn rebuild_point_watch<S>(
        &self,
        scheduler: &RuleScheduler<S>,
        bitmap: Option<&SubscriptionBitmap>,
    ) -> bool
    where
        S: aether_calc::StateStore + 'static,
    {
        let (Ok(bindings), Some(bitmap)) = (self.measurement_route_bindings(), bitmap) else {
            return false;
        };
        let Some(subscriptions) = scheduler.rebuild_point_watch(&bindings).await else {
            return false;
        };

        bitmap.clear_all();
        for address in subscriptions {
            let physical =
                PhysicalPointAddress::new(address.channel_id(), address.kind(), address.point_id());
            let Some(slot) = self.point_manifest().slot_for(physical) else {
                bitmap.clear_all();
                return false;
            };
            bitmap.set_watched(slot);
        }
        true
    }

    /// Reads one logical point without mixing routing and SHM generations.
    pub fn read_instance_point(
        &self,
        instance_id: u32,
        action: bool,
        point_id: u32,
    ) -> PortResult<Option<(f64, u64)>> {
        let target = if action {
            self.action_route(instance_id, point_id)
        } else {
            self.measurement_route(instance_id, point_id)
        };
        let Some(target) = target else {
            return Ok(None);
        };
        let Some(slot) = self.read.point_manifest().slot_for(target) else {
            return Err(PortError::new(
                PortErrorKind::Conflict,
                "logical route is absent from its pinned point manifest",
            ));
        };
        let Some(sample) = self.read.point_source().read_slot(slot)? else {
            return Ok(None);
        };
        if sample.value().is_nan() {
            return Ok(None);
        }
        if !sample.value().is_finite() {
            return Err(PortError::new(
                PortErrorKind::InvalidData,
                "authoritative SHM contains a non-finite point value",
            ));
        }
        Ok(Some((sample.value(), sample.timestamp_ms())))
    }

    /// Reads channel connectivity from the health plane pinned to this generation.
    pub fn channel_health(&self, channel_id: u32) -> PortResult<Option<ChannelHealthObservation>> {
        self.read.channel_health().read_channel(channel_id)
    }

    /// Rejects queued PointWatch hints whose typed slot was remapped.
    #[must_use]
    pub fn accepts_point_watch_event(&self, event: PointWatchEvent) -> bool {
        event.matches_manifest(self.read.point_manifest())
    }

    /// Accepts a hint only for the exact sequence whose index was published.
    #[must_use]
    pub fn accepts_ready_point_watch_event(
        &self,
        event: PointWatchEvent,
        ready_sequence: u64,
    ) -> bool {
        self.sequence == ready_sequence && self.accepts_point_watch_event(event)
    }

    fn has_physical_layout(&self, parts: &CandidateParts) -> bool {
        self.read.point_manifest().layout_hash() == parts.point_manifest.layout_hash()
            && self.read.point_manifest().slot_count() == parts.point_manifest.slot_count()
            && self.read.health_manifest().layout_hash() == parts.health_manifest.layout_hash()
            && self.read.health_manifest().slot_count() == parts.health_manifest.slot_count()
    }
}

/// Service-owned coordinator for coherent automation topology replacement.
pub struct AutomationTopologyHandle {
    current: ArcSwap<AutomationTopologyGeneration>,
    point_path: PathBuf,
    health_path: PathBuf,
    command_sink: Arc<ShmDeviceCommandSink>,
    refresh_gate: Arc<Mutex<()>>,
    command_gate: Arc<RwLock<()>>,
    change_sequence: AtomicU64,
    change_tx: watch::Sender<u64>,
}

impl AutomationTopologyHandle {
    /// Loads one canonical SQLite snapshot and creates an offline-first runtime.
    pub async fn from_sqlite_lazy(
        point_path: impl Into<PathBuf>,
        health_path: impl Into<PathBuf>,
        pool: &SqlitePool,
        command_sink: Arc<ShmDeviceCommandSink>,
    ) -> PortResult<Self> {
        let snapshot = aether_sqlite_topology::load_sqlite_live_topology(pool).await?;
        Self::new_lazy(point_path, health_path, snapshot, command_sink)
    }

    /// Creates an offline-first generation from one SQLite transaction.
    ///
    /// No SHM file is opened here. [`Self::refresh`] eagerly validates both
    /// physical planes before replacing this lazy generation.
    pub fn new_lazy(
        point_path: impl Into<PathBuf>,
        health_path: impl Into<PathBuf>,
        snapshot: SqliteLiveTopologySnapshot,
        command_sink: Arc<ShmDeviceCommandSink>,
    ) -> PortResult<Self> {
        let point_path = point_path.into();
        let health_path = health_path.into();
        let parts = CandidateParts::from_snapshot(snapshot);
        let read = Arc::new(ShmReadTopologyGeneration::new_lazy(
            shm_client(&point_path, parts.point_manifest.layout_hash()),
            shm_client(&health_path, parts.health_manifest.layout_hash()),
            Arc::clone(&parts.point_manifest),
            Arc::clone(&parts.health_manifest),
        )?);
        let initial = Arc::new(AutomationTopologyGeneration::compose(read, parts, false, 0));
        let (change_tx, _change_rx) = watch::channel(0);
        Ok(Self {
            current: ArcSwap::new(initial),
            point_path,
            health_path,
            command_sink,
            refresh_gate: Arc::new(Mutex::new(())),
            command_gate: Arc::new(RwLock::new(())),
            change_sequence: AtomicU64::new(0),
            change_tx,
        })
    }

    /// Pins one complete automation topology for a query or command.
    #[must_use]
    pub fn load(&self) -> Arc<AutomationTopologyGeneration> {
        self.current.load_full()
    }

    /// Subscribes to successful logical/physical topology replacements.
    ///
    /// Consumers use this to rebuild PointWatch subscriptions. Same-generation
    /// command-writer recovery does not emit a change.
    #[must_use]
    pub fn subscribe(&self) -> watch::Receiver<u64> {
        self.change_tx.subscribe()
    }

    /// Pins a command generation until the returned view is dropped.
    ///
    /// The retained read guard prevents topology publication from replacing
    /// the command writer after routing has been resolved but before the
    /// physical sink accepts the command.
    pub async fn pin_command(self: &Arc<Self>) -> PinnedAutomationCommandView {
        let guard = Arc::clone(&self.command_gate).read_owned().await;
        let generation = self.load();
        PinnedAutomationCommandView {
            generation,
            _guard: guard,
        }
    }

    /// Revokes commands and retains the refresh lease across one SQLite
    /// action-routing mutation, closing the commit-to-publication window.
    pub async fn begin_action_routing_mutation(self: &Arc<Self>) -> RoutingMutationLease {
        self.begin_routing_mutation(RoutingPlane::Action).await
    }

    /// Revokes C2M before a measurement-route transaction and retains the
    /// refresh lease until the exact committed SQLite snapshot is published.
    pub async fn begin_measurement_routing_mutation(self: &Arc<Self>) -> RoutingMutationLease {
        self.begin_routing_mutation(RoutingPlane::Measurement).await
    }

    async fn begin_routing_mutation(self: &Arc<Self>, plane: RoutingPlane) -> RoutingMutationLease {
        let refresh_guard = Arc::clone(&self.refresh_gate).lock_owned().await;
        let previous_generation = self.load();
        let revoked_generation = match plane {
            RoutingPlane::Measurement => self.revoke_measurement_routes_locked().await,
            RoutingPlane::Action => self.revoke_action_routes_locked().await,
        };
        let restore_generation = (!Arc::ptr_eq(&previous_generation, &revoked_generation))
            .then_some(previous_generation);
        RoutingMutationLease {
            plane,
            topology: Arc::clone(self),
            restore_generation,
            revoked_generation,
            refresh_guard: Some(refresh_guard),
        }
    }

    /// Loads SQLite topology and publishes it only after both SHM planes and
    /// the command writer validate against the same physical manifest.
    ///
    /// A point-only or health-only IO publication returns a retryable conflict
    /// and leaves the current service generation untouched.
    pub async fn refresh(&self, pool: &SqlitePool) -> PortResult<bool> {
        let _refresh = self.refresh_gate.lock().await;
        match self
            .refresh_locked(pool, RefreshMode::ValidatePhysical)
            .await
        {
            Ok(changed) => Ok(changed),
            Err(error) => {
                if error.kind() == PortErrorKind::InvalidData {
                    self.revoke_action_routes_locked().await;
                }
                Err(error)
            },
        }
    }

    /// Refreshes a committed service mutation and revokes commands before
    /// releasing the publication lock if the new view cannot be installed.
    pub async fn refresh_or_revoke_commands(&self, pool: &SqlitePool) -> PortResult<bool> {
        let _refresh = self.refresh_gate.lock().await;
        match self
            .refresh_locked(pool, RefreshMode::PublishConfiguration)
            .await
        {
            Ok(changed) => Ok(changed),
            Err(error) => {
                self.revoke_action_routes_locked().await;
                Err(error)
            },
        }
    }

    async fn refresh_locked(&self, pool: &SqlitePool, mode: RefreshMode) -> PortResult<bool> {
        let snapshot = aether_sqlite_topology::load_sqlite_live_topology(pool).await?;
        let parts = CandidateParts::from_snapshot(snapshot);
        let current = self.current.load_full();
        let physical_changed = !current.has_physical_layout(&parts);
        let logical_changed = current.digest != parts.digest
            || current.measurement_routes_revoked
            || current.action_routes_revoked;
        let physical_current =
            current.physical_validated && current.read.validate_layouts().is_ok();

        if !physical_changed && !logical_changed && mode == RefreshMode::PublishConfiguration {
            return Ok(false);
        }

        if !physical_changed && physical_current && self.command_sink.is_writer_available() {
            if !logical_changed {
                return Ok(false);
            }
            let sequence = self.next_sequence();
            let replacement = Arc::new(AutomationTopologyGeneration::compose(
                Arc::clone(&current.read),
                parts,
                true,
                sequence,
            ));
            let _commands = self.command_gate.write().await;
            self.current.store(replacement);
            self.notify_change(sequence);
            return Ok(true);
        }

        // A routing-only update may be accepted while IO is offline because
        // the physical layout remains identical and commands still fail closed.
        if !physical_changed && logical_changed && !current.physical_validated {
            let sequence = self.next_sequence();
            let replacement = Arc::new(AutomationTopologyGeneration::compose(
                Arc::clone(&current.read),
                parts,
                false,
                sequence,
            ));
            let _commands = self.command_gate.write().await;
            self.current.store(replacement);
            self.notify_change(sequence);
            return Ok(true);
        }

        let point_path = self.point_path.clone();
        let health_path = self.health_path.clone();
        let point_manifest = Arc::clone(&parts.point_manifest);
        let health_manifest = Arc::clone(&parts.health_manifest);
        let read = tokio::task::spawn_blocking(move || {
            ShmReadTopologyGeneration::open(
                shm_client(&point_path, point_manifest.layout_hash()),
                shm_client(&health_path, health_manifest.layout_hash()),
                point_manifest,
                health_manifest,
            )
        })
        .await
        .map_err(topology_validation_task_error)?
        .map_err(physical_publication_conflict)?;
        let changed = logical_changed || physical_changed || !current.physical_validated;
        let sequence = if changed {
            self.next_sequence()
        } else {
            current.sequence
        };
        let replacement = Arc::new(AutomationTopologyGeneration::compose(
            Arc::new(read),
            parts,
            true,
            sequence,
        ));
        let command_manifest = Arc::clone(replacement.point_manifest());
        let _commands = self.command_gate.write().await;
        let publication = replacement
            .read
            .with_validated_authority(|| -> PortResult<()> {
                self.command_sink
                    .open_generation(&self.point_path, command_manifest)?;
                self.current.store(Arc::clone(&replacement));
                Ok(())
            })
            .map_err(physical_publication_conflict)?;
        publication?;

        if changed {
            self.notify_change(sequence);
        }
        Ok(changed)
    }

    async fn revoke_measurement_routes_locked(&self) -> Arc<AutomationTopologyGeneration> {
        let _commands = self.command_gate.write().await;
        let current = self.current.load_full();
        if current.measurement_routes_revoked {
            return current;
        }
        let sequence = self.next_sequence();
        let revoked = Arc::new(AutomationTopologyGeneration {
            read: Arc::clone(&current.read),
            measurement_routes: Arc::new(LogicalPointRoutes::new()),
            action_routes: Arc::clone(&current.action_routes),
            digest: current.digest,
            sequence,
            physical_validated: current.physical_validated,
            measurement_routes_revoked: true,
            action_routes_revoked: current.action_routes_revoked,
        });
        self.current.store(Arc::clone(&revoked));
        self.notify_change(sequence);
        revoked
    }

    async fn revoke_action_routes_locked(&self) -> Arc<AutomationTopologyGeneration> {
        let _commands = self.command_gate.write().await;
        let current = self.current.load_full();
        if current.action_routes_revoked {
            return current;
        }
        let sequence = self.next_sequence();
        let revoked = Arc::new(AutomationTopologyGeneration {
            read: Arc::clone(&current.read),
            measurement_routes: Arc::clone(&current.measurement_routes),
            action_routes: Arc::new(LogicalCommandRoutes::new()),
            digest: current.digest,
            sequence,
            physical_validated: current.physical_validated,
            measurement_routes_revoked: current.measurement_routes_revoked,
            action_routes_revoked: true,
        });
        self.current.store(Arc::clone(&revoked));
        self.notify_change(sequence);
        revoked
    }

    async fn restore_revoked_generation(
        &self,
        previous: Arc<AutomationTopologyGeneration>,
        revoked: &Arc<AutomationTopologyGeneration>,
        plane: RoutingPlane,
    ) {
        let _commands = self.command_gate.write().await;
        let current = self.current.load_full();
        if !Arc::ptr_eq(&current, revoked) {
            tracing::error!(
                plane = plane.as_str(),
                expected_sequence = revoked.sequence(),
                current_sequence = current.sequence(),
                "refusing to restore a logical-routing generation over a concurrent publication"
            );
            return;
        }
        let sequence = previous.sequence();
        self.current.store(previous);
        self.notify_change(sequence);
    }

    fn next_sequence(&self) -> u64 {
        self.change_sequence
            .fetch_add(1, Ordering::Relaxed)
            .wrapping_add(1)
    }

    fn notify_change(&self, sequence: u64) {
        self.change_tx.send_replace(sequence);
    }
}

/// One command's immutable topology plus its publication read lease.
pub struct PinnedAutomationCommandView {
    generation: Arc<AutomationTopologyGeneration>,
    _guard: OwnedRwLockReadGuard<()>,
}

/// Exclusive refresh lease spanning one logical-route mutation and publication.
#[must_use = "the lease must span the SQLite mutation and runtime publication"]
pub struct RoutingMutationLease {
    plane: RoutingPlane,
    topology: Arc<AutomationTopologyHandle>,
    restore_generation: Option<Arc<AutomationTopologyGeneration>>,
    revoked_generation: Arc<AutomationTopologyGeneration>,
    refresh_guard: Option<OwnedMutexGuard<()>>,
}

impl RoutingMutationLease {
    /// Restores the pre-mutation generation after a known-uncommitted failure.
    pub(crate) async fn restore(mut self) {
        self.restore_before_commit().await;
    }

    /// Disarms restoration before a commit with potentially ambiguous outcome.
    pub(crate) fn commit_started(&mut self) {
        self.restore_generation = None;
    }

    /// Publishes the committed SQLite view before releasing the refresh lease.
    pub async fn publish(mut self, pool: &SqlitePool) -> PortResult<bool> {
        self.commit_started();
        self.topology
            .refresh_locked(pool, RefreshMode::PublishConfiguration)
            .await
    }

    async fn restore_before_commit(&mut self) {
        let Some(previous) = self.restore_generation.as_ref().cloned() else {
            return;
        };
        self.topology
            .restore_revoked_generation(previous, &self.revoked_generation, self.plane)
            .await;
        self.restore_generation = None;
    }
}

impl Drop for RoutingMutationLease {
    fn drop(&mut self) {
        let Some(previous) = self.restore_generation.take() else {
            return;
        };
        let revoked = Arc::clone(&self.revoked_generation);
        let topology = Arc::clone(&self.topology);
        let plane = self.plane;
        let Some(refresh_guard) = self.refresh_guard.take() else {
            tracing::error!(
                plane = plane.as_str(),
                "logical-routing mutation lease lost its refresh guard before restoration"
            );
            return;
        };
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            tracing::error!(
                plane = plane.as_str(),
                "cannot schedule logical-routing restoration outside a Tokio runtime"
            );
            return;
        };
        drop(runtime.spawn(async move {
            topology
                .restore_revoked_generation(previous, &revoked, plane)
                .await;
            drop(refresh_guard);
        }));
    }
}

impl PinnedAutomationCommandView {
    /// Returns the immutable generation held for this command transaction.
    #[must_use]
    pub fn generation(&self) -> &AutomationTopologyGeneration {
        &self.generation
    }
}

fn shm_client(path: &Path, layout_hash: u64) -> ShmClientConfig {
    ShmClientConfig::new(path, layout_hash).with_writer_stale_after(WRITER_STALE_AFTER)
}

fn physical_publication_conflict(error: PortError) -> PortError {
    match error.kind() {
        PortErrorKind::InvalidData => PortError::new(
            PortErrorKind::Conflict,
            format!("IO has not published a coherent point/health topology yet: {error}"),
        ),
        _ => error,
    }
}

fn topology_validation_task_error(error: tokio::task::JoinError) -> PortError {
    if error.is_cancelled() {
        return PortError::new(
            PortErrorKind::Unavailable,
            format!("automation topology validation task was cancelled: {error}"),
        );
    }
    PortError::new(
        PortErrorKind::Permanent,
        format!("automation topology validation task panicked: {error}"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn physical_publication_mapping_preserves_permanent_failures() {
        let error = PortError::new(PortErrorKind::Permanent, "SHM permission denied");

        assert_eq!(physical_publication_conflict(error.clone()), error);
    }

    #[test]
    fn physical_publication_mapping_reclassifies_layout_transition() {
        let error = PortError::new(PortErrorKind::InvalidData, "layout is incomplete");

        assert_eq!(
            physical_publication_conflict(error).kind(),
            PortErrorKind::Conflict
        );
    }

    #[tokio::test]
    async fn panicked_topology_validation_task_is_permanent() {
        let error = tokio::task::spawn_blocking(|| panic!("validation panic"))
            .await
            .expect_err("validation task must panic");

        assert_eq!(
            topology_validation_task_error(error).kind(),
            PortErrorKind::Permanent
        );
    }
}
