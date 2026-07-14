//! Alarm-rule reads from the SHM live-state plane.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use aether_domain::{ChannelId, PointKind};
#[cfg(test)]
use aether_ports::ChannelHealthObservation;
use aether_ports::{ChannelHealthSource, PortError, PortErrorKind, PortResult};
use aether_shm_bridge::{
    ChannelPointManifest, PhysicalPointAddress, PointWatchEvent, ShmClientConfig,
    ShmReadTopologyGeneration, SlotSnapshot, SlotSource,
};
use aether_store_local::{SqliteLiveTopologySnapshot, load_sqlite_live_topology};
use anyhow::Context;
use arc_swap::ArcSwap;
use sqlx::SqlitePool;

use crate::config::AlarmConfig;
use crate::models::AlertRule;

/// One physical channel point referenced by routing configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChannelPointRef {
    channel_id: u32,
    kind: PointKind,
    point_id: u32,
}

impl ChannelPointRef {
    /// Creates a physical channel-point reference.
    #[must_use]
    pub const fn new(channel_id: u32, kind: PointKind, point_id: u32) -> Self {
        Self {
            channel_id,
            kind,
            point_id,
        }
    }
}

/// In-memory instance-to-channel routing snapshot used by alarm reads.
#[derive(Debug, Clone, Default)]
pub struct AlarmRouting {
    measurements: HashMap<(u32, u32), ChannelPointRef>,
    actions: HashMap<(u32, u32), ChannelPointRef>,
}

impl AlarmRouting {
    /// Builds routing from `(instance_id, point_id, channel_target)` entries.
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

    fn measurement(&self, instance_id: u32, point_id: u32) -> Option<ChannelPointRef> {
        self.measurements.get(&(instance_id, point_id)).copied()
    }

    fn action(&self, instance_id: u32, point_id: u32) -> Option<ChannelPointRef> {
        self.actions.get(&(instance_id, point_id)).copied()
    }
}

/// Health source used until the dedicated SHM channel-health plane is enabled.
#[cfg(test)]
#[derive(Debug, Clone, Copy, Default)]
pub struct NoChannelHealth;

#[cfg(test)]
impl ChannelHealthSource for NoChannelHealth {
    fn read_channel(&self, _channel_id: ChannelId) -> PortResult<Option<ChannelHealthObservation>> {
        Ok(None)
    }
}

/// Service-local capability consumed by the alarm monitor.
pub trait AlarmValueSource: Send + Sync + 'static {
    /// Resolves an existing alarm-rule address and reads its current value.
    fn read_rule(&self, rule: &AlertRule) -> PortResult<Option<SlotSnapshot>>;

    /// Resolves the physical slot used for PointWatch subscription.
    /// Channel-health rules use a separate segment and return `None`.
    fn watched_slot(&self, rule: &AlertRule) -> PortResult<Option<usize>>;

    /// Validates a PointWatch hint against the current typed manifest.
    fn validated_point_watch_slot(&self, _event: PointWatchEvent) -> Option<usize> {
        None
    }
}

/// Alarm value adapter over an atomically replaceable topology generation.
pub struct ShmAlarmValueSource {
    current: ArcSwap<AlarmValueGeneration>,
}

struct AlarmValueGeneration {
    slots: Arc<dyn SlotSource>,
    manifest: Arc<ChannelPointManifest>,
    routing: Arc<AlarmRouting>,
    channel_health: Arc<dyn ChannelHealthSource>,
    topology: Option<Arc<ShmReadTopologyGeneration>>,
    digest: u64,
}

impl ShmAlarmValueSource {
    /// Creates an alarm value source from independently testable capabilities.
    #[must_use]
    #[cfg(test)]
    pub fn new<S, H>(
        slots: Arc<S>,
        manifest: Arc<ChannelPointManifest>,
        routing: Arc<AlarmRouting>,
        channel_health: Arc<H>,
    ) -> Self
    where
        S: SlotSource,
        H: ChannelHealthSource,
    {
        Self {
            current: ArcSwap::from_pointee(AlarmValueGeneration {
                slots,
                manifest,
                routing,
                channel_health,
                topology: None,
                digest: 0,
            }),
        }
    }

