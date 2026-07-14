//! Historical sampling from one coherent SQLite/SHM topology generation.
//!
//! SQLite supplies exact configured physical points plus enabled logical
//! measurement/action routes. A collection batch pins one immutable generation
//! and never consults protocol mappings.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use aether_domain::PointKind;
use aether_ports::{PortError, PortErrorKind, PortResult};
use aether_shm_bridge::{
    ChannelPointManifest, PhysicalPointAddress, ShmClientConfig, ShmReadTopologyGeneration,
    SlotSource,
};
use aether_store_local::{SqliteLiveTopologySnapshot, load_sqlite_live_topology};
use anyhow::Context;
use arc_swap::ArcSwap;
use chrono::{DateTime, Utc};
use regex::Regex;
use sqlx::SqlitePool;
use tracing::warn;

use crate::config::EnvConfig;
use crate::models::{DataPoint, PatternEntry, ServiceConfig};

#[derive(Debug, Clone, PartialEq, Eq)]
struct HistorySeries {
    logical_key: String,
    point_id: String,
    slot: usize,
}

/// Service-owned history view with atomically replaceable coherent generations.
pub struct ShmHistoryCollector {
    current: ArcSwap<HistoryGeneration>,
}

struct HistoryGeneration {
    slots: Arc<dyn SlotSource>,
    series: Arc<[HistorySeries]>,
    topology: Option<Arc<ShmReadTopologyGeneration>>,
    point_slot_count: usize,
    digest: u64,
}

impl ShmHistoryCollector {
    #[cfg(test)]
    fn new<S>(slots: Arc<S>, series: Vec<HistorySeries>) -> Self
    where
        S: SlotSource,
    {
        let point_slot_count = slots.slot_count().unwrap_or_default();
        Self {
            current: ArcSwap::from_pointee(HistoryGeneration {
                slots,
                series: series.into(),
                topology: None,
                point_slot_count,
                digest: 0,
            }),
        }
    }

    /// Samples all selected series from one pinned immutable generation.
    ///
    /// Any SHM validation/read error rejects the entire batch. `None` and NaN
    /// represent an uninitialised point and are omitted without failing it.
    pub fn collect_patterns(
        &self,
        cfg: &ServiceConfig,
        patterns: &[PatternEntry],
    ) -> PortResult<Vec<DataPoint>> {
        let generation = self.current.load_full();
        generation.collect_patterns(cfg, patterns)
    }

    /// Reloads one authoritative SQLite snapshot and atomically publishes it
    /// only while its paired point/health SHM publication remains committed.
    pub async fn refresh_topology(
        &self,
        pool: &SqlitePool,
        config: &EnvConfig,
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

        if physical_current && physical_layout_matches(&current, &snapshot) {
            let topology = Arc::clone(current.topology.as_ref().ok_or_else(|| {
                PortError::new(
                    PortErrorKind::Permanent,
                    "validated history generation is missing its SHM topology",
                )
            })?);
            let next = Arc::new(HistoryGeneration {
                slots: Arc::clone(&current.slots),
                series: history_series_from_snapshot(&snapshot)?.into(),
                topology: Some(Arc::clone(&topology)),
                point_slot_count: snapshot.point_manifest().slot_count(),
                digest: snapshot.digest(),
            });
            topology.with_validated_authority(|| self.current.store(next))?;
            return Ok(true);
        }

        let config = config.clone();
        let candidate = tokio::task::spawn_blocking(move || {
            build_history_generation(snapshot, &config, TopologyOpenMode::ValidatePhysical)
        })
        .await
        .map_err(|error| {
            PortError::new(
                PortErrorKind::Unavailable,
                format!("history topology validation task failed: {error}"),
            )
        })??;
        let candidate = Arc::new(candidate);
        let topology = Arc::clone(candidate.topology.as_ref().ok_or_else(|| {
            PortError::new(
                PortErrorKind::Permanent,
                "validated history candidate is missing its SHM topology",
            )
        })?);
        topology.with_validated_authority(|| self.current.store(candidate))?;
        Ok(true)
    }
}

