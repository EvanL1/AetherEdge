//! WebSocket live-state reads from SHM.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;
use std::time::Duration;

use aether_domain::{PointAddress, PointKind};
use aether_ports::{PortError, PortErrorKind, PortResult};
use aether_shm_bridge::{
    ChannelHealthSample, ChannelPointManifest, ReconnectingSlotSource, ShmChannelHealthReader,
    ShmClientConfig, ShmLiveState, SlotSnapshot, SlotSource, StaticSlotResolver,
};
use aether_store_local::load_sqlite_shm_topology;
use anyhow::Context;
use sqlx::SqlitePool;

use crate::config::GatewayConfig;

/// One physical channel point referenced by instance routing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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

/// Read side for the separate channel-connectivity plane.
pub trait ChannelHealthSource: Send + Sync + 'static {
    /// Reads one channel connectivity sample.
    fn read_channel(&self, channel_id: u32) -> PortResult<Option<ChannelHealthSample>>;
}

/// Test-only empty health source.
#[cfg(test)]
#[derive(Debug, Clone, Copy, Default)]
pub struct NoChannelHealth;

#[cfg(test)]
impl ChannelHealthSource for NoChannelHealth {
    fn read_channel(&self, _channel_id: u32) -> PortResult<Option<ChannelHealthSample>> {
        Ok(None)
    }
}