    /// Reloads one SQLite topology snapshot and atomically publishes it after
    /// both physical SHM planes validate against that same snapshot.
    pub async fn refresh_topology(
        &self,
        pool: &SqlitePool,
        config: &AlarmConfig,
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

        let lazy_route_only = current.digest != snapshot.digest()
            && current
                .topology
                .as_ref()
                .is_some_and(|topology| topology.publication_epoch() == 0);
        if physical_layout_matches(&current, &snapshot) && (physical_current || lazy_route_only) {
            let routing = Arc::new(alarm_routing_from_snapshot(&snapshot));
            let topology = Arc::clone(current.topology.as_ref().ok_or_else(|| {
                PortError::new(
                    PortErrorKind::Permanent,
                    "validated alarm generation is missing its SHM topology",
                )
            })?);
            let next = Arc::new(AlarmValueGeneration {
                slots: Arc::clone(&current.slots),
                manifest: Arc::clone(&current.manifest),
                routing,
                channel_health: Arc::clone(&current.channel_health),
                topology: Some(Arc::clone(&topology)),
                digest: snapshot.digest(),
            });
            if physical_current {
                topology
                    .with_validated_authority(|| self.current.store(next))
                    .map_err(retryable_topology_transition)?;
            } else {
                self.current.store(next);
            }
            return Ok(true);
        }

        let config = config.clone();
        let candidate = tokio::task::spawn_blocking(move || {
            build_alarm_generation(snapshot, &config, TopologyOpenMode::ValidatePhysical)
        })
        .await
        .map_err(|error| {
            PortError::new(
                PortErrorKind::Unavailable,
                format!("alarm topology validation task failed: {error}"),
            )
        })??;
        let candidate = Arc::new(candidate);
        let topology = Arc::clone(candidate.topology.as_ref().ok_or_else(|| {
            PortError::new(
                PortErrorKind::Permanent,
                "validated alarm generation is missing its SHM topology",
            )
        })?);
        topology
            .with_validated_authority(|| self.current.store(candidate))
            .map_err(retryable_topology_transition)?;
        Ok(true)
    }

    /// Returns whether an event still names the current typed physical slot.
    #[must_use]
    pub fn accepts_point_watch_event(&self, event: PointWatchEvent) -> bool {
        let generation = self.current.load();
        event.matches_manifest(&generation.manifest)
    }
}

impl AlarmValueGeneration {
    fn read_point(&self, point: ChannelPointRef) -> PortResult<Option<SlotSnapshot>> {
        let Some(slot) = self
            .manifest
            .slot_for(PhysicalPointAddress::from_legacy_raw(
                point.channel_id,
                point.kind,
                point.point_id,
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
                format!("SHM slot {slot} contains a non-finite alarm value"),
            ));
        }
        Ok(Some(sample))
    }

    fn resolve_target(&self, rule: &AlertRule) -> PortResult<Option<ResolvedAlarmTarget>> {
        let owner_id = checked_u32(rule.channel_id, "owner/channel id")?;
        let point_id = checked_u32(rule.point_id, "point id")?;

        let target = match (rule.service_type.as_str(), rule.data_type.as_str()) {
            ("io", AlertRule::CHANNEL_ONLINE_DATA_TYPE) => {
                ResolvedAlarmTarget::ChannelHealth(owner_id)
            },
            ("io", data_type) => ResolvedAlarmTarget::Point(ChannelPointRef::new(
                owner_id,
                parse_channel_kind(data_type)
                    .ok_or_else(|| invalid_rule_target(rule, "unsupported channel point type"))?,
                point_id,
            )),
            ("inst", "M") => {
                let Some(target) = self.routing.measurement(owner_id, point_id) else {
                    return Ok(None);
                };
                ResolvedAlarmTarget::Point(target)
            },
            ("inst", "A") => {
                let Some(target) = self.routing.action(owner_id, point_id) else {
                    return Ok(None);
                };
                ResolvedAlarmTarget::Point(target)
            },
            _ => {
                return Err(invalid_rule_target(
                    rule,
                    "unsupported live-state namespace",
                ));
            },
        };
        Ok(Some(target))
    }
}

enum ResolvedAlarmTarget {
    Point(ChannelPointRef),
    ChannelHealth(u32),
}

