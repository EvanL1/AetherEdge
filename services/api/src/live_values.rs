//! WebSocket live-state reads from SHM.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use aether_domain::{ChannelId, PointAddress, PointKind, PointQuality, PointSample, TimestampMs};
#[cfg(test)]
use aether_ports::ChannelHealthObservation;
use aether_ports::{ChannelHealthSource, LiveState, PortError, PortErrorKind, PortResult};
use aether_shm_bridge::{
    ChannelPointManifest, PhysicalPointAddress, PointWatchEvent, ShmClientConfig,
    ShmReadTopologyGeneration, SlotSnapshot, SlotSource,
};
use aether_store_local::{SqliteLiveTopologySnapshot, load_sqlite_live_topology};
use anyhow::Context;
use arc_swap::ArcSwap;
use async_trait::async_trait;
use sqlx::SqlitePool;
use tokio_util::sync::CancellationToken;

use crate::config::GatewayConfig;

/// One physical channel point referenced by instance routing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ChannelPointRef {
    channel_id: u32,
    kind: PointKind,
    point_id: u32,
}

impl ChannelPointRef {
    /// Creates a channel point reference.
    #[must_use]
    pub const fn new(channel_id: u32, kind: PointKind, point_id: u32) -> Self {
        Self {
            channel_id,
            kind,
            point_id,
        }
    }
}

/// Instance point routing snapshot loaded from embedded configuration.
#[derive(Debug, Clone, Default)]
pub struct GatewayRouting {
    measurements: HashMap<(u32, u32), ChannelPointRef>,
    actions: HashMap<(u32, u32), ChannelPointRef>,
}

impl GatewayRouting {
    /// Builds routing from `(instance_id, point_id, physical_target)` entries.
    #[must_use]
    pub fn from_entries(
        measurements: impl IntoIterator<Item = (u32, u32, ChannelPointRef)>,
        actions: impl IntoIterator<Item = (u32, u32, ChannelPointRef)>,
    ) -> Self {
        Self {
            measurements: measurements
                .into_iter()
                .map(|(instance_id, point_id, target)| ((instance_id, point_id), target))
                .collect(),
            actions: actions
                .into_iter()
                .map(|(instance_id, point_id, target)| ((instance_id, point_id), target))
                .collect(),
        }
    }

    fn point(&self, instance_id: u32, data_type: &str, point_id: u32) -> Option<ChannelPointRef> {
        match data_type {
            "M" => self.measurements.get(&(instance_id, point_id)).copied(),
            "A" => self.actions.get(&(instance_id, point_id)).copied(),
            _ => None,
        }
    }

    fn points(
        &self,
        instance_id: u32,
        data_type: &str,
    ) -> impl Iterator<Item = (u32, ChannelPointRef)> + '_ {
        let routes = match data_type {
            "M" => Some(&self.measurements),
            "A" => Some(&self.actions),
            _ => None,
        };
        routes.into_iter().flat_map(move |routes| {
            routes
                .iter()
                .filter_map(move |(&(owner_id, point_id), &target)| {
                    (owner_id == instance_id).then_some((point_id, target))
                })
        })
    }
}

/// Test-only empty health source.
#[cfg(test)]
#[derive(Debug, Clone, Copy, Default)]
pub struct NoChannelHealth;

#[cfg(test)]
impl ChannelHealthSource for NoChannelHealth {
    fn read_channel(&self, _channel_id: ChannelId) -> PortResult<Option<ChannelHealthObservation>> {
        Ok(None)
    }
}

/// Current-value capability consumed by WebSocket transports.
pub trait GatewayValueSource: Send + Sync + 'static {
    /// Reads every current point in one logical subscription group.
    fn read_group(
        &self,
        source: &str,
        owner_id: i64,
        data_type: &str,
    ) -> PortResult<BTreeMap<String, SlotSnapshot>>;

    /// Reads one logical homepage formula such as `inst:42:M:7`.
    fn read_formula(&self, formula: &str) -> PortResult<Option<SlotSnapshot>>;

    /// Resolves all main-data-plane slots covered by a subscription.
    fn watched_slots(
        &self,
        source: &str,
        owner_ids: &[i64],
        data_types: &[String],
    ) -> PortResult<BTreeSet<usize>>;

    /// Resolves one homepage formula to its main-plane event slot.
    /// Channel-health formulas return `None` because they use a separate SHM.
    fn watched_formula_slot(&self, formula: &str) -> PortResult<Option<usize>>;

    /// Validates a PointWatch hint against the pinned current manifest.
    fn validated_point_watch_slot(&self, _event: PointWatchEvent) -> Option<usize> {
        None
    }
}

#[derive(Debug, Clone, Copy)]
enum FormulaTarget {
    Main(ChannelPointRef),
    ChannelHealth(u32),
}

