//! Cloud-facing logical groups backed by SQLite configuration and SHM values.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::time::Duration;

use aether_domain::{
    InstanceId, PointAddress, PointId, PointKind, PointQuality, PointSample, TimestampMs,
};
use aether_ports::{ChannelHealthObservation, PortError, PortErrorKind, PortResult};
use aether_shm_bridge::{
    PhysicalPointAddress, ShmClientConfig, ShmReadTopologyGeneration, SlotSource,
};
use aether_store_local::{SqliteLiveTopologySnapshot, load_sqlite_live_topology};
use arc_swap::ArcSwap;
use regex::Regex;
use sqlx::SqlitePool;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;
use tracing::warn;

use crate::config::EnvConfig;
use crate::models::PropertyEntry;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogicalGroup {
    key: String,
    source: String,
    device: String,
    data_type: String,
    points: BTreeMap<String, usize>,
}

impl LogicalGroup {
    #[must_use]
    pub fn new<P>(
        source: impl Into<String>,
        device: impl Into<String>,
        data_type: impl Into<String>,
        points: impl IntoIterator<Item = (P, usize)>,
    ) -> Self
    where
        P: Into<String>,
    {
        let source = source.into();
        let device = device.into();
        let data_type = data_type.into();
        Self {
            key: format!("{source}:{device}:{data_type}"),
            source,
            device,
            data_type,
            points: points
                .into_iter()
                .map(|(point_id, slot)| (point_id.into(), slot))
                .collect(),
        }
    }
}

/// One immutable live-value catalogue. Rebuild it after configuration changes.
pub struct ShmNetValueSource {
    slots: Arc<dyn SlotSource>,
    groups: BTreeMap<String, LogicalGroup>,
}

/// One immutable Uplink view of the SQLite catalogue and committed IO epoch.
///
/// The retained read generation is deliberately service-owned: callers pin
/// this object once for a complete cloud read or upload pass, so physical
/// slots and logical routes cannot be selected from different generations.
pub struct UplinkTopologyGeneration {
    read: Arc<ShmReadTopologyGeneration>,
    values: ShmNetValueSource,
    digest: u64,
}

impl UplinkTopologyGeneration {
    /// Reads one group from the routes and SHM generation captured together.
    pub fn read_group(
        &self,
        key: &str,
        field: Option<&str>,
    ) -> PortResult<Option<HashMap<String, serde_json::Value>>> {
        self.read.validate_layouts()?;
        self.values.read_group(key, field)
    }

    /// Collects one complete scheduler pass from this immutable generation.
    pub fn collect_entries(
        &self,
        patterns: &[String],
        excludes: &[Regex],
    ) -> PortResult<Vec<PropertyEntry>> {
        self.read.validate_layouts()?;
        self.values.collect_entries(patterns, excludes)
    }

    /// Collects acquisition-owned business point facts for CloudLink.
    ///
    /// The current SHM slot encoding has value/raw/timestamp but no quality
    /// field, so accepted finite values are exposed as `Good`, matching the
    /// read-only `ShmLiveState` adapter. This is not a claim that the physical
    /// source supplied original quality metadata.
    #[allow(dead_code)]
    pub fn collect_point_samples(
        &self,
        patterns: &[String],
        excludes: &[Regex],
    ) -> PortResult<Vec<PointSample>> {
        self.read.validate_layouts()?;
        self.values.collect_point_samples(patterns, excludes)
    }

    /// Iterates the channels this generation's health manifest configures.
    ///
    /// Deterministic order, so a telemetry pass reports the same channel set
    /// every tick and a consumer sees a stable series.
    pub fn channel_ids(&self) -> impl Iterator<Item = u32> + '_ {
        self.read.channel_health().manifest().channel_ids()
    }

    /// Reads channel connectivity from the health plane pinned to this generation.
    ///
    /// `None` means unconfigured or never observed — deliberately not the same
    /// claim as offline (ADR-0016).
    pub fn channel_health(&self, channel_id: u32) -> PortResult<Option<ChannelHealthObservation>> {
        self.read.channel_health().read_channel(channel_id)
    }

    /// Returns the deterministic digest of the one SQLite snapshot used here.
    #[must_use]
    pub const fn digest(&self) -> u64 {
        self.digest
    }

    /// Returns the existing snapshot digest with an explicit non-cryptographic
    /// algorithm tag for the experimental CloudLink topology binding.
    #[allow(dead_code)]
    #[must_use]
    pub fn cloudlink_snapshot_digest(&self) -> String {
        format!("fx64:{:016x}", self.digest)
    }

    /// Returns the committed point/health IO epoch paired with the snapshot.
    #[must_use]
    pub fn publication_epoch(&self) -> u64 {
        self.read.publication_epoch()
    }
}

/// Atomically publishes complete Uplink generations after dual-plane validation.
pub struct UplinkTopologyHandle {
    current: ArcSwap<UplinkTopologyGeneration>,
    refresh_gate: Mutex<()>,
}

impl UplinkTopologyHandle {
    /// Creates an offline-first generation from exactly one SQLite snapshot.
    pub fn new_lazy(snapshot: SqliteLiveTopologySnapshot, config: &EnvConfig) -> PortResult<Self> {
        let initial = Arc::new(build_uplink_generation(snapshot, config, false)?);
        Ok(Self {
            current: ArcSwap::new(initial),
            refresh_gate: Mutex::new(()),
        })
    }

    /// Pins one complete generation for a cloud read or scheduler/upload pass.
    #[must_use]
    pub fn load(&self) -> Arc<UplinkTopologyGeneration> {
        self.current.load_full()
    }