impl AlarmValueSource for ShmAlarmValueSource {
    fn read_rule(&self, rule: &AlertRule) -> PortResult<Option<SlotSnapshot>> {
        let generation = self.current.load();
        match generation.resolve_target(rule)? {
            Some(ResolvedAlarmTarget::Point(target)) => generation.read_point(target),
            Some(ResolvedAlarmTarget::ChannelHealth(channel_id)) => Ok(generation
                .channel_health
                .read_channel(ChannelId::new(channel_id))?
                .map(|sample| {
                    SlotSnapshot::new(
                        if sample.online() { 1.0 } else { 0.0 },
                        sample.timestamp_ms(),
                    )
                })),
            None => Ok(None),
        }
    }

    fn watched_slot(&self, rule: &AlertRule) -> PortResult<Option<usize>> {
        let generation = self.current.load();
        match generation.resolve_target(rule)? {
            Some(ResolvedAlarmTarget::Point(target)) => {
                Ok(generation
                    .manifest
                    .slot_for(PhysicalPointAddress::from_legacy_raw(
                        target.channel_id,
                        target.kind,
                        target.point_id,
                    )))
            },
            Some(ResolvedAlarmTarget::ChannelHealth(_)) | None => Ok(None),
        }
    }

    fn validated_point_watch_slot(&self, event: PointWatchEvent) -> Option<usize> {
        if !self.accepts_point_watch_event(event) {
            return None;
        }
        usize::try_from(event.slot_index()).ok()
    }
}