/// SHM-backed implementation of gateway current-value reads.
pub struct ShmGatewayValueSource {
    current: ArcSwap<GatewayValueGeneration>,
}

struct GatewayValueGeneration {
    slots: Arc<dyn SlotSource>,
    manifest: Arc<ChannelPointManifest>,
    routing: Arc<GatewayRouting>,
    channel_health: Arc<dyn ChannelHealthSource>,
    topology: Option<Arc<ShmReadTopologyGeneration>>,
    digest: u64,
}

impl ShmGatewayValueSource {
    /// Creates the source from independently testable capabilities.
    #[must_use]
    #[cfg(test)]
    pub fn new<S, H>(
        slots: Arc<S>,
        manifest: Arc<ChannelPointManifest>,
        routing: Arc<GatewayRouting>,
        channel_health: Arc<H>,
    ) -> Self
    where
        S: SlotSource,
        H: ChannelHealthSource,
    {
        Self {
            current: ArcSwap::from_pointee(GatewayValueGeneration {
                slots,
                manifest,
                routing,
                channel_health,
                topology: None,
                digest: 0,
            }),
        }
    }

    /// Reloads a complete SQLite snapshot and publishes it only after both SHM
    /// planes validate against that same snapshot.
    pub async fn refresh_topology(
        &self,
        pool: &SqlitePool,
        config: &GatewayConfig,
    ) -> PortResult<bool> {
        let snapshot = load_sqlite_live_topology(pool).await?;
        let current = self.current.load_full();
        let physical_current = current
            .topology
            .as_ref()
            .is_some_and(|topology| topology.validate_layouts().is_ok());
        if current.digest == snapshot.digest() && physical_current {
            return Ok(false);
        }

        let physical_layout_unchanged = current.topology.as_ref().is_some_and(|topology| {
            topology.point_manifest().layout_hash() == snapshot.point_manifest().layout_hash()
                && topology.point_manifest().slot_count() == snapshot.point_manifest().slot_count()
                && topology.health_manifest().layout_hash()
                    == snapshot.health_manifest().layout_hash()
                && topology.health_manifest().slot_count()
                    == snapshot.health_manifest().slot_count()
        });
        let lazy_route_only = current.digest != snapshot.digest()
            && current
                .topology
                .as_ref()
                .is_some_and(|topology| topology.publication_epoch() == 0);
        if physical_layout_unchanged && (physical_current || lazy_route_only) {
            let topology = Arc::clone(current.topology.as_ref().ok_or_else(|| {
                PortError::new(
                    PortErrorKind::Permanent,
                    "validated API generation is missing its SHM topology",
                )
            })?);
            let replacement = Arc::new(GatewayValueGeneration {
                slots: Arc::clone(&current.slots),
                manifest: Arc::clone(&current.manifest),
                routing: gateway_routing(&snapshot),
                channel_health: Arc::clone(&current.channel_health),
                topology: Some(Arc::clone(&topology)),
                digest: snapshot.digest(),
            });
            if physical_current {
                topology.with_validated_authority(|| self.current.store(replacement))?;
            } else {
                self.current.store(replacement);
            }
            return Ok(true);
        }

        let owned_config = config.clone();
        let candidate = tokio::task::spawn_blocking(move || {
            build_gateway_generation(snapshot, &owned_config, true)
        })
        .await
        .map_err(|error| {
            PortError::new(
                PortErrorKind::Unavailable,
                format!("API topology validation task failed: {error}"),
            )
        })??;
        let candidate = Arc::new(candidate);
        let topology = Arc::clone(candidate.topology.as_ref().ok_or_else(|| {
            PortError::new(
                PortErrorKind::Permanent,
                "production gateway generation has no physical topology",
            )
        })?);
        topology.with_validated_authority(|| self.current.store(candidate))?;
        Ok(true)
    }

    /// Returns whether an event still names the current typed physical slot.
    #[must_use]
    pub fn accepts_point_watch_event(&self, event: PointWatchEvent) -> bool {
        let generation = self.current.load();
        event.matches_manifest(&generation.manifest)
    }
}

impl GatewayValueGeneration {
    fn read_point(&self, target: ChannelPointRef) -> PortResult<Option<SlotSnapshot>> {
        let Some(slot) = self
            .manifest
            .slot_for(PhysicalPointAddress::from_legacy_raw(
                target.channel_id,
                target.kind,
                target.point_id,
            ))
        else {
            return Ok(None);
        };
        let Some(sample) = self.slots.read_slot(slot)? else {
            return Ok(None);
        };
        if sample.value().is_nan() {
            return Ok(None);
        }
        if !sample.value().is_finite() {
            return Err(PortError::new(
                PortErrorKind::InvalidData,
                format!("SHM slot {slot} contains a non-finite gateway value"),
            ));
        }
        Ok(Some(sample))
    }