impl HistoryGeneration {
    fn collect_patterns(
        &self,
        cfg: &ServiceConfig,
        patterns: &[PatternEntry],
    ) -> PortResult<Vec<DataPoint>> {
        let selectors = compile_globs(patterns);
        if selectors.is_empty() {
            return Ok(Vec::new());
        }
        if let Some(topology) = &self.topology {
            topology.validate_layouts()?;
        }

        let exclude_regexes = compile_excludes(&cfg.exclude_patterns);
        let slot_count = self.slots.slot_count()?;
        if slot_count != self.point_slot_count {
            return Err(PortError::new(
                PortErrorKind::Conflict,
                format!(
                    "history generation expects {} SHM slots but the pinned source exposes {slot_count}",
                    self.point_slot_count
                ),
            ));
        }

        let mut points = Vec::new();
        for series in self.series.iter().filter(|series| {
            selectors
                .iter()
                .any(|selector| selector.is_match(&series.logical_key))
                && !exclude_regexes
                    .iter()
                    .any(|exclude| exclude.is_match(&series.logical_key))
        }) {
            if series.slot >= slot_count {
                return Err(PortError::new(
                    PortErrorKind::InvalidData,
                    format!(
                        "history series {}:{} maps to slot {}, outside slot_count {slot_count}",
                        series.logical_key, series.point_id, series.slot
                    ),
                ));
            }
            let Some(sample) = self.slots.read_slot(series.slot)? else {
                continue;
            };
            if sample.value().is_nan() {
                continue;
            }
            if !sample.value().is_finite() {
                return Err(PortError::new(
                    PortErrorKind::InvalidData,
                    format!(
                        "history SHM slot {} contains a non-finite value",
                        series.slot
                    ),
                ));
            }
            let timestamp_ms = i64::try_from(sample.timestamp_ms()).map_err(|_| {
                PortError::new(
                    PortErrorKind::InvalidData,
                    format!("history SHM slot {} timestamp exceeds i64", series.slot),
                )
            })?;
            let time = DateTime::<Utc>::from_timestamp_millis(timestamp_ms).ok_or_else(|| {
                PortError::new(
                    PortErrorKind::InvalidData,
                    format!("history SHM slot {} timestamp is invalid", series.slot),
                )
            })?;
            points.push(DataPoint {
                time,
                series_key: series.logical_key.clone(),
                point_id: series.point_id.clone(),
                value: Some(sample.value()),
                string_value: None,
            });
        }
        Ok(points)
    }
}

/// Builds the service-owned history topology. Physical files are opened lazily
/// so the service can start before IO; batches remain fail-closed until a
/// committed dual-plane publication exists.
pub async fn build_shm_history_collector(
    pool: &SqlitePool,
    config: &EnvConfig,
) -> anyhow::Result<Arc<ShmHistoryCollector>> {
    let snapshot = load_sqlite_live_topology(pool)
        .await
        .context("load canonical live topology for history")?;
    let generation = build_history_generation(snapshot, config, TopologyOpenMode::Lazy)
        .context("compose lazy history SHM topology")?;
    Ok(Arc::new(ShmHistoryCollector {
        current: ArcSwap::from_pointee(generation),
    }))
}