    /// Loads one authoritative SQLite snapshot, validates the matching
    /// committed point/health pair, and atomically publishes both together.
    pub async fn refresh(&self, pool: &SqlitePool, config: &EnvConfig) -> PortResult<bool> {
        let _refresh = self.refresh_gate.lock().await;
        let snapshot = load_sqlite_live_topology(pool).await?;
        let candidate = Arc::new(build_uplink_generation(snapshot, config, true)?);
        let current = self.load();
        if current.digest() == candidate.digest()
            && current.publication_epoch() == candidate.publication_epoch()
            && current.read.point_writer_generation() == candidate.read.point_writer_generation()
            && current.read.health_writer_generation() == candidate.read.health_writer_generation()
        {
            return Ok(false);
        }
        candidate
            .read
            .with_validated_authority(|| self.current.store(Arc::clone(&candidate)))?;
        Ok(true)
    }
}

/// Keeps the service generation reconciled with SQLite and committed IO state.
pub async fn run_topology_refresher(
    topology: Arc<UplinkTopologyHandle>,
    pool: SqlitePool,
    config: Arc<EnvConfig>,
    shutdown: CancellationToken,
) {
    let interval = Duration::from_millis(config.shm_topology_refresh_interval_ms.max(100));
    let mut ticker = tokio::time::interval(interval);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            _ = shutdown.cancelled() => break,
            _ = ticker.tick() => match topology.refresh(&pool, &config).await {
                Ok(true) => {
                    let generation = topology.load();
                    tracing::info!(
                        digest = generation.digest(),
                        publication_epoch = generation.publication_epoch(),
                        "Uplink live topology generation refreshed"
                    );
                },
                Ok(false) => {},
                Err(error) => tracing::warn!(
                    retryable = error.is_retryable(),
                    "Uplink live topology refresh retained the previous generation: {error}"
                ),
            },
        }
    }
}

impl ShmNetValueSource {
    #[must_use]
    pub fn new(slots: Arc<dyn SlotSource>, groups: Vec<LogicalGroup>) -> Self {
        Self {
            slots,
            groups: groups
                .into_iter()
                .map(|group| (group.key.clone(), group))
                .collect(),
        }
    }

    /// Reads one logical group, optionally restricted to a single field.
    pub fn read_group(
        &self,
        key: &str,
        field: Option<&str>,
    ) -> PortResult<Option<HashMap<String, serde_json::Value>>> {
        let Some(group) = self.groups.get(key) else {
            return Ok(None);
        };
        let slot_count = self.slots.slot_count()?;
        let mut values = HashMap::new();
        for (point_id, &slot) in &group.points {
            if field.is_some_and(|field| field != point_id) {
                continue;
            }
            if slot >= slot_count {
                return Err(PortError::new(
                    PortErrorKind::InvalidData,
                    format!("logical point {key}:{point_id} maps outside SHM slot_count"),
                ));
            }
            let Some(sample) = self.slots.read_slot(slot)? else {
                continue;
            };
            if sample.value().is_nan() {
                continue;
            }
            if !sample.value().is_finite() {
                return Err(PortError::new(
                    PortErrorKind::InvalidData,
                    format!("logical point {key}:{point_id} is non-finite"),
                ));
            }
            let value = serde_json::Number::from_f64(sample.value())
                .map(serde_json::Value::Number)
                .ok_or_else(|| {
                    PortError::new(
                        PortErrorKind::InvalidData,
                        format!("logical point {key}:{point_id} cannot be encoded"),
                    )
                })?;
            values.insert(point_id.clone(), value);
        }
        Ok(Some(values))
    }

    /// Reads selected logical groups into the existing MQTT property shape.
    pub fn collect_entries(
        &self,
        patterns: &[String],
        excludes: &[Regex],
    ) -> PortResult<Vec<PropertyEntry>> {
        let selectors = compile_globs(patterns);
        let mut entries = Vec::new();
        for group in self.groups.values() {
            if !selectors
                .iter()
                .any(|selector| selector.is_match(&group.key))
                || excludes.iter().any(|exclude| exclude.is_match(&group.key))
            {
                continue;
            }
            let Some(value) = self.read_group(&group.key, None)? else {
                continue;
            };
            if value.is_empty() {
                continue;
            }
            entries.push(PropertyEntry {
                source: group.source.clone(),
                device: group.device.replace(' ', "_"),
                data_type: group.data_type.clone(),
                value,
            });
        }
        Ok(entries)
    }