    fn channel_targets(
        &self,
        channel_id: u32,
        data_type: &str,
    ) -> PortResult<Vec<(u32, ChannelPointRef)>> {
        let kind = parse_channel_kind(data_type)
            .ok_or_else(|| invalid_target(format!("unsupported channel data type {data_type}")))?;
        let type_index = kind_index(kind);
        let count = self
            .manifest
            .counts()
            .get(&channel_id)
            .map_or(0, |counts| counts[type_index]);
        Ok((0..count)
            .map(|point_id| (point_id, ChannelPointRef::new(channel_id, kind, point_id)))
            .collect())
    }

    fn formula_target(&self, formula: &str) -> PortResult<Option<FormulaTarget>> {
        let parts = formula.split(':').collect::<Vec<_>>();
        if let ["io", "online", channel_id] = parts.as_slice() {
            return Ok(Some(FormulaTarget::ChannelHealth(parse_u32(
                channel_id,
                "formula channel id",
            )?)));
        }
        let [source, owner_id, data_type, point_id] = parts.as_slice() else {
            return Err(invalid_target(format!(
                "invalid homepage formula {formula}"
            )));
        };
        let owner_id = parse_u32(owner_id, "formula owner id")?;
        let point_id = parse_u32(point_id, "formula point id")?;
        let target = match *source {
            "io" => ChannelPointRef::new(
                owner_id,
                parse_channel_kind(data_type).ok_or_else(|| {
                    invalid_target(format!("invalid formula data type {data_type}"))
                })?,
                point_id,
            ),
            "inst" => {
                let Some(target) = self.routing.point(owner_id, data_type, point_id) else {
                    return Ok(None);
                };
                target
            },
            _ => return Err(invalid_target(format!("invalid formula source {source}"))),
        };
        Ok(Some(FormulaTarget::Main(target)))
    }
}

impl GatewayValueSource for ShmGatewayValueSource {
    fn read_group(
        &self,
        source: &str,
        owner_id: i64,
        data_type: &str,
    ) -> PortResult<BTreeMap<String, SlotSnapshot>> {
        let generation = self.current.load();
        let owner_id = checked_u32(owner_id, "subscription owner id")?;
        if source == "io" && data_type == "online" {
            return Ok(generation
                .channel_health
                .read_channel(ChannelId::new(owner_id))?
                .map(|sample| {
                    BTreeMap::from([(
                        owner_id.to_string(),
                        SlotSnapshot::new(
                            if sample.online() { 1.0 } else { 0.0 },
                            sample.timestamp_ms(),
                        ),
                    )])
                })
                .unwrap_or_default());
        }

        let targets = match source {
            "io" => generation.channel_targets(owner_id, data_type)?,
            "inst" if matches!(data_type, "M" | "A") => {
                generation.routing.points(owner_id, data_type).collect()
            },
            _ => {
                return Err(invalid_target(format!(
                    "unsupported group {source}:{data_type}"
                )));
            },
        };
        let mut values = BTreeMap::new();
        for (point_id, target) in targets {
            if let Some(sample) = generation.read_point(target)? {
                values.insert(point_id.to_string(), sample);
            }
        }
        Ok(values)
    }

    fn read_formula(&self, formula: &str) -> PortResult<Option<SlotSnapshot>> {
        let generation = self.current.load();
        let Some(target) = generation.formula_target(formula)? else {
            return Ok(None);
        };
        match target {
            FormulaTarget::ChannelHealth(channel_id) => Ok(generation
                .channel_health
                .read_channel(ChannelId::new(channel_id))?
                .map(|sample| {
                    SlotSnapshot::new(
                        if sample.online() { 1.0 } else { 0.0 },
                        sample.timestamp_ms(),
                    )
                })),
            FormulaTarget::Main(target) => generation.read_point(target),
        }
    }

    fn watched_slots(
        &self,
        source: &str,
        owner_ids: &[i64],
        data_types: &[String],
    ) -> PortResult<BTreeSet<usize>> {
        let generation = self.current.load();
        let mut slots = BTreeSet::new();
        for &owner_id in owner_ids {
            let owner_id = checked_u32(owner_id, "subscription owner id")?;
            for data_type in data_types {
                let targets: Vec<_> = match source {
                    "io" if data_type == "online" => Vec::new(),
                    "io" => generation.channel_targets(owner_id, data_type)?,
                    "inst" if matches!(data_type.as_str(), "M" | "A") => {
                        generation.routing.points(owner_id, data_type).collect()
                    },
                    _ => continue,
                };
                for (_, target) in targets {
                    if let Some(slot) =
                        generation
                            .manifest
                            .slot_for(PhysicalPointAddress::from_legacy_raw(
                                target.channel_id,
                                target.kind,
                                target.point_id,
                            ))
                    {
                        slots.insert(slot);
                    }
                }
            }
        }
        Ok(slots)
    }