/// Reconciles the service generation against SQLite and committed IO SHM.
/// A failed candidate never replaces the last coherent generation.
pub async fn run_history_topology_refresh(
    collector: Arc<ShmHistoryCollector>,
    pool: SqlitePool,
    config: EnvConfig,
    shutdown: tokio_util::sync::CancellationToken,
) {
    let refresh_interval = Duration::from_millis(config.shm_topology_refresh_interval_ms.max(100));
    let mut ticker = tokio::time::interval(refresh_interval);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        tokio::select! {
            _ = shutdown.cancelled() => return,
            _ = ticker.tick() => match collector.refresh_topology(&pool, &config).await {
                Ok(true) => tracing::info!("History live topology generation refreshed"),
                Ok(false) => {},
                Err(error) => warn!(
                    retryable = error.is_retryable(),
                    "History topology refresh retained the previous generation: {error}"
                ),
            },
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum TopologyOpenMode {
    Lazy,
    ValidatePhysical,
}

fn build_history_generation(
    snapshot: SqliteLiveTopologySnapshot,
    config: &EnvConfig,
    mode: TopologyOpenMode,
) -> PortResult<HistoryGeneration> {
    let digest = snapshot.digest();
    let series: Arc<[HistorySeries]> = history_series_from_snapshot(&snapshot)?.into();
    let (point_manifest, health_manifest, _, _) = snapshot.into_parts();
    let point_manifest = Arc::new(point_manifest);
    let health_manifest = Arc::new(health_manifest);
    let point_slot_count = point_manifest.slot_count();
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
            point_manifest,
            health_manifest,
        )?,
        TopologyOpenMode::ValidatePhysical => ShmReadTopologyGeneration::open(
            point_config,
            health_config,
            point_manifest,
            health_manifest,
        )?,
    });
    let slots: Arc<dyn SlotSource> = topology.point_source().clone();
    Ok(HistoryGeneration {
        slots,
        series,
        topology: Some(topology),
        point_slot_count,
        digest,
    })
}

fn history_series_from_snapshot(
    snapshot: &SqliteLiveTopologySnapshot,
) -> PortResult<Vec<HistorySeries>> {
    let manifest = snapshot.point_manifest();
    let mut series = BTreeMap::<(String, String), usize>::new();

    for target in snapshot.configured_physical_points().iter().copied() {
        add_series(
            &mut series,
            manifest,
            format!(
                "io:{}:{}",
                target.channel_id().get(),
                point_kind_code(target.kind())
            ),
            target.point_id().get(),
            target,
        )?;
    }
    for (instance_id, point_id, target) in snapshot.measurement_routes() {
        add_series(
            &mut series,
            manifest,
            format!("inst:{instance_id}:M"),
            point_id,
            target,
        )?;
    }
    for (instance_id, point_id, target) in snapshot.action_routes() {
        add_series(
            &mut series,
            manifest,
            format!("inst:{instance_id}:A"),
            point_id,
            target,
        )?;
    }

    Ok(series
        .into_iter()
        .map(|((logical_key, point_id), slot)| HistorySeries {
            logical_key,
            point_id,
            slot,
        })
        .collect())
}

fn add_series(
    series: &mut BTreeMap<(String, String), usize>,
    manifest: &ChannelPointManifest,
    logical_key: String,
    logical_point_id: u32,
    target: PhysicalPointAddress,
) -> PortResult<()> {
    let slot = manifest.slot_for(target).ok_or_else(|| {
        PortError::new(
            PortErrorKind::InvalidData,
            format!("history route target {target:?} is absent from its SQLite point manifest"),
        )
    })?;
    if series
        .insert((logical_key.clone(), logical_point_id.to_string()), slot)
        .is_some()
    {
        return Err(PortError::new(
            PortErrorKind::InvalidData,
            format!("history topology contains duplicate series {logical_key}:{logical_point_id}"),
        ));
    }
    Ok(())
}

fn physical_layout_matches(
    current: &HistoryGeneration,
    snapshot: &SqliteLiveTopologySnapshot,
) -> bool {
    current.topology.as_ref().is_some_and(|topology| {
        topology.point_manifest().layout_hash() == snapshot.point_manifest().layout_hash()
            && topology.point_manifest().slot_count() == snapshot.point_manifest().slot_count()
            && topology.health_manifest().layout_hash() == snapshot.health_manifest().layout_hash()
            && topology.health_manifest().slot_count() == snapshot.health_manifest().slot_count()
    })
}