    /// Reads selected instance measurement groups as truthful `PointSample`s.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn collect_point_samples(
        &self,
        patterns: &[String],
        excludes: &[Regex],
    ) -> PortResult<Vec<PointSample>> {
        let selectors = compile_globs(patterns);
        let slot_count = self.slots.slot_count()?;
        let mut samples = Vec::new();
        for group in self.groups.values() {
            if group.source != "inst"
                || group.data_type != "M"
                || !selectors
                    .iter()
                    .any(|selector| selector.is_match(&group.key))
                || excludes.iter().any(|exclude| exclude.is_match(&group.key))
            {
                continue;
            }
            let instance_id = group.device.parse::<u32>().map_err(|_| {
                PortError::new(
                    PortErrorKind::InvalidData,
                    format!("logical group {} has a non-numeric instance ID", group.key),
                )
            })?;
            for (point_id, &slot) in &group.points {
                if slot >= slot_count {
                    return Err(PortError::new(
                        PortErrorKind::InvalidData,
                        format!(
                            "logical point {}:{point_id} maps outside SHM slot_count",
                            group.key
                        ),
                    ));
                }
                let point_id = point_id.parse::<u32>().map_err(|_| {
                    PortError::new(
                        PortErrorKind::InvalidData,
                        format!(
                            "logical point {}:{point_id} has a non-numeric point ID",
                            group.key
                        ),
                    )
                })?;
                let Some(sample) = self.slots.read_slot(slot)? else {
                    continue;
                };
                if sample.value().is_nan() {
                    continue;
                }
                if !sample.value().is_finite() {
                    return Err(PortError::new(
                        PortErrorKind::InvalidData,
                        format!("logical point {}:{point_id} is non-finite", group.key),
                    ));
                }
                samples.push(PointSample::new(
                    PointAddress::new(
                        InstanceId::new(instance_id),
                        PointKind::Telemetry,
                        PointId::new(point_id),
                    ),
                    sample.value(),
                    TimestampMs::new(sample.timestamp_ms()),
                    PointQuality::Good,
                ));
            }
        }
        Ok(samples)
    }
}