    fn watched_formula_slot(&self, formula: &str) -> PortResult<Option<usize>> {
        let generation = self.current.load();
        Ok(match generation.formula_target(formula)? {
            Some(FormulaTarget::Main(target)) => {
                generation
                    .manifest
                    .slot_for(PhysicalPointAddress::from_legacy_raw(
                        target.channel_id,
                        target.kind,
                        target.point_id,
                    ))
            },
            Some(FormulaTarget::ChannelHealth(_)) | None => None,
        })
    }

    fn validated_point_watch_slot(&self, event: PointWatchEvent) -> Option<usize> {
        self.accepts_point_watch_event(event)
            .then(|| usize::try_from(event.slot_index()).ok())
            .flatten()
    }
}

fn parse_channel_kind(code: &str) -> Option<PointKind> {
    match code {
        "T" => Some(PointKind::Telemetry),
        "S" => Some(PointKind::Status),
        "C" => Some(PointKind::Command),
        "A" => Some(PointKind::Action),
        _ => None,
    }
}

const fn kind_index(kind: PointKind) -> usize {
    match kind {
        PointKind::Telemetry => 0,
        PointKind::Status => 1,
        PointKind::Command => 2,
        PointKind::Action => 3,
    }
}

fn checked_u32(value: i64, label: &str) -> PortResult<u32> {
    u32::try_from(value)
        .map_err(|_| invalid_target(format!("{label} must fit in u32, got {value}")))
}

fn parse_u32(value: &str, label: &str) -> PortResult<u32> {
    value
        .parse::<u32>()
        .map_err(|_| invalid_target(format!("{label} must be a u32, got {value}")))
}

fn invalid_target(message: impl Into<String>) -> PortError {
    PortError::new(PortErrorKind::Permanent, message)
}

/// Builds a lazy production reader from embedded SQLite configuration.
pub async fn build_gateway_value_source(
    pool: &SqlitePool,
    config: &GatewayConfig,
) -> anyhow::Result<Arc<ShmGatewayValueSource>> {
    let snapshot = load_sqlite_live_topology(pool)
        .await
        .context("load canonical live topology for api")?;
    let generation = build_gateway_generation(snapshot, config, false)?;
    Ok(Arc::new(ShmGatewayValueSource {
        current: ArcSwap::from_pointee(generation),
    }))
}

/// Periodically reloads the embedded topology and retains the last validated
/// service generation while IO is between its point and health publications.
pub async fn run_gateway_topology_refresh(
    source: Arc<ShmGatewayValueSource>,
    pool: SqlitePool,
    config: GatewayConfig,
    shutdown: CancellationToken,
) {
    let mut interval = tokio::time::interval(Duration::from_millis(
        config.shm_topology_refresh_interval_ms.max(100),
    ));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            _ = shutdown.cancelled() => break,
            _ = interval.tick() => match source.refresh_topology(&pool, &config).await {
                Ok(true) => tracing::info!("API live topology generation refreshed"),
                Ok(false) => {},
                Err(error) if error.is_retryable() => {
                    tracing::warn!("API live topology refresh deferred: {error}");
                },
                Err(error) => tracing::error!("API live topology refresh rejected: {error}"),
            },
        }
    }
}

fn build_gateway_generation(
    snapshot: SqliteLiveTopologySnapshot,
    config: &GatewayConfig,
    validate_physical: bool,
) -> PortResult<GatewayValueGeneration> {
    let digest = snapshot.digest();
    let routing = gateway_routing(&snapshot);
    let (point_manifest, health_manifest, _, _) = snapshot.into_parts();
    let point_manifest = Arc::new(point_manifest);
    let health_manifest = Arc::new(health_manifest);
    let point_config = ShmClientConfig::new(&config.shm_path, point_manifest.layout_hash())
        .with_writer_stale_after(Duration::from_millis(config.shm_writer_stale_after_ms))
        .with_identity_check_interval(Duration::from_millis(config.shm_identity_check_interval_ms));
    let health_config = ShmClientConfig::new(
        &config.channel_health_shm_path,
        health_manifest.layout_hash(),
    )
    .with_writer_stale_after(Duration::from_millis(config.shm_writer_stale_after_ms))
    .with_identity_check_interval(Duration::from_millis(config.shm_identity_check_interval_ms));
    let topology = Arc::new(if validate_physical {
        ShmReadTopologyGeneration::open(
            point_config,
            health_config,
            point_manifest,
            health_manifest,
        )?
    } else {
        ShmReadTopologyGeneration::new_lazy(
            point_config,
            health_config,
            point_manifest,
            health_manifest,
        )?
    });
    let slots: Arc<dyn SlotSource> = topology.point_source().clone();
    let channel_health: Arc<dyn ChannelHealthSource> = topology.channel_health().clone();
    Ok(GatewayValueGeneration {
        slots,
        manifest: Arc::clone(topology.point_manifest()),
        routing,
        channel_health,
        topology: Some(topology),
        digest,
    })
}