fn shm_client_config(path: &str, layout_hash: u64, config: &EnvConfig) -> ShmClientConfig {
    ShmClientConfig::new(path, layout_hash)
        .with_writer_stale_after(Duration::from_millis(config.shm_writer_stale_after_ms))
        .with_identity_check_interval(Duration::from_millis(config.shm_identity_check_interval_ms))
}

const fn point_kind_code(kind: PointKind) -> &'static str {
    match kind {
        PointKind::Telemetry => "T",
        PointKind::Status => "S",
        PointKind::Command => "C",
        PointKind::Action => "A",
    }
}

fn compile_globs(patterns: &[PatternEntry]) -> Vec<Regex> {
    patterns
        .iter()
        .filter_map(|entry| match series_glob_regex(&entry.pattern) {
            Ok(regex) => Some(regex),
            Err(error) => {
                warn!("Invalid history selector '{}': {error}", entry.pattern);
                None
            },
        })
        .collect()
}

fn compile_excludes(patterns: &[String]) -> Vec<Regex> {
    patterns
        .iter()
        .filter_map(|pattern| {
            Regex::new(pattern)
                .map_err(|error| warn!("Invalid exclude pattern '{pattern}': {error}"))
                .ok()
        })
        .collect()
}

#[cfg(test)]
fn glob_matches(pattern: &str, candidate: &str) -> bool {
    series_glob_regex(pattern).is_ok_and(|regex| regex.is_match(candidate))
}

