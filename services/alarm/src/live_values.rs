//! Alarm-rule reads from the SHM live-state plane.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use aether_domain::PointKind;
use aether_ports::{PortError, PortErrorKind, PortResult};
use aether_shm_bridge::{
    ChannelPointManifest, ReconnectingSlotSource, ShmChannelHealthReader, ShmClientConfig,
    SlotSnapshot, SlotSource,
};
use aether_store_local::load_sqlite_shm_topology;
use anyhow::Context;
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

/// Read side for per-channel connectivity, deliberately separate from point
/// values so online state cannot be fabricated from measurement data.
pub trait ChannelHealthSource: Send + Sync + 'static {
    /// Reads a channel's online state as `1.0` or `0.0`.
    fn read_channel(&self, channel_id: u32) -> PortResult<Option<SlotSnapshot>>;
}

/// Health source used until the dedicated SHM channel-health plane is enabled.
#[cfg(test)]
#[derive(Debug, Clone, Copy, Default)]
pub struct NoChannelHealth;

#[cfg(test)]
impl ChannelHealthSource for NoChannelHealth {
    fn read_channel(&self, _channel_id: u32) -> PortResult<Option<SlotSnapshot>> {
        Ok(None)
    }
}

impl ChannelHealthSource for ShmChannelHealthReader {
    fn read_channel(&self, channel_id: u32) -> PortResult<Option<SlotSnapshot>> {
        Ok(
            ShmChannelHealthReader::read_channel(self, channel_id)?.map(|sample| {
                SlotSnapshot::new(
                    if sample.online() { 1.0 } else { 0.0 },
                    sample.timestamp_ms(),
                )
            }),
        )
    }
}

/// Service-local capability consumed by the alarm monitor.
pub trait AlarmValueSource: Send + Sync + 'static {
    /// Resolves an existing alarm-rule address and reads its current value.
    fn read_rule(&self, rule: &AlertRule) -> PortResult<Option<SlotSnapshot>>;

    /// Resolves the physical slot used for PointWatch subscription.
    /// Channel-health rules use a separate segment and return `None`.
    fn watched_slot(&self, rule: &AlertRule) -> PortResult<Option<usize>>;
}

/// Alarm value adapter over the self-healing slot source and SQLite-derived
/// manifest/routing snapshots.
pub struct ShmAlarmValueSource {
    slots: Arc<dyn SlotSource>,
    manifest: Arc<ChannelPointManifest>,
    routing: Arc<AlarmRouting>,
    channel_health: Arc<dyn ChannelHealthSource>,
}

impl ShmAlarmValueSource {
    /// Creates an alarm value source from independently testable capabilities.
    #[must_use]
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
            slots,
            manifest,
            routing,
            channel_health,
        }
    }

    fn read_point(&self, point: ChannelPointRef) -> PortResult<Option<SlotSnapshot>> {
        let Some(slot) = self
            .manifest
            .slot(point.channel_id, point.kind, point.point_id)
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
        match self.resolve_target(rule)? {
            Some(ResolvedAlarmTarget::Point(target)) => self.read_point(target),
            Some(ResolvedAlarmTarget::ChannelHealth(channel_id)) => {
                self.channel_health.read_channel(channel_id)
            },
            None => Ok(None),
        }
    }

    fn watched_slot(&self, rule: &AlertRule) -> PortResult<Option<usize>> {
        match self.resolve_target(rule)? {
            Some(ResolvedAlarmTarget::Point(target)) => {
                Ok(self
                    .manifest
                    .slot(target.channel_id, target.kind, target.point_id))
            },
            Some(ResolvedAlarmTarget::ChannelHealth(_)) | None => Ok(None),
        }
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
    let (point_manifest, health_manifest) = load_sqlite_shm_topology(pool)
        .await
        .context("load canonical SHM topology for alarm")?
        .into_manifests();
    let manifest = Arc::new(point_manifest);
    let routing = Arc::new(load_alarm_routing(pool).await?);
    let health_manifest = Arc::new(health_manifest);
    let slot_source = Arc::new(ReconnectingSlotSource::new(
        ShmClientConfig::new(&config.shm_path, manifest.layout_hash())
            .with_writer_stale_after(Duration::from_millis(config.shm_writer_stale_after_ms))
            .with_identity_check_interval(Duration::from_millis(
                config.shm_identity_check_interval_ms,
            )),
    ));

    let channel_health = Arc::new(ShmChannelHealthReader::new(
        ShmClientConfig::new(
            &config.channel_health_shm_path,
            health_manifest.layout_hash(),
        )
        .with_writer_stale_after(Duration::from_millis(config.shm_writer_stale_after_ms))
        .with_identity_check_interval(Duration::from_millis(config.shm_identity_check_interval_ms)),
        health_manifest,
    ));

    Ok(Arc::new(ShmAlarmValueSource::new(
        slot_source,
        manifest,
        routing,
        channel_health,
    )))
}

async fn load_alarm_routing(pool: &SqlitePool) -> anyhow::Result<AlarmRouting> {
    let measurement_rows: Vec<(i64, i64, String, i64, i64)> = sqlx::query_as(
        "SELECT instance_id, channel_id, channel_type, channel_point_id, measurement_id \
         FROM measurement_routing WHERE enabled = TRUE",
    )
    .fetch_all(pool)
    .await
    .context("load measurement routing for alarm")?;
    let action_rows: Vec<(i64, i64, String, i64, i64)> = sqlx::query_as(
        "SELECT instance_id, channel_id, channel_type, channel_point_id, action_id \
         FROM action_routing WHERE enabled = TRUE",
    )
    .fetch_all(pool)
    .await
    .context("load action routing for alarm")?;

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

    Ok(AlarmRouting::from_entries(measurements, actions))
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
            .slot(10, PointKind::Telemetry, 1)
            .expect("telemetry slot");
        let a_slot = manifest
            .slot(10, PointKind::Action, 0)
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
}