fn gateway_routing(snapshot: &SqliteLiveTopologySnapshot) -> Arc<GatewayRouting> {
    Arc::new(GatewayRouting::from_entries(
        snapshot
            .measurement_routes()
            .map(|(instance_id, point_id, target)| {
                (instance_id, point_id, channel_point_ref(target))
            }),
        snapshot
            .action_routes()
            .map(|(instance_id, point_id, target)| {
                (instance_id, point_id, channel_point_ref(target))
            }),
    ))
}

const fn channel_point_ref(target: PhysicalPointAddress) -> ChannelPointRef {
    ChannelPointRef::new(
        target.channel_id().get(),
        target.kind(),
        target.point_id().get(),
    )
}

/// Builds the read-only `LiveState` used by an enabled Data Processing route.
///
/// Only explicitly commissioned logical point addresses are resolved. Reads
/// pin the gateway's current physical/routing generation and therefore follow
/// validated topology refreshes without receiving any writer capability.
pub fn build_data_processing_live_state(
    source: Arc<ShmGatewayValueSource>,
    addresses: &[PointAddress],
) -> anyhow::Result<Arc<dyn LiveState>> {
    let generation = source.current.load_full();
    let mut commissioned = HashSet::with_capacity(addresses.len());
    let mut physical_targets = HashSet::with_capacity(addresses.len());
    for address in addresses {
        if address.kind().is_writable() {
            anyhow::bail!("Data Processing live-state mapping targets a writable point");
        }
        let target = generation
            .routing
            .point(address.instance_id().get(), "M", address.point_id().get())
            .with_context(|| format!("no enabled measurement route for {address:?}"))?;
        if target.kind != address.kind() {
            anyhow::bail!("commissioned logical point kind does not match its physical route");
        }
        generation
            .manifest
            .slot_for(PhysicalPointAddress::from_legacy_raw(
                target.channel_id,
                target.kind,
                target.point_id,
            ))
            .with_context(|| format!("no SHM slot for commissioned point {address:?}"))?;
        if !physical_targets.insert(target) {
            anyhow::bail!("multiple commissioned logical points resolve to one SHM slot");
        }
        commissioned.insert(*address);
    }
    Ok(Arc::new(GatewayCommissionedLiveState {
        source,
        commissioned,
    }))
}

struct GatewayCommissionedLiveState {
    source: Arc<ShmGatewayValueSource>,
    commissioned: HashSet<PointAddress>,
}

impl GatewayCommissionedLiveState {
    fn resolve_target(
        &self,
        generation: &GatewayValueGeneration,
        address: PointAddress,
    ) -> PortResult<ChannelPointRef> {
        if !self.commissioned.contains(&address) {
            return Err(PortError::new(
                PortErrorKind::NotFound,
                format!("point {address:?} is not commissioned for Data Processing"),
            ));
        }
        let target = generation
            .routing
            .point(address.instance_id().get(), "M", address.point_id().get())
            .ok_or_else(|| {
                PortError::new(
                    PortErrorKind::NotFound,
                    format!("no current measurement route for {address:?}"),
                )
            })?;
        if target.kind != address.kind() {
            return Err(PortError::new(
                PortErrorKind::InvalidData,
                format!("current physical route kind changed for {address:?}"),
            ));
        }
        if generation
            .manifest
            .slot_for(PhysicalPointAddress::from_legacy_raw(
                target.channel_id,
                target.kind,
                target.point_id,
            ))
            .is_none()
        {
            return Err(PortError::new(
                PortErrorKind::InvalidData,
                format!("current physical route is absent from SHM for {address:?}"),
            ));
        }
        Ok(target)
    }

    fn read_from(
        &self,
        generation: &GatewayValueGeneration,
        address: PointAddress,
    ) -> PortResult<Option<PointSample>> {
        let target = self.resolve_target(generation, address)?;
        Ok(generation.read_point(target)?.map(|sample| {
            PointSample::new(
                address,
                sample.value(),
                TimestampMs::new(sample.timestamp_ms()),
                PointQuality::Good,
            )
        }))
    }
}

#[async_trait]
impl LiveState for GatewayCommissionedLiveState {
    async fn read(&self, address: PointAddress) -> PortResult<Option<PointSample>> {
        let generation = self.source.current.load_full();
        self.read_from(&generation, address)
    }