fn build_uplink_generation(
    snapshot: SqliteLiveTopologySnapshot,
    config: &EnvConfig,
    validate_physical: bool,
) -> PortResult<UplinkTopologyGeneration> {
    let digest = snapshot.digest();
    let groups = logical_groups_from_snapshot(&snapshot)?;
    let point_manifest = Arc::new(snapshot.point_manifest().clone());
    let health_manifest = Arc::new(snapshot.health_manifest().clone());
    let point_config = shm_client_config(&config.shm_path, point_manifest.layout_hash(), config);
    let health_config = shm_client_config(
        &config.channel_health_shm_path,
        health_manifest.layout_hash(),
        config,
    );
    let read = Arc::new(if validate_physical {
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
    let slots: Arc<dyn SlotSource> = read.point_source().clone();
    Ok(UplinkTopologyGeneration {
        read,
        values: ShmNetValueSource::new(slots, groups),
        digest,
    })
}

fn logical_groups_from_snapshot(
    snapshot: &SqliteLiveTopologySnapshot,
) -> PortResult<Vec<LogicalGroup>> {
    let manifest = snapshot.point_manifest();
    let mut groups = BTreeMap::<String, (String, String, String, BTreeMap<String, usize>)>::new();
    for &target in snapshot.configured_physical_points() {
        let slot = manifest.slot_for(target).ok_or_else(|| {
            PortError::new(
                PortErrorKind::Conflict,
                "configured physical point is absent from its snapshot manifest",
            )
        })?;
        add_physical_group_point(&mut groups, target, slot);
    }

    for (instance_id, point_id, target) in snapshot.measurement_routes() {
        add_routed_group_point(&mut groups, manifest, "M", instance_id, point_id, target)?;
    }
    for (instance_id, point_id, target) in snapshot.action_routes() {
        add_routed_group_point(&mut groups, manifest, "A", instance_id, point_id, target)?;
    }

    Ok(groups
        .into_values()
        .map(|(source, device, data_type, points)| {
            LogicalGroup::new(source, device, data_type, points)
        })
        .collect())
}

type GroupMap = BTreeMap<String, (String, String, String, BTreeMap<String, usize>)>;

fn add_group_point(
    groups: &mut GroupMap,
    source: &str,
    device: &str,
    data_type: &str,
    point_id: String,
    slot: usize,
) {
    let key = format!("{source}:{device}:{data_type}");
    groups
        .entry(key)
        .or_insert_with(|| {
            (
                source.to_string(),
                device.to_string(),
                data_type.to_string(),
                BTreeMap::new(),
            )
        })
        .3
        .insert(point_id, slot);
}

fn add_physical_group_point(groups: &mut GroupMap, target: PhysicalPointAddress, slot: usize) {
    add_group_point(
        groups,
        "io",
        &target.channel_id().get().to_string(),
        point_kind_code(target),
        target.point_id().get().to_string(),
        slot,
    );
}

fn add_routed_group_point(
    groups: &mut GroupMap,
    manifest: &aether_shm_bridge::ChannelPointManifest,
    instance_data_type: &str,
    instance_id: u32,
    logical_point_id: u32,
    target: PhysicalPointAddress,
) -> PortResult<()> {
    let slot = manifest.slot_for(target).ok_or_else(|| {
        PortError::new(
            PortErrorKind::Conflict,
            "uplink logical route is absent from its snapshot manifest",
        )
    })?;
    add_group_point(
        groups,
        "inst",
        &instance_id.to_string(),
        instance_data_type,
        logical_point_id.to_string(),
        slot,
    );
    Ok(())
}

fn point_kind_code(target: PhysicalPointAddress) -> &'static str {
    match target.kind() {
        aether_domain::PointKind::Telemetry => "T",
        aether_domain::PointKind::Status => "S",
        aether_domain::PointKind::Command => "C",
        aether_domain::PointKind::Action => "A",
    }
}

fn shm_client_config(path: &str, layout_hash: u64, config: &EnvConfig) -> ShmClientConfig {
    ShmClientConfig::new(path, layout_hash)
        .with_writer_stale_after(Duration::from_millis(config.shm_writer_stale_after_ms))
        .with_identity_check_interval(Duration::from_millis(config.shm_identity_check_interval_ms))
}

fn compile_globs(patterns: &[String]) -> Vec<Regex> {
    patterns
        .iter()
        .filter_map(|pattern| match logical_glob_regex(pattern) {
            Ok(regex) => Some(regex),
            Err(error) => {
                warn!("Invalid uplink logical selector '{pattern}': {error}");
                None
            },
        })
        .collect()
}

fn logical_glob_regex(pattern: &str) -> Result<Regex, regex::Error> {
    let mut regex = String::from("^");
    for character in pattern.chars() {
        match character {
            '*' => regex.push_str(".*"),
            '?' => regex.push('.'),
            literal => regex.push_str(&regex::escape(&literal.to_string())),
        }
    }
    regex.push('$');
    Regex::new(&regex)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Arc;

    use aether_ports::{PortError, PortErrorKind, PortResult};
    use aether_shm_bridge::SlotSnapshot;

    use super::*;

    struct StubSlots(HashMap<usize, SlotSnapshot>);

    impl SlotSource for StubSlots {
        fn slot_count(&self) -> PortResult<usize> {
            Ok(2)
        }

        fn read_slot(&self, index: usize) -> PortResult<Option<SlotSnapshot>> {
            if index >= 2 {
                return Err(PortError::new(
                    PortErrorKind::InvalidData,
                    "slot outside stub",
                ));
            }
            Ok(self.0.get(&index).copied())
        }
    }

    async fn config_pool() -> SqlitePool {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("open embedded config database");
        for statement in [
            "CREATE TABLE channels (channel_id INTEGER PRIMARY KEY)",
            "CREATE TABLE telemetry_points (channel_id INTEGER, point_id INTEGER)",
            "CREATE TABLE signal_points (channel_id INTEGER, point_id INTEGER)",
            "CREATE TABLE control_points (channel_id INTEGER, point_id INTEGER)",
            "CREATE TABLE adjustment_points (channel_id INTEGER, point_id INTEGER)",
            "CREATE TABLE measurement_routing (instance_id INTEGER, channel_id INTEGER, channel_type TEXT, channel_point_id INTEGER, measurement_id INTEGER, enabled BOOLEAN)",
            "CREATE TABLE action_routing (instance_id INTEGER, channel_id INTEGER, channel_type TEXT, channel_point_id INTEGER, action_id INTEGER, enabled BOOLEAN)",
            "INSERT INTO channels VALUES (10)",
            "INSERT INTO telemetry_points VALUES (10, 0)",
            "INSERT INTO measurement_routing VALUES (12, 10, 'T', 0, 5, TRUE)",
            "ALTER TABLE telemetry_points ADD COLUMN protocol_mappings TEXT NOT NULL DEFAULT '{}'",
        ] {
            sqlx::query(statement)
                .execute(&pool)
                .await
                .expect("create minimal uplink catalogue");
        }
        pool
    }

    #[tokio::test]
    async fn uplink_catalog_uses_exact_snapshot_points_without_protocol_mapping() {
        let pool = config_pool().await;
        sqlx::query("INSERT INTO telemetry_points (channel_id, point_id) VALUES (10, 2)")
            .execute(&pool)
            .await
            .expect("insert sparse physical point");
        let snapshot = aether_store_local::load_sqlite_live_topology(&pool)
            .await
            .expect("load one authoritative snapshot");

        let groups = logical_groups_from_snapshot(&snapshot).expect("build uplink catalogue");
        let physical = groups
            .iter()
            .find(|group| group.key == "io:10:T")
            .expect("physical telemetry group");

        assert_eq!(physical.points.keys().collect::<Vec<_>>(), vec!["0", "2"]);
        assert!(groups.iter().any(|group| {
            group.key == "inst:12:M" && group.points.keys().collect::<Vec<_>>() == vec!["5"]
        }));
    }

    #[tokio::test]
    async fn service_handle_replaces_digest_and_epoch_as_one_generation() {
        let pool = config_pool().await;
        let directory = tempfile::tempdir().expect("temporary directory");
        let point_path = directory.path().join("live.shm");
        let health_path = directory.path().join("health.shm");
        let config = EnvConfig {
            shm_path: point_path.to_string_lossy().into_owned(),
            channel_health_shm_path: health_path.to_string_lossy().into_owned(),
            ..Default::default()
        };
        let snapshot = aether_store_local::load_sqlite_live_topology(&pool)
            .await
            .expect("load initial snapshot");
        let points = Arc::new(snapshot.point_manifest().clone());
        let health = Arc::new(snapshot.health_manifest().clone());
        let point_writer = aether_shm_bridge::ShmWriterHandle::create_published_at_epoch(
            aether_shm_bridge::ShmRuntimeConfig::new(&point_path, 32),
            Arc::clone(&points),
            None,
            10,
        )
        .expect("publish point plane");
        let health_writer = aether_shm_bridge::ShmChannelHealthWriterHandle::create_at_epoch(
            &health_path,
            Arc::clone(&health),
            10,
        )
        .expect("publish health plane");
        aether_shm_bridge::commit_topology_publication(&point_path, &health_path, 10)
            .expect("commit initial topology");
        point_writer
            .generation()
            .expect("initial point generation")
            .acquisition_writer()
            .update_heartbeat(aether_shm_bridge::timestamp_ms());
        let handle = UplinkTopologyHandle::new_lazy(snapshot, &config).expect("build lazy handle");

        assert!(handle.refresh(&pool, &config).await.expect("first refresh"));
        let pinned = handle.load();
        assert_eq!(pinned.publication_epoch(), 10);
        assert_eq!(
            pinned.cloudlink_snapshot_digest(),
            format!("fx64:{:016x}", pinned.digest())
        );

        sqlx::query("UPDATE measurement_routing SET measurement_id = 6")
            .execute(&pool)
            .await
            .expect("change only the logical route");
        assert!(
            handle
                .refresh(&pool, &config)
                .await
                .expect("routing-only refresh")
        );
        let routing_replacement = handle.load();
        assert_eq!(routing_replacement.publication_epoch(), 10);
        assert_ne!(routing_replacement.digest(), pinned.digest());
        assert!(
            routing_replacement
                .values
                .groups
                .get("inst:12:M")
                .is_some_and(|group| group.points.contains_key("6"))
        );

        health_writer
            .rebuild_for_publication(Arc::clone(&health), 11)
            .expect("publish only a replacement health plane");
        let partial_health = routing_replacement
            .read_group("io:10:T", None)
            .expect_err("a pinned upload generation must revalidate both physical planes");
        assert!(partial_health.is_retryable());

        point_writer
            .rebuild_for_publication(Arc::clone(&points), 12)
            .expect("restart point plane at same layout");
        assert!(handle.refresh(&pool, &config).await.is_err());
        assert!(Arc::ptr_eq(&routing_replacement, &handle.load()));
        health_writer
            .rebuild_for_publication(Arc::clone(&health), 12)
            .expect("restart health plane at same layout");
        aether_shm_bridge::commit_topology_publication(&point_path, &health_path, 12)
            .expect("commit restarted topology");

        assert!(handle.refresh(&pool, &config).await.expect("epoch refresh"));
        assert_eq!(pinned.publication_epoch(), 10);
        assert_eq!(handle.load().publication_epoch(), 12);
    }

    #[tokio::test]
    async fn service_handle_does_not_noop_a_reused_epoch_with_new_writer_generations() {
        let pool = config_pool().await;
        let directory = tempfile::tempdir().expect("temporary directory");
        let point_path = directory.path().join("live.shm");
        let health_path = directory.path().join("health.shm");
        let config = EnvConfig {
            shm_path: point_path.to_string_lossy().into_owned(),
            channel_health_shm_path: health_path.to_string_lossy().into_owned(),
            ..Default::default()
        };
        let snapshot = aether_store_local::load_sqlite_live_topology(&pool)
            .await
            .expect("load initial snapshot");
        let points = Arc::new(snapshot.point_manifest().clone());
        let health = Arc::new(snapshot.health_manifest().clone());
        let point_writer = aether_shm_bridge::ShmWriterHandle::create_published_at_epoch(
            aether_shm_bridge::ShmRuntimeConfig::new(&point_path, 32),
            Arc::clone(&points),
            None,
            500,
        )
        .expect("publish initial point plane");
        let health_writer = aether_shm_bridge::ShmChannelHealthWriterHandle::create_at_epoch(
            &health_path,
            Arc::clone(&health),
            500,
        )
        .expect("publish initial health plane");
        aether_shm_bridge::commit_topology_publication(&point_path, &health_path, 500)
            .expect("commit initial topology");
        let handle = UplinkTopologyHandle::new_lazy(snapshot, &config).expect("build lazy handle");
        assert!(
            handle
                .refresh(&pool, &config)
                .await
                .expect("pin initial view")
        );
        let stale = handle.load();
        drop(point_writer);
        drop(health_writer);

        let _replacement_point = aether_shm_bridge::ShmWriterHandle::create_published_at_epoch(
            aether_shm_bridge::ShmRuntimeConfig::new(&point_path, 32),
            Arc::clone(&points),
            None,
            500,
        )
        .expect("fault-inject a point writer that reused the epoch");
        let _replacement_health = aether_shm_bridge::ShmChannelHealthWriterHandle::create_at_epoch(
            &health_path,
            Arc::clone(&health),
            500,
        )
        .expect("fault-inject a health writer that reused the epoch");
        std::fs::remove_file(aether_shm_bridge::topology_commit_path_from_shm(
            &point_path,
        ))
        .expect("fault-inject loss of the old durable witness");
        aether_shm_bridge::commit_topology_publication(&point_path, &health_path, 500)
            .expect("fault-inject a valid replacement witness that reused the epoch");

        let refreshed =
            tokio::time::timeout(Duration::from_secs(2), handle.refresh(&pool, &config))
                .await
                .expect("same-epoch refresh must not deadlock");
        assert!(refreshed.expect("replace stale same-epoch view"));
        assert!(!Arc::ptr_eq(&stale, &handle.load()));
    }

    #[test]
    fn logical_group_is_read_from_shm() {
        let source = ShmNetValueSource::new(
            Arc::new(StubSlots(HashMap::from([
                (0, SlotSnapshot::new(42.5, 1_000)),
                (1, SlotSnapshot::new(7.0, 1_001)),
            ]))),
            vec![LogicalGroup::new("inst", "12", "M", [("5", 0), ("6", 1)])],
        );

        let values = source
            .read_group("inst:12:M", None)
            .expect("read group")
            .expect("configured group");

        assert_eq!(values["5"], 42.5);
        assert_eq!(values["6"], 7.0);
    }

    #[test]
    fn forwarder_patterns_select_logical_groups() {
        let source = ShmNetValueSource::new(
            Arc::new(StubSlots(HashMap::from([(
                0,
                SlotSnapshot::new(42.5, 1_000),
            )]))),
            vec![LogicalGroup::new("inst", "12", "M", [("5", 0)])],
        );

        let entries = source
            .collect_entries(&["inst:*:M".to_string()], &[])
            .expect("collect property entries");

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].device, "12");
        assert_eq!(entries[0].value["5"], 42.5);
    }

    #[test]
    fn cloudlink_samples_preserve_logical_address_value_and_source_timestamp() {
        let source = ShmNetValueSource::new(
            Arc::new(StubSlots(HashMap::from([(
                0,
                SlotSnapshot::new(42.5, 1_234),
            )]))),
            vec![LogicalGroup::new("inst", "12", "M", [("5", 0)])],
        );

        let samples = source
            .collect_point_samples(&["inst:*:M".to_string()], &[])
            .expect("collect CloudLink samples");

        assert_eq!(samples.len(), 1);
        assert_eq!(samples[0].address().instance_id(), InstanceId::new(12));
        assert_eq!(samples[0].address().point_id(), PointId::new(5));
        assert_eq!(samples[0].address().kind(), PointKind::Telemetry);
        assert_eq!(samples[0].value(), 42.5);
        assert_eq!(samples[0].timestamp(), TimestampMs::new(1_234));
        assert_eq!(samples[0].quality(), PointQuality::Good);
    }

    #[test]
    fn logical_glob_supports_wildcards_and_escapes_literals() {
        let regex = logical_glob_regex("inst:*:M?").expect("compile logical glob");

        assert!(regex.is_match("inst:12:M1"));
        assert!(!regex.is_match("inst:12:M"));
        assert!(!regex.is_match("inst.12:M1"));
    }

    #[tokio::test]
    async fn sqlite_catalog_discovers_cloud_groups_without_scan() {
        let pool = config_pool().await;
        let snapshot = load_sqlite_live_topology(&pool)
            .await
            .expect("load snapshot");
        let groups = logical_groups_from_snapshot(&snapshot).expect("load groups");

        assert!(groups.iter().any(|group| {
            group.key == "inst:12:M"
                && group.points.get("5")
                    == snapshot
                        .point_manifest()
                        .slot_for(PhysicalPointAddress::from_legacy_raw(
                            10,
                            aether_domain::PointKind::Telemetry,
                            0,
                        ))
                        .as_ref()
        }));
    }

    #[tokio::test]
    async fn production_source_builds_before_shm_writer() {
        let pool = config_pool().await;
        let config = EnvConfig {
            shm_path: std::env::temp_dir()
                .join(format!("uplink-missing-writer-{}", std::process::id()))
                .to_string_lossy()
                .into_owned(),
            ..Default::default()
        };

        let snapshot = load_sqlite_live_topology(&pool)
            .await
            .expect("load embedded snapshot");
        let handle = UplinkTopologyHandle::new_lazy(snapshot, &config)
            .expect("build with embedded config only");
        let error = handle
            .load()
            .read_group("inst:12:M", None)
            .expect_err("missing writer is a retryable read-time condition");

        assert!(error.is_retryable());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[ignore = "explicit long-running Uplink dynamic-config/IO-restart soak gate"]
    async fn dynamic_configuration_service_generation_soak() {
        let iterations = soak_usize("AETHER_DYNAMIC_CONFIG_SOAK_ITERATIONS", 400);
        let readers = soak_usize("AETHER_DYNAMIC_CONFIG_SOAK_READERS", 4);
        let restart_interval = soak_usize("AETHER_DYNAMIC_CONFIG_SOAK_RESTART_INTERVAL", 25);
        let timeout = Duration::from_secs(soak_u64("AETHER_DYNAMIC_CONFIG_SOAK_TIMEOUT_SECS", 900));
        assert!(iterations > 0, "Uplink soak needs at least one iteration");
        assert!(readers > 0, "Uplink soak needs at least one reader");
        assert!(restart_interval > 0, "restart interval must be non-zero");

        tokio::time::timeout(
            timeout,
            run_dynamic_configuration_soak(iterations, readers, restart_interval),
        )
        .await
        .unwrap_or_else(|_| panic!("Uplink dynamic configuration soak exceeded {timeout:?}"));
    }

    async fn run_dynamic_configuration_soak(
        iterations: usize,
        reader_count: usize,
        restart_interval: usize,
    ) {
        use std::sync::atomic::{AtomicUsize, Ordering};

        use aether_domain::{AcquiredPointSample, ChannelPointAddress, PointQuality, TimestampMs};
        use aether_shm_bridge::{
            ShmChannelHealthWriterHandle, ShmRuntimeConfig, ShmWriterHandle,
            begin_topology_publication,
        };
        use tokio_util::sync::CancellationToken;

        let pool = config_pool().await;
        let directory = tempfile::tempdir().expect("Uplink soak directory");
        let point_path = directory.path().join("live.shm");
        let health_path = directory.path().join("health.shm");
        let config = EnvConfig {
            shm_path: point_path.to_string_lossy().into_owned(),
            channel_health_shm_path: health_path.to_string_lossy().into_owned(),
            shm_identity_check_interval_ms: 0,
            ..Default::default()
        };

        let initial = load_sqlite_live_topology(&pool)
            .await
            .expect("initial Uplink topology");
        let mut epoch = 20_000_u64;
        let publication =
            begin_topology_publication(&point_path).expect("begin initial Uplink publication");
        let mut point_writer = ShmWriterHandle::create_published_at_epoch(
            ShmRuntimeConfig::new(&point_path, 64),
            Arc::new(initial.point_manifest().clone()),
            None,
            epoch,
        )
        .expect("initial Uplink point writer");
        write_uplink_soak_points(&point_writer, &initial, epoch);
        let mut health_writer = ShmChannelHealthWriterHandle::create_at_epoch(
            &health_path,
            Arc::new(initial.health_manifest().clone()),
            epoch,
        )
        .expect("initial Uplink health writer");
        write_uplink_soak_health(&health_writer, &initial, epoch);
        publication
            .commit(&health_path, epoch)
            .expect("commit initial Uplink publication");

        let handle = Arc::new(
            UplinkTopologyHandle::new_lazy(initial, &config).expect("build Uplink soak handle"),
        );
        assert!(
            handle
                .refresh(&pool, &config)
                .await
                .expect("pin initial Uplink publication")
        );
        assert_eq!(handle.load().publication_epoch(), epoch);

        let shutdown = CancellationToken::new();
        let progress = Arc::new(AtomicUsize::new(0));
        let reader_handles = (0..reader_count)
            .map(|_| {
                let handle = Arc::clone(&handle);
                let shutdown = shutdown.clone();
                let progress = Arc::clone(&progress);
                tokio::spawn(async move {
                    let mut coherent_reads = 0_usize;
                    while !shutdown.is_cancelled() {
                        let generation = handle.load();
                        match generation
                            .collect_entries(&["inst:*:M".to_string(), "io:*:T".to_string()], &[])
                        {
                            Ok(entries) => {
                                assert_uplink_batch_is_one_generation(&entries);
                                progress.fetch_add(1, Ordering::Relaxed);
                                coherent_reads += 1;
                            },
                            Err(error) => assert!(
                                error.is_retryable(),
                                "Uplink reader observed non-retryable publication error: {error}"
                            ),
                        }
                        tokio::task::yield_now().await;
                    }
                    coherent_reads
                })
            })
            .collect::<Vec<_>>();
        assert_eq!(reader_handles.len(), reader_count, "reader task bound");
        tokio::time::sleep(Duration::from_millis(20)).await;

        for iteration in 0..iterations {
            let logical_point_id = if iteration & 1 == 0 { 6 } else { 5 };
            let before_route = handle.load();
            sqlx::query("UPDATE measurement_routing SET measurement_id = ? WHERE instance_id = 12")
                .bind(logical_point_id)
                .execute(&pool)
                .await
                .expect("Uplink routing-only mutation");
            assert!(
                handle
                    .refresh(&pool, &config)
                    .await
                    .expect("Uplink routing-only refresh")
            );
            let routed = handle.load();
            assert_eq!(
                routed.publication_epoch(),
                epoch,
                "routing changed Uplink SHM epoch"
            );
            assert_ne!(routed.digest(), before_route.digest());
            assert_eq!(
                uplink_measurement_ids(&routed),
                vec![logical_point_id.to_string()],
                "Uplink published an inexact logical route generation"
            );

            let before_mapping = Arc::clone(&routed);
            sqlx::query(
                "UPDATE telemetry_points SET protocol_mappings = ? \
                 WHERE channel_id = 10 AND point_id = 0",
            )
            .bind(format!("{{\"json_path\":\"$.uplink_{iteration}\"}}"))
            .execute(&pool)
            .await
            .expect("Uplink protocol-mapping-only mutation");
            assert!(
                !handle
                    .refresh(&pool, &config)
                    .await
                    .expect("Uplink mapping-only refresh"),
                "protocol mapping entered the Uplink service manifest"
            );
            assert!(
                Arc::ptr_eq(&before_mapping, &handle.load()),
                "protocol mapping replaced the Uplink generation"
            );

            let extra_point_present = iteration & 1 == 0;
            if extra_point_present {
                sqlx::query(
                    "INSERT OR IGNORE INTO telemetry_points \
                     (channel_id, point_id, protocol_mappings) VALUES (10, 2, '{}')",
                )
                .execute(&pool)
                .await
                .expect("add Uplink layout point");
            } else {
                sqlx::query("DELETE FROM telemetry_points WHERE channel_id = 10 AND point_id = 2")
                    .execute(&pool)
                    .await
                    .expect("remove Uplink layout point");
            }
            let snapshot = load_sqlite_live_topology(&pool)
                .await
                .expect("load switched Uplink topology");
            epoch += 1;
            let publication =
                begin_topology_publication(&point_path).expect("begin Uplink topology publication");

            if (iteration + 1) % restart_interval == 0 {
                drop(point_writer);
                drop(health_writer);
                point_writer = ShmWriterHandle::create_published_at_epoch(
                    ShmRuntimeConfig::new(&point_path, 64),
                    Arc::new(snapshot.point_manifest().clone()),
                    None,
                    epoch,
                )
                .expect("restart Uplink point writer");
                write_uplink_soak_points(&point_writer, &snapshot, epoch);
                drop(publication);
                assert_uplink_partial_publication_fails_closed(&handle, &pool, &config).await;
                health_writer = ShmChannelHealthWriterHandle::create_at_epoch(
                    &health_path,
                    Arc::new(snapshot.health_manifest().clone()),
                    epoch,
                )
                .expect("restart Uplink health writer");
            } else {
                point_writer
                    .rebuild_for_publication(Arc::new(snapshot.point_manifest().clone()), epoch)
                    .expect("publish Uplink point plane");
                write_uplink_soak_points(&point_writer, &snapshot, epoch);
                drop(publication);
                assert_uplink_partial_publication_fails_closed(&handle, &pool, &config).await;
                health_writer
                    .rebuild_for_publication(Arc::new(snapshot.health_manifest().clone()), epoch)
                    .expect("publish Uplink health plane");
            }
            write_uplink_soak_health(&health_writer, &snapshot, epoch);
            aether_shm_bridge::commit_topology_publication(&point_path, &health_path, epoch)
                .expect("commit Uplink topology switch");
            assert!(
                handle
                    .refresh(&pool, &config)
                    .await
                    .expect("recover Uplink generation")
            );
            let recovered = handle.load();
            assert_eq!(recovered.publication_epoch(), epoch);
            assert_eq!(
                recovered.read.point_manifest().slot_count(),
                snapshot.point_manifest().slot_count()
            );
            assert_eq!(
                uplink_measurement_ids(&recovered),
                vec![logical_point_id.to_string()]
            );
        }

        shutdown.cancel();
        for reader in reader_handles {
            let coherent_reads = tokio::time::timeout(Duration::from_secs(5), reader)
                .await
                .expect("Uplink reader stopped within the task bound")
                .expect("Uplink reader task completed");
            assert!(
                coherent_reads > 0,
                "Uplink reader made no coherent progress"
            );
        }
        assert!(
            progress.load(Ordering::Relaxed) >= reader_count,
            "every Uplink reader must make coherent progress"
        );
        let retained = handle.load();
        assert_eq!(
            Arc::strong_count(&retained),
            2,
            "Uplink retained more than its current service generation"
        );
        assert_soak_files_are_bounded(directory.path());

        fn write_uplink_soak_points(
            writer: &ShmWriterHandle,
            snapshot: &SqliteLiveTopologySnapshot,
            epoch: u64,
        ) {
            let samples = snapshot
                .configured_physical_points()
                .iter()
                .copied()
                .map(|target| {
                    let address = ChannelPointAddress::new(
                        target.channel_id(),
                        target.kind(),
                        target.point_id(),
                    )
                    .expect("valid Uplink soak address");
                    AcquiredPointSample::new(
                        address,
                        epoch as f64,
                        epoch as f64,
                        TimestampMs::new(aether_shm_bridge::timestamp_ms()),
                        PointQuality::Good,
                    )
                    .expect("valid Uplink soak sample")
                })
                .collect::<Vec<_>>();
            writer
                .generation()
                .expect("Uplink point generation")
                .acquisition_writer()
                .commit_batch(&samples)
                .expect("write Uplink epoch samples");
        }

        fn write_uplink_soak_health(
            writer: &ShmChannelHealthWriterHandle,
            snapshot: &SqliteLiveTopologySnapshot,
            epoch: u64,
        ) {
            for channel_id in snapshot.health_manifest().channel_ids() {
                writer
                    .set_online(
                        channel_id,
                        epoch & 1 != 0,
                        aether_shm_bridge::timestamp_ms(),
                    )
                    .expect("write Uplink epoch health");
            }
        }
    }

    async fn assert_uplink_partial_publication_fails_closed(
        handle: &UplinkTopologyHandle,
        pool: &SqlitePool,
        config: &EnvConfig,
    ) {
        let retained = handle.load();
        let error = retained
            .read_group("io:10:T", None)
            .expect_err("Uplink must fail closed during point/health partial publication");
        assert!(
            error.is_retryable(),
            "unexpected partial read error: {error}"
        );
        assert!(handle.refresh(pool, config).await.is_err());
        assert!(
            Arc::ptr_eq(&retained, &handle.load()),
            "partial publication replaced the last Uplink generation"
        );
    }

    fn assert_uplink_batch_is_one_generation(entries: &[PropertyEntry]) {
        if entries.is_empty() {
            return;
        }
        let measurement = entries
            .iter()
            .find(|entry| entry.source == "inst" && entry.device == "12")
            .expect("Uplink measurement group");
        let measurement_ids = measurement
            .value
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>();
        assert!(
            matches!(measurement_ids.as_slice(), ["5"] | ["6"]),
            "mixed Uplink route generation: {measurement_ids:?}"
        );
        let physical = entries
            .iter()
            .find(|entry| entry.source == "io" && entry.device == "10")
            .expect("Uplink physical group");
        assert!(
            matches!(physical.value.len(), 1 | 2),
            "Uplink exposed holes or lost configured points: {}",
            physical.value.len()
        );
        let values = entries
            .iter()
            .flat_map(|entry| entry.value.values())
            .filter_map(serde_json::Value::as_f64)
            .collect::<Vec<_>>();
        let first = values[0];
        assert!(
            values.iter().all(|value| *value == first),
            "Uplink batch mixed SHM epochs"
        );
    }

    fn uplink_measurement_ids(generation: &UplinkTopologyGeneration) -> Vec<String> {
        generation
            .values
            .groups
            .get("inst:12:M")
            .expect("Uplink measurement route")
            .points
            .keys()
            .cloned()
            .collect()
    }

    fn soak_usize(name: &str, default: usize) -> usize {
        std::env::var(name)
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(default)
    }

    fn soak_u64(name: &str, default: u64) -> u64 {
        std::env::var(name)
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(default)
    }

    fn assert_soak_files_are_bounded(directory: &std::path::Path) {
        let entries = std::fs::read_dir(directory)
            .expect("read Uplink soak directory")
            .collect::<Result<Vec<_>, _>>()
            .expect("read Uplink soak entries");
        assert!(
            entries
                .iter()
                .all(|entry| !entry.file_name().to_string_lossy().contains(".staging")),
            "Uplink publication staging files accumulated"
        );
        assert!(
            entries.len() <= 6,
            "Uplink topology files grew without bound: {:?}",
            entries
                .iter()
                .map(std::fs::DirEntry::file_name)
                .collect::<Vec<_>>()
        );
    }
}