fn checked_u32(value: i64, label: &str) -> PortResult<u32> {
    u32::try_from(value).map_err(|_| {
        PortError::new(
            PortErrorKind::Permanent,
            format!("alarm rule {label} must fit in u32, got {value}"),
        )
    })
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

fn invalid_rule_target(rule: &AlertRule, reason: &str) -> PortError {
    PortError::new(
        PortErrorKind::Permanent,
        format!(
            "{reason}: service_type={}, owner_id={}, data_type={}, point_id={}",
            rule.service_type, rule.channel_id, rule.data_type, rule.point_id
        ),
    )
}

/// Builds the production alarm value source from embedded SQLite configuration.
///
/// Opening the SHM file is lazy, so io being down or starting later does
/// not block alarm startup.
pub async fn build_shm_alarm_source(
    pool: &SqlitePool,
    config: &AlarmConfig,
) -> anyhow::Result<Arc<ShmAlarmValueSource>> {
    let snapshot = load_sqlite_live_topology(pool)
        .await
        .context("load canonical live topology for alarm")?;
    let generation = build_alarm_generation(snapshot, config, TopologyOpenMode::Lazy)
        .context("compose lazy alarm SHM topology")?;
    Ok(Arc::new(ShmAlarmValueSource {
        current: ArcSwap::from_pointee(generation),
    }))
}

/// Periodically refreshes the alarm's complete SQLite/SHM topology view.
///
/// A failed or partially published candidate is logged and retried while the
/// last coherent generation remains active.
pub async fn run_alarm_topology_refresh(
    source: Arc<ShmAlarmValueSource>,
    pool: SqlitePool,
    config: AlarmConfig,
    shutdown: tokio_util::sync::CancellationToken,
) {
    let refresh_interval = Duration::from_millis(config.shm_topology_refresh_interval_ms.max(100));
    let mut ticker = tokio::time::interval(refresh_interval);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        tokio::select! {
            _ = shutdown.cancelled() => break,
            _ = ticker.tick() => {
                match source.refresh_topology(&pool, &config).await {
                    Ok(true) => tracing::info!("Alarm live topology generation refreshed"),
                    Ok(false) => {},
                    Err(error) => tracing::warn!(
                        retryable = error.is_retryable(),
                        "Alarm live topology refresh retained the previous generation: {error}"
                    ),
                }
            }
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum TopologyOpenMode {
    Lazy,
    ValidatePhysical,
}

fn build_alarm_generation(
    snapshot: SqliteLiveTopologySnapshot,
    config: &AlarmConfig,
    mode: TopologyOpenMode,
) -> PortResult<AlarmValueGeneration> {
    let digest = snapshot.digest();
    let routing = Arc::new(alarm_routing_from_snapshot(&snapshot));
    let (point_manifest, health_manifest, _, _) = snapshot.into_parts();
    let point_manifest = Arc::new(point_manifest);
    let health_manifest = Arc::new(health_manifest);
    let point_config = shm_client_config(&config.shm_path, point_manifest.layout_hash(), config);
    let health_config = shm_client_config(
        &config.channel_health_shm_path,
        health_manifest.layout_hash(),
        config,
    );
    let topology = Arc::new(match mode {
        TopologyOpenMode::Lazy => ShmReadTopologyGeneration::new_lazy(
            point_config,
            health_config,
            Arc::clone(&point_manifest),
            Arc::clone(&health_manifest),
        )?,
        TopologyOpenMode::ValidatePhysical => ShmReadTopologyGeneration::open(
            point_config,
            health_config,
            Arc::clone(&point_manifest),
            Arc::clone(&health_manifest),
        )
        .map_err(retryable_topology_transition)?,
    });
    let slots: Arc<dyn SlotSource> = topology.point_source().clone();
    let channel_health: Arc<dyn ChannelHealthSource> = topology.channel_health().clone();
    Ok(AlarmValueGeneration {
        slots,
        manifest: point_manifest,
        routing,
        channel_health,
        topology: Some(topology),
        digest,
    })
}

fn physical_layout_matches(
    current: &AlarmValueGeneration,
    snapshot: &SqliteLiveTopologySnapshot,
) -> bool {
    current.topology.as_ref().is_some_and(|topology| {
        topology.point_manifest().layout_hash() == snapshot.point_manifest().layout_hash()
            && topology.point_manifest().slot_count() == snapshot.point_manifest().slot_count()
            && topology.health_manifest().layout_hash() == snapshot.health_manifest().layout_hash()
            && topology.health_manifest().slot_count() == snapshot.health_manifest().slot_count()
    })
}

fn alarm_routing_from_snapshot(snapshot: &SqliteLiveTopologySnapshot) -> AlarmRouting {
    AlarmRouting::from_entries(
        snapshot
            .measurement_routes()
            .map(|(instance_id, point_id, target)| {
                (
                    instance_id,
                    point_id,
                    ChannelPointRef::new(
                        target.channel_id().get(),
                        target.kind(),
                        target.point_id().get(),
                    ),
                )
            }),
        snapshot
            .action_routes()
            .map(|(instance_id, point_id, target)| {
                (
                    instance_id,
                    point_id,
                    ChannelPointRef::new(
                        target.channel_id().get(),
                        target.kind(),
                        target.point_id().get(),
                    ),
                )
            }),
    )
}

fn shm_client_config(path: &str, layout_hash: u64, config: &AlarmConfig) -> ShmClientConfig {
    ShmClientConfig::new(path, layout_hash)
        .with_writer_stale_after(Duration::from_millis(config.shm_writer_stale_after_ms))
        .with_identity_check_interval(Duration::from_millis(config.shm_identity_check_interval_ms))
}

fn retryable_topology_transition(error: PortError) -> PortError {
    if error.is_retryable() {
        return error;
    }
    PortError::new(
        PortErrorKind::Conflict,
        format!("alarm SHM topology publication is incomplete: {error}"),
    )
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Arc;

    use aether_domain::PointKind;
    use aether_ports::{PortError, PortErrorKind, PortResult};
    use aether_shm_bridge::{
        ChannelPointManifest, PhysicalPointAddress, PointWatchEvent, SlotSnapshot, SlotSource,
    };

    use super::{
        AlarmRouting, AlarmValueSource, ChannelPointRef, NoChannelHealth, ShmAlarmValueSource,
    };
    use crate::models::AlertRule;

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

    fn rule(service_type: &str, owner_id: i64, data_type: &str, point_id: i64) -> AlertRule {
        AlertRule {
            id: 1,
            service_type: service_type.to_string(),
            channel_id: owner_id,
            data_type: data_type.to_string(),
            point_id,
            rule_name: "test".to_string(),
            warning_level: 1,
            operator: ">".to_string(),
            value: 10.0,
            enabled: true,
            description: None,
            created_at: 0,
            updated_at: 0,
        }
    }

    fn source() -> ShmAlarmValueSource {
        let manifest = ChannelPointManifest::from_entries([(10, [2, 1, 0, 1]), (20, [1, 0, 1, 0])]);
        let t_slot = manifest
            .slot_for(PhysicalPointAddress::from_legacy_raw(
                10,
                PointKind::Telemetry,
                1,
            ))
            .expect("telemetry slot");
        let a_slot = manifest
            .slot_for(PhysicalPointAddress::from_legacy_raw(
                10,
                PointKind::Action,
                0,
            ))
            .expect("action slot");
        let values = HashMap::from([
            (t_slot, SlotSnapshot::new(42.5, 1_000)),
            (a_slot, SlotSnapshot::new(7.0, 1_001)),
        ]);
        let routing = AlarmRouting::from_entries(
            [(100, 5, ChannelPointRef::new(10, PointKind::Telemetry, 1))],
            [(100, 8, ChannelPointRef::new(10, PointKind::Action, 0))],
        );

        ShmAlarmValueSource::new(
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
    fn direct_channel_rule_reads_shm_slot() {
        let sample = source()
            .read_rule(&rule("io", 10, "T", 1))
            .expect("read channel rule")
            .expect("channel sample");

        assert_eq!(sample.value(), 42.5);
        assert_eq!(sample.timestamp_ms(), 1_000);
    }

    #[test]
    fn instance_measurement_and_action_rules_resolve_through_routes() {
        let source = source();

        let measurement = source
            .read_rule(&rule("inst", 100, "M", 5))
            .expect("read measurement")
            .expect("measurement sample");
        let action = source
            .read_rule(&rule("inst", 100, "A", 8))
            .expect("read action")
            .expect("action sample");

        assert_eq!(measurement.value(), 42.5);
        assert_eq!(action.value(), 7.0);
    }

    #[test]
    fn watch_slots_use_the_same_resolution_as_reads() {
        let source = source();

        assert_eq!(
            source
                .watched_slot(&rule("io", 10, "T", 1))
                .expect("channel watch slot"),
            source
                .watched_slot(&rule("inst", 100, "M", 5))
                .expect("instance watch slot")
        );
        assert_eq!(
            source
                .watched_slot(&rule("io", 10, "online", 0))
                .expect("health has no point-watch slot"),
            None
        );
    }

    #[test]
    fn point_watch_hints_are_validated_by_typed_physical_address() {
        let source = source();
        let slot = source
            .watched_slot(&rule("io", 10, "T", 1))
            .expect("resolve watched slot")
            .expect("configured slot");
        let valid = PointWatchEvent::new(
            10,
            PointKind::Telemetry,
            1,
            u64::try_from(slot).expect("slot fits in wire field"),
            42.5,
            42.5,
            1_000,
            1,
        );
        let stale = PointWatchEvent::new(
            20,
            PointKind::Telemetry,
            0,
            u64::try_from(slot).expect("slot fits in wire field"),
            42.5,
            42.5,
            1_000,
            1,
        );

        assert_eq!(source.validated_point_watch_slot(valid), Some(slot));
        assert_eq!(source.validated_point_watch_slot(stale), None);
    }

    #[test]
    fn channel_online_rule_uses_health_port_not_a_fake_slot() {
        let value = source()
            .read_rule(&rule("io", 10, "online", 0))
            .expect("health source is available");

        assert_eq!(value, None);
    }

    #[test]
    fn unsupported_live_state_namespace_is_a_permanent_configuration_error() {
        let error = source()
            .read_rule(&rule("unknown", 10, "T", 1))
            .expect_err("unknown namespace must fail");

        assert!(!error.is_retryable());
    }

    #[tokio::test]
    async fn production_source_builds_before_shm_writer() {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("open in-memory config database");
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
                .expect("create minimal config schema");
        }
        let dir = tempfile::tempdir().expect("temporary directory");
        let shm_path = dir
            .path()
            .join("writer-not-started.shm")
            .display()
            .to_string();
        let channel_health_shm_path = dir
            .path()
            .join("health-writer-not-started.shm")
            .display()
            .to_string();
        let config = crate::config::AlarmConfig {
            shm_path,
            channel_health_shm_path,
            ..Default::default()
        };

        let source = super::build_shm_alarm_source(&pool, &config)
            .await
            .expect("build source without any external service");
        let error = source
            .read_rule(&rule("io", 10, "T", 0))
            .expect_err("missing writer is a read-time condition");

        assert!(error.is_retryable());
    }

    #[tokio::test]
    async fn routing_only_refresh_succeeds_while_io_is_offline() {
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
            "INSERT INTO channels VALUES (10, 'virtual')",
            "INSERT INTO telemetry_points VALUES (10, 0)",
            "INSERT INTO telemetry_points VALUES (10, 1)",
            "INSERT INTO measurement_routing VALUES (100, 10, 'T', 0, 5, TRUE)",
        ] {
            sqlx::query(statement)
                .execute(&pool)
                .await
                .expect("create routing-only topology");
        }
        let directory = tempfile::tempdir().expect("temporary missing-SHM directory");
        let config = crate::config::AlarmConfig {
            shm_path: directory
                .path()
                .join("missing-live.shm")
                .to_string_lossy()
                .into_owned(),
            channel_health_shm_path: directory
                .path()
                .join("missing-health.shm")
                .to_string_lossy()
                .into_owned(),
            ..Default::default()
        };
        let source = super::build_shm_alarm_source(&pool, &config)
            .await
            .expect("build offline alarm source");
        assert_eq!(
            source
                .watched_slot(&rule("inst", 100, "M", 5))
                .expect("initial route"),
            Some(0)
        );

        sqlx::query(
            "UPDATE measurement_routing SET channel_point_id = 1 \
             WHERE instance_id = 100 AND measurement_id = 5",
        )
        .execute(&pool)
        .await
        .expect("move logical route without changing the physical layout");

        assert!(source.refresh_topology(&pool, &config).await.unwrap());
        assert_eq!(
            source
                .watched_slot(&rule("inst", 100, "M", 5))
                .expect("replacement route"),
            Some(1)
        );
    }

    #[tokio::test]
    async fn production_source_refreshes_point_health_and_routing_as_one_view() {
        use aether_domain::{
            AcquiredPointSample, ChannelId, ChannelPointAddress, PointId, PointQuality, TimestampMs,
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
            "INSERT INTO measurement_routing VALUES (100, 10, 'T', 0, 5, TRUE)",
        ] {
            sqlx::query(statement)
                .execute(&pool)
                .await
                .expect("create initial topology");
        }
        let directory = tempfile::tempdir().expect("temporary SHM directory");
        let point_path = directory.path().join("live.shm");
        let health_path = directory.path().join("health.shm");
        let config = crate::config::AlarmConfig {
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
            .set_online(10, true, first_timestamp.get())
            .expect("write initial health");

        let source = super::build_shm_alarm_source(&pool, &config)
            .await
            .expect("build alarm source");
        source
            .refresh_topology(&pool, &config)
            .await
            .expect("pin initial committed publication");
        assert_eq!(
            source
                .read_rule(&rule("inst", 100, "M", 5))
                .expect("read initial route")
                .expect("initial sample")
                .value(),
            10.0
        );
        assert_eq!(
            source
                .read_rule(&rule("io", 10, "online", 0))
                .expect("read initial health")
                .expect("initial health sample")
                .value(),
            1.0
        );
        let old_event = PointWatchEvent::new(10, PointKind::Telemetry, 0, 0, 10.0, 10.0, 1_000, 1);
        assert!(source.accepts_point_watch_event(old_event));

        for statement in [
            "INSERT INTO channels VALUES (5, 'virtual')",
            "INSERT INTO telemetry_points VALUES (5, 0)",
            "UPDATE measurement_routing SET channel_id = 5 WHERE instance_id = 100 AND measurement_id = 5",
        ] {
            sqlx::query(statement)
                .execute(&pool)
                .await
                .expect("mutate replacement topology");
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
            .expect_err("point-only publication must retain the old service generation");
        assert!(partial_error.is_retryable());
        assert!(source.accepts_point_watch_event(old_event));

        let second_publication =
            begin_topology_publication(&point_path).expect("resume replacement publication");
        health_writer
            .rebuild_for_publication(Arc::new(second.health_manifest().clone()), second_epoch)
            .expect("publish replacement health plane");
        let second_timestamp = TimestampMs::new(aether_shm_bridge::timestamp_ms());
        health_writer
            .set_online(5, false, second_timestamp.get())
            .expect("write replacement health");
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
                .read_rule(&rule("inst", 100, "M", 5))
                .expect("read replacement route")
                .expect("replacement sample")
                .value(),
            50.0
        );
        assert_eq!(
            source
                .read_rule(&rule("io", 5, "online", 0))
                .expect("read replacement health")
                .expect("replacement health sample")
                .value(),
            0.0
        );
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