    async fn read_many(&self, addresses: &[PointAddress]) -> PortResult<Vec<Option<PointSample>>> {
        let generation = self.source.current.load_full();
        let mut physical_owners = HashMap::with_capacity(addresses.len());
        for &address in addresses {
            let target = self.resolve_target(&generation, address)?;
            if physical_owners
                .insert(target, address)
                .is_some_and(|existing| existing != address)
            {
                return Err(PortError::new(
                    PortErrorKind::InvalidData,
                    "multiple commissioned Data Processing points now resolve to one SHM slot",
                ));
            }
        }
        addresses
            .iter()
            .copied()
            .map(|address| self.read_from(&generation, address))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Arc;

    use aether_domain::PointKind;
    use aether_ports::{PortError, PortErrorKind, PortResult};
    use aether_shm_bridge::{ChannelPointManifest, PhysicalPointAddress, SlotSnapshot, SlotSource};

    use super::{
        ChannelPointRef, GatewayRouting, GatewayValueSource, NoChannelHealth, ShmGatewayValueSource,
    };

    struct StubSlots {
        slot_count: usize,
        values: HashMap<usize, SlotSnapshot>,
    }

    impl SlotSource for StubSlots {
        fn slot_count(&self) -> PortResult<usize> {
            Ok(self.slot_count)
        }

        fn read_slot(&self, index: usize) -> PortResult<Option<SlotSnapshot>> {
            if index >= self.slot_count {
                return Err(PortError::new(
                    PortErrorKind::InvalidData,
                    "slot outside test source",
                ));
            }
            Ok(self.values.get(&index).copied())
        }
    }

    fn source() -> ShmGatewayValueSource {
        let manifest = ChannelPointManifest::from_entries([(10, [2, 1, 0, 1])]);
        let points = [
            (PointKind::Telemetry, 0, 10.0),
            (PointKind::Telemetry, 1, 11.0),
            (PointKind::Action, 0, 20.0),
        ];
        let values = points
            .into_iter()
            .map(|(kind, point_id, value)| {
                (
                    manifest
                        .slot_for(PhysicalPointAddress::from_legacy_raw(10, kind, point_id))
                        .expect("configured slot"),
                    SlotSnapshot::new(value, 1_000 + u64::from(point_id)),
                )
            })
            .collect();
        let routing = GatewayRouting::from_entries(
            [(100, 5, ChannelPointRef::new(10, PointKind::Telemetry, 1))],
            [(100, 8, ChannelPointRef::new(10, PointKind::Action, 0))],
        );
        ShmGatewayValueSource::new(
            Arc::new(StubSlots {
                slot_count: manifest.slot_count(),
                values,
            }),
            Arc::new(manifest),
            Arc::new(routing),
            Arc::new(NoChannelHealth),
        )
    }

    #[test]
    fn channel_and_instance_groups_are_enumerated_from_manifests() {
        let source = source();

        let channel = source
            .read_group("io", 10, "T")
            .expect("read channel group");
        let instance = source
            .read_group("inst", 100, "M")
            .expect("read instance group");

        assert_eq!(channel.get("0").map(|sample| sample.value()), Some(10.0));
        assert_eq!(channel.get("1").map(|sample| sample.value()), Some(11.0));
        assert_eq!(instance.get("5").map(|sample| sample.value()), Some(11.0));
    }

    #[test]
    fn homepage_formula_resolves_to_shm() {
        let source = source();

        let sample = source
            .read_formula("inst:100:M:5")
            .expect("read formula")
            .expect("formula sample");

        assert_eq!(sample.value(), 11.0);
    }

    #[test]
    fn watched_slots_cover_group_subscriptions() {
        let slots = source()
            .watched_slots("io", &[10], &["T".to_string()])
            .expect("resolve watched slots");

        assert_eq!(slots.len(), 2);
    }

    #[test]
    fn homepage_formula_resolves_to_its_event_slot() {
        let source = source();
        let slot = source
            .watched_formula_slot("inst:100:M:5")
            .expect("resolve formula slot")
            .expect("main-plane formula");

        assert_eq!(slot, 1);
        assert_eq!(
            source
                .watched_formula_slot("io:online:10")
                .expect("health formula"),
            None
        );
    }

    #[tokio::test]
    async fn production_source_builds_before_writer_and_without_external_databases() {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("open embedded config database");
        for statement in [
            "CREATE TABLE channels (channel_id INTEGER PRIMARY KEY, protocol TEXT NOT NULL)",
            "CREATE TABLE telemetry_points (channel_id INTEGER, point_id INTEGER)",
            "CREATE TABLE signal_points (channel_id INTEGER, point_id INTEGER)",
            "CREATE TABLE control_points (channel_id INTEGER, point_id INTEGER)",
            "CREATE TABLE adjustment_points (channel_id INTEGER, point_id INTEGER)",
            "CREATE TABLE measurement_routing (instance_id INTEGER, channel_id INTEGER, channel_type TEXT, channel_point_id INTEGER, measurement_id INTEGER, enabled BOOLEAN)",
            "CREATE TABLE action_routing (instance_id INTEGER, channel_id INTEGER, channel_type TEXT, channel_point_id INTEGER, action_id INTEGER, enabled BOOLEAN)",
            "INSERT INTO channels VALUES (10, 'modbus')",
            "INSERT INTO telemetry_points VALUES (10, 0)",
            "INSERT INTO telemetry_points VALUES (10, 1)",
            "INSERT INTO measurement_routing VALUES (100, 10, 'T', 0, 5, TRUE)",
        ] {
            sqlx::query(statement)
                .execute(&pool)
                .await
                .expect("create minimal gateway config");
        }
        let unique = format!(
            "aether-api-missing-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system clock")
                .as_nanos()
        );
        let config = crate::config::GatewayConfig {
            shm_path: std::env::temp_dir()
                .join(format!("{unique}.shm"))
                .to_string_lossy()
                .into_owned(),
            channel_health_shm_path: std::env::temp_dir()
                .join(format!("{unique}-health.shm"))
                .to_string_lossy()
                .into_owned(),
            ..Default::default()
        };

        let source = super::build_gateway_value_source(&pool, &config)
            .await
            .expect("build before an active SHM writer is available");
        assert_eq!(
            source
                .watched_formula_slot("inst:100:M:5")
                .expect("resolve initial route"),
            Some(0)
        );
        sqlx::query(
            "UPDATE measurement_routing SET channel_point_id = 1 \
             WHERE instance_id = 100 AND measurement_id = 5",
        )
        .execute(&pool)
        .await
        .expect("change routing only");
        assert!(
            source
                .refresh_topology(&pool, &config)
                .await
                .expect("routing-only refresh does not require a writer")
        );
        assert_eq!(
            source
                .watched_formula_slot("inst:100:M:5")
                .expect("resolve refreshed route"),
            Some(1)
        );
        let error = source
            .read_group("io", 10, "T")
            .expect_err("missing writer is a retryable read-time condition");

        assert!(error.is_retryable());
    }