fn series_glob_regex(pattern: &str) -> Result<Regex, regex::Error> {
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

    use aether_shm_bridge::SlotSnapshot;

    use super::*;

    struct StubSlots {
        slot_count: usize,
        values: HashMap<usize, SlotSnapshot>,
        fail_at: Option<usize>,
    }

    impl SlotSource for StubSlots {
        fn slot_count(&self) -> PortResult<usize> {
            Ok(self.slot_count)
        }

        fn read_slot(&self, index: usize) -> PortResult<Option<SlotSnapshot>> {
            if self.fail_at == Some(index) {
                return Err(PortError::new(
                    PortErrorKind::Unavailable,
                    "injected batch read failure",
                ));
            }
            if index >= self.slot_count {
                return Err(PortError::new(
                    PortErrorKind::InvalidData,
                    "slot outside stub",
                ));
            }
            Ok(self.values.get(&index).copied())
        }
    }

    async fn config_pool() -> SqlitePool {
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
            "CREATE TABLE point_mappings (point_id INTEGER, protocol_address TEXT)",
            "INSERT INTO channels VALUES (10, 'virtual')",
            "INSERT INTO telemetry_points VALUES (10, 0)",
            "INSERT INTO telemetry_points VALUES (10, 2)",
            "INSERT INTO adjustment_points VALUES (10, 4)",
            "INSERT INTO measurement_routing VALUES (42, 10, 'T', 2, 7, TRUE)",
            "INSERT INTO action_routing VALUES (43, 10, 'A', 4, 8, TRUE)",
            "INSERT INTO point_mappings VALUES (0, 'deliberately-unused')",
            "ALTER TABLE telemetry_points ADD COLUMN protocol_mappings TEXT NOT NULL DEFAULT '{}'",
        ] {
            sqlx::query(statement)
                .execute(&pool)
                .await
                .expect("create minimal history catalogue");
        }
        pool
    }

    #[test]
    fn series_globs_match_sqlite_discovered_series_keys() {
        assert!(glob_matches("inst:*:M", "inst:42:M"));
        assert!(glob_matches("io:1?:T", "io:10:T"));
        assert!(!glob_matches("inst:*:A", "inst:42:M"));
    }

    #[test]
    fn collection_reads_finite_samples_from_shm() {
        let collector = ShmHistoryCollector::new(
            Arc::new(StubSlots {
                slot_count: 3,
                values: HashMap::from([
                    (0, SlotSnapshot::new(42.5, 1_720_000_000_000)),
                    (1, SlotSnapshot::new(f64::NAN, 1_720_000_000_001)),
                    (2, SlotSnapshot::new(7.0, 1_720_000_000_002)),
                ]),
                fail_at: None,
            }),
            vec![
                HistorySeries {
                    logical_key: "inst:1:M".to_string(),
                    point_id: "100".to_string(),
                    slot: 0,
                },
                HistorySeries {
                    logical_key: "inst:1:M".to_string(),
                    point_id: "101".to_string(),
                    slot: 1,
                },
                HistorySeries {
                    logical_key: "inst:1:A".to_string(),
                    point_id: "8".to_string(),
                    slot: 2,
                },
            ],
        );
        let cfg = ServiceConfig {
            subscribe_patterns: vec![PatternEntry::new("inst:*:M")],
            ..ServiceConfig::default()
        };

        let points = collector
            .collect_patterns(&cfg, &cfg.subscribe_patterns)
            .expect("collect one coherent batch");

        assert_eq!(points.len(), 1);
        assert_eq!(points[0].series_key, "inst:1:M");
        assert_eq!(points[0].point_id, "100");
        assert_eq!(points[0].value, Some(42.5));
    }

    #[test]
    fn collection_fails_the_whole_batch_when_one_selected_slot_read_fails() {
        let collector = ShmHistoryCollector::new(
            Arc::new(StubSlots {
                slot_count: 2,
                values: HashMap::from([
                    (0, SlotSnapshot::new(42.5, 1_720_000_000_000)),
                    (1, SlotSnapshot::new(7.0, 1_720_000_000_001)),
                ]),
                fail_at: Some(1),
            }),
            vec![
                HistorySeries {
                    logical_key: "io:10:T".to_string(),
                    point_id: "0".to_string(),
                    slot: 0,
                },
                HistorySeries {
                    logical_key: "io:10:T".to_string(),
                    point_id: "1".to_string(),
                    slot: 1,
                },
            ],
        );

        let result =
            collector.collect_patterns(&ServiceConfig::default(), &[PatternEntry::new("io:*:T")]);

        assert!(result.is_err());
    }

    #[tokio::test]
    async fn sqlite_snapshot_yields_only_exact_points_and_enabled_logical_routes() {
        let pool = config_pool().await;
        let snapshot = load_sqlite_live_topology(&pool)
            .await
            .expect("load one authoritative topology snapshot");

        let series = history_series_from_snapshot(&snapshot).expect("compose history catalogue");

        assert!(series.iter().any(|series| {
            series.logical_key == "io:10:T" && series.point_id == "0" && series.slot == 0
        }));
        assert!(series.iter().any(|series| {
            series.logical_key == "io:10:T" && series.point_id == "2" && series.slot == 2
        }));
        assert!(
            !series
                .iter()
                .any(|series| series.logical_key == "io:10:T" && series.point_id == "1")
        );
        assert!(series.iter().any(|series| {
            series.logical_key == "inst:42:M" && series.point_id == "7" && series.slot == 2
        }));
        assert!(
            series
                .iter()
                .any(|series| { series.logical_key == "inst:43:A" && series.point_id == "8" })
        );
    }

    #[tokio::test]
    async fn production_collector_builds_before_shm_writer_but_batches_fail_closed() {
        let pool = config_pool().await;
        let unique = format!("history-missing-writer-{}", std::process::id());
        let point_path = std::env::temp_dir().join(unique);
        let config = EnvConfig {
            shm_path: point_path.to_string_lossy().into_owned(),
            channel_health_shm_path: aether_shm_bridge::channel_health_path_from_shm(&point_path)
                .to_string_lossy()
                .into_owned(),
            ..Default::default()
        };

        let collector = build_shm_history_collector(&pool, &config)
            .await
            .expect("build with embedded config only");

        assert!(
            collector
                .collect_patterns(&ServiceConfig::default(), &[PatternEntry::new("inst:*:M")])
                .is_err()
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[ignore = "explicit long-running History dynamic-config/IO-restart soak gate"]
    async fn dynamic_configuration_service_generation_soak() {
        let iterations = soak_usize("AETHER_DYNAMIC_CONFIG_SOAK_ITERATIONS", 400);
        let readers = soak_usize("AETHER_DYNAMIC_CONFIG_SOAK_READERS", 4);
        let restart_interval = soak_usize("AETHER_DYNAMIC_CONFIG_SOAK_RESTART_INTERVAL", 25);
        let timeout = Duration::from_secs(soak_u64("AETHER_DYNAMIC_CONFIG_SOAK_TIMEOUT_SECS", 900));
        assert!(iterations > 0, "History soak needs at least one iteration");
        assert!(readers > 0, "History soak needs at least one reader");
        assert!(restart_interval > 0, "restart interval must be non-zero");

        tokio::time::timeout(
            timeout,
            run_dynamic_configuration_soak(iterations, readers, restart_interval),
        )
        .await
        .unwrap_or_else(|_| panic!("History dynamic configuration soak exceeded {timeout:?}"));
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
        let directory = tempfile::tempdir().expect("History soak directory");
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
            .expect("initial History topology");
        let mut epoch = 10_000_u64;
        let publication =
            begin_topology_publication(&point_path).expect("begin initial History publication");
        let mut point_writer = ShmWriterHandle::create_published_at_epoch(
            ShmRuntimeConfig::new(&point_path, 64),
            Arc::new(initial.point_manifest().clone()),
            None,
            epoch,
        )
        .expect("initial History point writer");
        write_history_soak_points(&point_writer, &initial, epoch);
        let mut health_writer = ShmChannelHealthWriterHandle::create_at_epoch(
            &health_path,
            Arc::new(initial.health_manifest().clone()),
            epoch,
        )
        .expect("initial History health writer");
        write_history_soak_health(&health_writer, &initial, epoch);
        publication
            .commit(&health_path, epoch)
            .expect("commit initial History publication");

        let collector = build_shm_history_collector(&pool, &config)
            .await
            .expect("build History soak collector");
        assert!(
            !collector
                .refresh_topology(&pool, &config)
                .await
                .expect("pin initial History publication")
        );
        assert_eq!(history_generation_epoch(&collector), epoch);

        let shutdown = CancellationToken::new();
        let progress = Arc::new(AtomicUsize::new(0));
        let patterns = Arc::new(vec![
            PatternEntry::new("inst:*:M"),
            PatternEntry::new("io:*:T"),
        ]);
        let reader_handles = (0..reader_count)
            .map(|_| {
                let collector = Arc::clone(&collector);
                let shutdown = shutdown.clone();
                let progress = Arc::clone(&progress);
                let patterns = Arc::clone(&patterns);
                tokio::spawn(async move {
                    let mut coherent_reads = 0_usize;
                    while !shutdown.is_cancelled() {
                        match collector
                            .collect_patterns(&ServiceConfig::default(), patterns.as_ref())
                        {
                            Ok(points) => {
                                assert_history_batch_is_one_generation(&points);
                                progress.fetch_add(1, Ordering::Relaxed);
                                coherent_reads += 1;
                            },
                            Err(error) => assert!(
                                error.is_retryable(),
                                "History reader observed non-retryable publication error: {error}"
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
            let logical_point_id = if iteration & 1 == 0 { 5 } else { 6 };
            let before_route = collector.current.load_full();
            sqlx::query("UPDATE measurement_routing SET measurement_id = ? WHERE instance_id = 42")
                .bind(logical_point_id)
                .execute(&pool)
                .await
                .expect("History routing-only mutation");
            assert!(
                collector
                    .refresh_topology(&pool, &config)
                    .await
                    .expect("History routing-only refresh")
            );
            let routed = collector.current.load_full();
            assert_eq!(history_epoch(&routed), epoch, "routing changed SHM epoch");
            assert_ne!(
                routed.digest, before_route.digest,
                "route digest did not change"
            );
            assert_eq!(
                history_measurement_ids(&routed),
                vec![logical_point_id.to_string()],
                "History published an inexact logical route generation"
            );

            let before_mapping = Arc::clone(&routed);
            sqlx::query(
                "UPDATE telemetry_points SET protocol_mappings = ? \
                 WHERE channel_id = 10 AND point_id = 0",
            )
            .bind(format!("{{\"json_path\":\"$.history_{iteration}\"}}"))
            .execute(&pool)
            .await
            .expect("History protocol-mapping-only mutation");
            assert!(
                !collector
                    .refresh_topology(&pool, &config)
                    .await
                    .expect("History mapping-only refresh"),
                "protocol mapping entered the History service manifest"
            );
            assert!(
                Arc::ptr_eq(&before_mapping, &collector.current.load_full()),
                "protocol mapping replaced the History generation"
            );

            let extra_point_present = iteration & 1 == 0;
            if extra_point_present {
                sqlx::query(
                    "INSERT OR IGNORE INTO telemetry_points \
                     (channel_id, point_id, protocol_mappings) VALUES (10, 3, '{}')",
                )
                .execute(&pool)
                .await
                .expect("add History layout point");
            } else {
                sqlx::query("DELETE FROM telemetry_points WHERE channel_id = 10 AND point_id = 3")
                    .execute(&pool)
                    .await
                    .expect("remove History layout point");
            }
            let snapshot = load_sqlite_live_topology(&pool)
                .await
                .expect("load switched History topology");
            epoch += 1;
            let publication = begin_topology_publication(&point_path)
                .expect("begin History topology publication");

            if (iteration + 1) % restart_interval == 0 {
                drop(point_writer);
                drop(health_writer);
                point_writer = ShmWriterHandle::create_published_at_epoch(
                    ShmRuntimeConfig::new(&point_path, 64),
                    Arc::new(snapshot.point_manifest().clone()),
                    None,
                    epoch,
                )
                .expect("restart History point writer");
                write_history_soak_points(&point_writer, &snapshot, epoch);
                drop(publication);
                assert_history_partial_publication_fails_closed(&collector, &pool, &config).await;
                health_writer = ShmChannelHealthWriterHandle::create_at_epoch(
                    &health_path,
                    Arc::new(snapshot.health_manifest().clone()),
                    epoch,
                )
                .expect("restart History health writer");
            } else {
                point_writer
                    .rebuild_for_publication(Arc::new(snapshot.point_manifest().clone()), epoch)
                    .expect("publish History point plane");
                write_history_soak_points(&point_writer, &snapshot, epoch);
                drop(publication);
                assert_history_partial_publication_fails_closed(&collector, &pool, &config).await;
                health_writer
                    .rebuild_for_publication(Arc::new(snapshot.health_manifest().clone()), epoch)
                    .expect("publish History health plane");
            }
            write_history_soak_health(&health_writer, &snapshot, epoch);
            aether_shm_bridge::commit_topology_publication(&point_path, &health_path, epoch)
                .expect("commit History topology switch");
            assert!(
                collector
                    .refresh_topology(&pool, &config)
                    .await
                    .expect("recover History generation")
            );
            let recovered = collector.current.load_full();
            assert_eq!(history_epoch(&recovered), epoch);
            assert_eq!(
                recovered.point_slot_count,
                snapshot.point_manifest().slot_count()
            );
            assert_eq!(
                history_measurement_ids(&recovered),
                vec![logical_point_id.to_string()]
            );
        }

        shutdown.cancel();
        for handle in reader_handles {
            let coherent_reads = tokio::time::timeout(Duration::from_secs(5), handle)
                .await
                .expect("History reader stopped within the task bound")
                .expect("History reader task completed");
            assert!(
                coherent_reads > 0,
                "History reader made no coherent progress"
            );
        }
        assert!(
            progress.load(Ordering::Relaxed) >= reader_count,
            "every History reader must make coherent progress"
        );
        let retained = collector.current.load_full();
        assert_eq!(
            Arc::strong_count(&retained),
            2,
            "History retained more than its current service generation"
        );
        assert_soak_files_are_bounded(directory.path());

        fn write_history_soak_points(
            writer: &ShmWriterHandle,
            snapshot: &SqliteLiveTopologySnapshot,
            epoch: u64,
        ) {
            let samples = snapshot
                .configured_physical_points()
                .iter()
                .copied()
                .filter(|target| matches!(target.kind(), PointKind::Telemetry | PointKind::Status))
                .map(|target| {
                    let address = ChannelPointAddress::new(
                        target.channel_id(),
                        target.kind(),
                        target.point_id(),
                    )
                    .expect("valid History soak address");
                    AcquiredPointSample::new(
                        address,
                        epoch as f64,
                        epoch as f64,
                        TimestampMs::new(aether_shm_bridge::timestamp_ms()),
                        PointQuality::Good,
                    )
                    .expect("valid History soak sample")
                })
                .collect::<Vec<_>>();
            writer
                .generation()
                .expect("History point generation")
                .acquisition_writer()
                .commit_batch(&samples)
                .expect("write History epoch samples");
        }

        fn write_history_soak_health(
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
                    .expect("write History epoch health");
            }
        }
    }

    async fn assert_history_partial_publication_fails_closed(
        collector: &ShmHistoryCollector,
        pool: &SqlitePool,
        config: &EnvConfig,
    ) {
        let error = collector
            .collect_patterns(&ServiceConfig::default(), &[PatternEntry::new("io:*:T")])
            .expect_err("History must fail closed during point/health partial publication");
        assert!(
            error.is_retryable(),
            "unexpected partial read error: {error}"
        );
        let retained = collector.current.load_full();
        assert!(collector.refresh_topology(pool, config).await.is_err());
        assert!(
            Arc::ptr_eq(&retained, &collector.current.load_full()),
            "partial publication replaced the last History generation"
        );
    }

    fn assert_history_batch_is_one_generation(points: &[DataPoint]) {
        if points.is_empty() {
            return;
        }
        let measurement_ids = points
            .iter()
            .filter(|point| point.series_key == "inst:42:M")
            .map(|point| point.point_id.as_str())
            .collect::<Vec<_>>();
        assert!(
            matches!(measurement_ids.as_slice(), ["5"] | ["6"] | ["7"]),
            "mixed History route generation: {measurement_ids:?}"
        );
        let physical_count = points
            .iter()
            .filter(|point| point.series_key == "io:10:T")
            .count();
        assert!(
            matches!(physical_count, 2 | 3),
            "History exposed holes or lost configured points: {physical_count}"
        );
        let first = points[0].value.expect("soak samples have values");
        assert!(
            points.iter().all(|point| point.value == Some(first)),
            "History batch mixed SHM epochs"
        );
    }

    fn history_measurement_ids(generation: &HistoryGeneration) -> Vec<String> {
        generation
            .series
            .iter()
            .filter(|series| series.logical_key == "inst:42:M")
            .map(|series| series.point_id.clone())
            .collect()
    }

    fn history_epoch(generation: &HistoryGeneration) -> u64 {
        generation
            .topology
            .as_ref()
            .expect("History soak topology")
            .publication_epoch()
    }

    fn history_generation_epoch(collector: &ShmHistoryCollector) -> u64 {
        history_epoch(&collector.current.load_full())
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
            .expect("read History soak directory")
            .collect::<Result<Vec<_>, _>>()
            .expect("read History soak entries");
        assert!(
            entries
                .iter()
                .all(|entry| !entry.file_name().to_string_lossy().contains(".staging")),
            "History publication staging files accumulated"
        );
        assert!(
            entries.len() <= 6,
            "History topology files grew without bound: {:?}",
            entries
                .iter()
                .map(std::fs::DirEntry::file_name)
                .collect::<Vec<_>>()
        );
    }
}