impl ChannelHealthSource for ShmChannelHealthReader {
    fn read_channel(&self, channel_id: u32) -> PortResult<Option<ChannelHealthSample>> {
        ShmChannelHealthReader::read_channel(self, channel_id)
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
}

#[derive(Debug, Clone, Copy)]
enum FormulaTarget {
    Main(ChannelPointRef),
    ChannelHealth(u32),
}

/// SHM-backed implementation of gateway current-value reads.
pub struct ShmGatewayValueSource {
    slots: Arc<dyn SlotSource>,
    manifest: Arc<ChannelPointManifest>,
    routing: Arc<GatewayRouting>,
    channel_health: Arc<dyn ChannelHealthSource>,
}

impl ShmGatewayValueSource {
    /// Creates the source from independently testable capabilities.
    #[must_use]
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
            slots,
            manifest,
            routing,
            channel_health,
        }
    }

    fn read_point(&self, target: ChannelPointRef) -> PortResult<Option<SlotSnapshot>> {
        let Some(slot) = self
            .manifest
            .slot(target.channel_id, target.kind, target.point_id)
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
        let owner_id = checked_u32(owner_id, "subscription owner id")?;
        if source == "io" && data_type == "online" {
            return Ok(self
                .channel_health
                .read_channel(owner_id)?
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
            "io" => self.channel_targets(owner_id, data_type)?,
            "inst" if matches!(data_type, "M" | "A") => {
                self.routing.points(owner_id, data_type).collect()
            },
            _ => {
                return Err(invalid_target(format!(
                    "unsupported group {source}:{data_type}"
                )));
            },
        };
        let mut values = BTreeMap::new();
        for (point_id, target) in targets {
            if let Some(sample) = self.read_point(target)? {
                values.insert(point_id.to_string(), sample);
            }
        }
        Ok(values)
    }

    fn read_formula(&self, formula: &str) -> PortResult<Option<SlotSnapshot>> {
        let Some(target) = self.formula_target(formula)? else {
            return Ok(None);
        };
        match target {
            FormulaTarget::ChannelHealth(channel_id) => {
                Ok(self.channel_health.read_channel(channel_id)?.map(|sample| {
                    SlotSnapshot::new(
                        if sample.online() { 1.0 } else { 0.0 },
                        sample.timestamp_ms(),
                    )
                }))
            },
            FormulaTarget::Main(target) => self.read_point(target),
        }
    }

    fn watched_slots(
        &self,
        source: &str,
        owner_ids: &[i64],
        data_types: &[String],
    ) -> PortResult<BTreeSet<usize>> {
        let mut slots = BTreeSet::new();
        for &owner_id in owner_ids {
            let owner_id = checked_u32(owner_id, "subscription owner id")?;
            for data_type in data_types {
                let targets: Vec<_> = match source {
                    "io" if data_type == "online" => Vec::new(),
                    "io" => self.channel_targets(owner_id, data_type)?,
                    "inst" if matches!(data_type.as_str(), "M" | "A") => {
                        self.routing.points(owner_id, data_type).collect()
                    },
                    _ => continue,
                };
                for (_, target) in targets {
                    if let Some(slot) =
                        self.manifest
                            .slot(target.channel_id, target.kind, target.point_id)
                    {
                        slots.insert(slot);
                    }
                }
            }
        }
        Ok(slots)
    }

    fn watched_formula_slot(&self, formula: &str) -> PortResult<Option<usize>> {
        Ok(match self.formula_target(formula)? {
            Some(FormulaTarget::Main(target)) => {
                self.manifest
                    .slot(target.channel_id, target.kind, target.point_id)
            },
            Some(FormulaTarget::ChannelHealth(_)) | None => None,
        })
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
    let (point_manifest, health_manifest) = load_sqlite_shm_topology(pool)
        .await
        .context("load canonical SHM topology for api")?
        .into_manifests();
    let manifest = Arc::new(point_manifest);
    let routing = Arc::new(load_gateway_routing(pool).await?);
    let health_manifest = Arc::new(health_manifest);
    let slots = Arc::new(ReconnectingSlotSource::new(
        ShmClientConfig::new(&config.shm_path, manifest.layout_hash())
            .with_writer_stale_after(Duration::from_millis(config.shm_writer_stale_after_ms))
            .with_identity_check_interval(Duration::from_millis(
                config.shm_identity_check_interval_ms,
            )),
    ));
    let health = Arc::new(ShmChannelHealthReader::new(
        ShmClientConfig::new(
            &config.channel_health_shm_path,
            health_manifest.layout_hash(),
        )
        .with_writer_stale_after(Duration::from_millis(config.shm_writer_stale_after_ms))
        .with_identity_check_interval(Duration::from_millis(config.shm_identity_check_interval_ms)),
        health_manifest,
    ));

    Ok(Arc::new(ShmGatewayValueSource::new(
        slots, manifest, routing, health,
    )))
}

/// Builds the read-only `LiveState` used by an enabled Data Processing route.
///
/// Only explicitly commissioned logical point addresses are resolved. The
/// adapter receives no SHM writer and does not change the existing authority.
pub async fn build_data_processing_live_state(
    pool: &SqlitePool,
    config: &GatewayConfig,
    addresses: &[PointAddress],
) -> anyhow::Result<Arc<ShmLiveState>> {
    let (point_manifest, _health_manifest) = load_sqlite_shm_topology(pool)
        .await
        .context("load canonical SHM topology for data processing")?
        .into_manifests();
    let manifest = Arc::new(point_manifest);
    let routing = load_gateway_routing(pool).await?;
    let mut entries = Vec::with_capacity(addresses.len());
    let mut commissioned_slots = std::collections::HashSet::new();
    for address in addresses {
        if address.kind().is_writable() {
            anyhow::bail!("Data Processing live-state mapping targets a writable point");
        }
        let target = routing
            .point(address.instance_id().get(), "M", address.point_id().get())
            .with_context(|| format!("no enabled measurement route for {address:?}"))?;
        if target.kind != address.kind() {
            anyhow::bail!("commissioned logical point kind does not match its physical route");
        }
        let slot = manifest
            .slot(target.channel_id, target.kind, target.point_id)
            .with_context(|| format!("no SHM slot for commissioned point {address:?}"))?;
        if !commissioned_slots.insert(slot) {
            anyhow::bail!("multiple commissioned logical points resolve to one SHM slot");
        }
        entries.push((*address, slot));
    }
    let slots = Arc::new(ReconnectingSlotSource::new(
        ShmClientConfig::new(&config.shm_path, manifest.layout_hash())
            .with_writer_stale_after(Duration::from_millis(config.shm_writer_stale_after_ms))
            .with_identity_check_interval(Duration::from_millis(
                config.shm_identity_check_interval_ms,
            )),
    ));
    Ok(Arc::new(ShmLiveState::new(
        slots,
        Arc::new(StaticSlotResolver::from_entries(entries)),
    )))
}

async fn load_gateway_routing(pool: &SqlitePool) -> anyhow::Result<GatewayRouting> {
    let measurement_rows: Vec<(i64, i64, String, i64, i64)> = sqlx::query_as(
        "SELECT instance_id, channel_id, channel_type, channel_point_id, measurement_id \
         FROM measurement_routing WHERE enabled = TRUE",
    )
    .fetch_all(pool)
    .await
    .context("load measurement routing for api")?;
    let action_rows: Vec<(i64, i64, String, i64, i64)> = sqlx::query_as(
        "SELECT instance_id, channel_id, channel_type, channel_point_id, action_id \
         FROM action_routing WHERE enabled = TRUE",
    )
    .fetch_all(pool)
    .await
    .context("load action routing for api")?;

    let mut measurements = Vec::with_capacity(measurement_rows.len());
    for (instance_id, channel_id, channel_type, channel_point_id, measurement_id) in
        measurement_rows
    {
        let Some(kind) = parse_channel_kind(&channel_type) else {
            tracing::warn!(
                channel_type,
                "skipping invalid measurement route point type"
            );
            continue;
        };
        measurements.push((
            config_u32(instance_id, "measurement instance_id")?,
            config_u32(measurement_id, "measurement_id")?,
            ChannelPointRef::new(
                config_u32(channel_id, "measurement channel_id")?,
                kind,
                config_u32(channel_point_id, "measurement channel_point_id")?,
            ),
        ));
    }
    let mut actions = Vec::with_capacity(action_rows.len());
    for (instance_id, channel_id, channel_type, channel_point_id, action_id) in action_rows {
        let Some(kind) = parse_channel_kind(&channel_type) else {
            tracing::warn!(channel_type, "skipping invalid action route point type");
            continue;
        };
        actions.push((
            config_u32(instance_id, "action instance_id")?,
            config_u32(action_id, "action_id")?,
            ChannelPointRef::new(
                config_u32(channel_id, "action channel_id")?,
                kind,
                config_u32(channel_point_id, "action channel_point_id")?,
            ),
        ));
    }
    Ok(GatewayRouting::from_entries(measurements, actions))
}

fn config_u32(value: i64, label: &str) -> anyhow::Result<u32> {
    u32::try_from(value).with_context(|| format!("{label} must fit in u32, got {value}"))
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Arc;

    use aether_domain::PointKind;
    use aether_ports::{PortError, PortErrorKind, PortResult};
    use aether_shm_bridge::{ChannelPointManifest, SlotSnapshot, SlotSource};

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
                    manifest.slot(10, kind, point_id).expect("configured slot"),
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

        assert_eq!(
            slot,
            source
                .manifest
                .slot(10, PointKind::Telemetry, 1)
                .expect("configured slot")
        );
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
        let error = source
            .read_group("io", 10, "T")
            .expect_err("missing writer is a retryable read-time condition");

        assert!(error.is_retryable());
    }
}