    #[tokio::test]
    async fn production_source_refreshes_point_health_and_routing_as_one_view() {
        use aether_domain::{
            AcquiredPointSample, ChannelId, ChannelPointAddress, InstanceId, PointAddress, PointId,
            PointQuality, TimestampMs,
        };
        use aether_shm_bridge::{
            PointWatchEvent, ShmChannelHealthWriterHandle, ShmRuntimeConfig, ShmWriterHandle,
            begin_topology_publication,
        };

        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("open embedded topology database");
        for statement in [
            "CREATE TABLE channels (channel_id INTEGER PRIMARY KEY, protocol TEXT NOT NULL)",
            "CREATE TABLE telemetry_points (channel_id INTEGER, point_id INTEGER)",
            "CREATE TABLE signal_points (channel_id INTEGER, point_id INTEGER)",
            "CREATE TABLE control_points (channel_id INTEGER, point_id INTEGER)",
            "CREATE TABLE adjustment_points (channel_id INTEGER, point_id INTEGER)",
            "CREATE TABLE measurement_routing (instance_id INTEGER, channel_id INTEGER, channel_type TEXT, channel_point_id INTEGER, measurement_id INTEGER, enabled BOOLEAN)",
            "CREATE TABLE action_routing (instance_id INTEGER, channel_id INTEGER, channel_type TEXT, channel_point_id INTEGER, action_id INTEGER, enabled BOOLEAN)",
            "INSERT INTO channels VALUES (10, 'virtual')",
            "INSERT INTO telemetry_points VALUES (10, 0)",
            "INSERT INTO telemetry_points VALUES (10, 1)",
            "INSERT INTO measurement_routing VALUES (100, 10, 'T', 0, 5, TRUE)",
            "INSERT INTO measurement_routing VALUES (101, 10, 'T', 1, 6, TRUE)",
        ] {
            sqlx::query(statement)
                .execute(&pool)
                .await
                .expect("create initial topology");
        }
        let directory = tempfile::tempdir().expect("temporary SHM directory");
        let point_path = directory.path().join("live.shm");
        let health_path = directory.path().join("health.shm");
        let config = crate::config::GatewayConfig {
            shm_path: point_path.to_string_lossy().into_owned(),
            channel_health_shm_path: health_path.to_string_lossy().into_owned(),
            ..Default::default()
        };
        let first = aether_store_local::load_sqlite_live_topology(&pool)
            .await
            .expect("initial topology snapshot");
        let first_epoch = 100;
        let first_publication =
            begin_topology_publication(&point_path).expect("begin initial publication");
        let point_writer = ShmWriterHandle::create_published_at_epoch(
            ShmRuntimeConfig::new(&point_path, 32),
            Arc::new(first.point_manifest().clone()),
            None,
            first_epoch,
        )
        .expect("publish initial point plane");
        let health_writer = ShmChannelHealthWriterHandle::create_at_epoch(
            &health_path,
            Arc::new(first.health_manifest().clone()),
            first_epoch,
        )
        .expect("publish initial health plane");
        first_publication
            .commit(&health_path, first_epoch)
            .expect("commit initial publication");
        let first_timestamp = TimestampMs::new(aether_shm_bridge::timestamp_ms());
        let first_sample = AcquiredPointSample::new(
            ChannelPointAddress::new(ChannelId::new(10), PointKind::Telemetry, PointId::new(0))
                .expect("initial address"),
            10.0,
            10.0,
            first_timestamp,
            PointQuality::Good,
        )
        .expect("initial sample");
        point_writer
            .generation()
            .expect("initial point generation")
            .acquisition_writer()
            .commit_batch(&[first_sample])
            .expect("write initial point");
        health_writer
            .update_heartbeat(aether_shm_bridge::timestamp_ms())
            .expect("initial health heartbeat");

        let source = super::build_gateway_value_source(&pool, &config)
            .await
            .expect("build live source");
        source
            .refresh_topology(&pool, &config)
            .await
            .expect("pin initial committed publication");
        let commissioned_address =
            PointAddress::new(InstanceId::new(100), PointKind::Telemetry, PointId::new(5));
        let second_commissioned_address =
            PointAddress::new(InstanceId::new(101), PointKind::Telemetry, PointId::new(6));
        let data_processing = super::build_data_processing_live_state(
            Arc::clone(&source),
            &[commissioned_address, second_commissioned_address],
        )
        .expect("commission Data Processing live state");
        assert_eq!(
            source
                .read_formula("inst:100:M:5")
                .expect("read initial route")
                .expect("initial value")
                .value(),
            10.0
        );
        assert_eq!(
            data_processing
                .read(commissioned_address)
                .await
                .expect("read initial commissioned point")
                .expect("initial commissioned sample")
                .value(),
            10.0
        );
        let old_event = PointWatchEvent::new(10, PointKind::Telemetry, 0, 0, 10.0, 10.0, 1_000, 1);
        assert!(source.accepts_point_watch_event(old_event));

        for statement in [
            "INSERT INTO channels VALUES (5, 'virtual')",
            "INSERT INTO telemetry_points VALUES (5, 0)",
            "UPDATE measurement_routing SET channel_id = 5 WHERE instance_id = 100 AND measurement_id = 5",
            "UPDATE measurement_routing SET channel_id = 5, channel_point_id = 0 WHERE instance_id = 101 AND measurement_id = 6",
        ] {
            sqlx::query(statement)
                .execute(&pool)
                .await
                .expect("change desired topology");
        }
        let second = aether_store_local::load_sqlite_live_topology(&pool)
            .await
            .expect("replacement topology snapshot");
        let second_epoch = 102;
        let partial_publication =
            begin_topology_publication(&point_path).expect("begin replacement publication");
        point_writer
            .rebuild_for_publication(Arc::new(second.point_manifest().clone()), second_epoch)
            .expect("publish replacement point plane");
        drop(partial_publication);
        let partial_error = source
            .refresh_topology(&pool, &config)
            .await
            .expect_err("point-only publication must not replace the service view");
        assert!(partial_error.is_retryable());

        let second_publication =
            begin_topology_publication(&point_path).expect("resume replacement publication");
        health_writer
            .rebuild_for_publication(Arc::new(second.health_manifest().clone()), second_epoch)
            .expect("publish replacement health plane");
        health_writer
            .update_heartbeat(aether_shm_bridge::timestamp_ms())
            .expect("replacement health heartbeat");
        let second_timestamp = TimestampMs::new(aether_shm_bridge::timestamp_ms());
        let second_sample = AcquiredPointSample::new(
            ChannelPointAddress::new(ChannelId::new(5), PointKind::Telemetry, PointId::new(0))
                .expect("replacement address"),
            50.0,
            50.0,
            second_timestamp,
            PointQuality::Good,
        )
        .expect("replacement sample");
        point_writer
            .generation()
            .expect("replacement point generation")
            .acquisition_writer()
            .commit_batch(&[second_sample])
            .expect("write replacement point");
        second_publication
            .commit(&health_path, second_epoch)
            .expect("commit replacement publication");

        assert!(source.refresh_topology(&pool, &config).await.unwrap());
        assert_eq!(
            source
                .read_formula("inst:100:M:5")
                .expect("read replacement route")
                .expect("replacement value")
                .value(),
            50.0
        );
        assert_eq!(
            data_processing
                .read(commissioned_address)
                .await
                .expect("read refreshed commissioned point")
                .expect("refreshed commissioned sample")
                .value(),
            50.0
        );
        let duplicate_error = data_processing
            .read_many(&[commissioned_address, second_commissioned_address])
            .await
            .expect_err("a refreshed duplicate physical target must fail closed");
        assert_eq!(duplicate_error.kind(), PortErrorKind::InvalidData);
        assert!(!source.accepts_point_watch_event(old_event));
        assert!(source.accepts_point_watch_event(PointWatchEvent::new(
            5,
            PointKind::Telemetry,
            0,
            0,
            50.0,
            50.0,
            2_000,
            1,
        )));
    }
}
