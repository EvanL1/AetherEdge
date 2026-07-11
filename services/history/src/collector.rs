//! Historical sampling directly from the authoritative SHM data plane.
//!
//! SQLite supplies the configured series catalogue. Series glob patterns select
//! which logical keys are sampled from SHM.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use aether_domain::PointKind;
use aether_shm_bridge::{
    ChannelPointManifest, ReconnectingSlotSource, ShmClientConfig, SlotSource,
};
use anyhow::Context;
use chrono::{DateTime, Utc};
use regex::Regex;
use sqlx::SqlitePool;
use tracing::{debug, warn};

use crate::config::EnvConfig;
use crate::models::{DataPoint, PatternEntry, ServiceConfig};

#[derive(Debug, Clone, PartialEq, Eq)]
struct HistorySeries {
    logical_key: String,
    point_id: String,
    slot: usize,
}

/// Immutable sampling snapshot built from embedded configuration.
pub struct ShmHistoryCollector {
    slots: Arc<dyn SlotSource>,
    series: Vec<HistorySeries>,
}

impl ShmHistoryCollector {
    #[must_use]
    fn new<S>(slots: Arc<S>, series: Vec<HistorySeries>) -> Self
    where
        S: SlotSource,
    {
        Self { slots, series }
    }

    /// Samples every configured series selected by at least one supplied glob.
    #[must_use]
    pub fn collect_patterns(
        &self,
        cfg: &ServiceConfig,
        patterns: &[PatternEntry],
    ) -> Vec<DataPoint> {
        let selectors = compile_globs(patterns);
        if selectors.is_empty() {
            return Vec::new();
        }
        let exclude_regexes = compile_excludes(&cfg.exclude_patterns);
        let slot_count = match self.slots.slot_count() {
            Ok(slot_count) => slot_count,
            Err(error) => {
                warn!(
                    retryable = error.is_retryable(),
                    "Historical SHM source unavailable: {error}"
                );
                return Vec::new();
            },
        };

        self.series
            .iter()
            .filter(|series| {
                selectors
                    .iter()
                    .any(|selector| selector.is_match(&series.logical_key))
            })
            .filter(|series| {
                !exclude_regexes
                    .iter()
                    .any(|exclude| exclude.is_match(&series.logical_key))
            })
            .filter_map(|series| {
                if series.slot >= slot_count {
                    warn!(
                        "Historical series {}:{} maps outside SHM slot_count {}",
                        series.logical_key, series.point_id, slot_count
                    );
                    return None;
                }
                let sample = match self.slots.read_slot(series.slot) {
                    Ok(Some(sample)) => sample,
                    Ok(None) => return None,
                    Err(error) => {
                        debug!(
                            retryable = error.is_retryable(),
                            "Historical SHM read failed for {}:{}: {error}",
                            series.logical_key,
                            series.point_id
                        );
                        return None;
                    },
                };
                if !sample.value().is_finite() {
                    debug!(
                        "Skipping non-finite SHM value for {}:{}",
                        series.logical_key, series.point_id
                    );
                    return None;
                }
                let timestamp_ms = i64::try_from(sample.timestamp_ms()).ok()?;
                let time = DateTime::<Utc>::from_timestamp_millis(timestamp_ms)?;
                Some(DataPoint {
                    time,
                    series_key: series.logical_key.clone(),
                    point_id: series.point_id.clone(),
                    value: Some(sample.value()),
                    string_value: None,
                })
            })
            .collect()
    }
}

/// Rebuilds the catalogue from SQLite and creates a lazy SHM reader.
///
/// Rebuilding on each due collection discovers newly configured instances and
/// follows SHM layout generations.
pub async fn build_shm_history_collector(
    pool: &SqlitePool,
    config: &EnvConfig,
) -> anyhow::Result<ShmHistoryCollector> {
    let manifest = load_channel_manifest(pool).await?;
    let series = load_history_series(pool, &manifest).await?;
    let slots = Arc::new(ReconnectingSlotSource::new(
        ShmClientConfig::new(&config.shm_path, manifest.layout_hash())
            .with_writer_stale_after(Duration::from_millis(config.shm_writer_stale_after_ms))
            .with_identity_check_interval(Duration::from_millis(
                config.shm_identity_check_interval_ms,
            )),
    ));
    Ok(ShmHistoryCollector::new(slots, series))
}

async fn load_channel_manifest(pool: &SqlitePool) -> anyhow::Result<ChannelPointManifest> {
    let mut counts = BTreeMap::<u32, [u32; 4]>::new();
    for (table, type_index, physical_only) in [
        ("telemetry_points", 0_usize, true),
        ("signal_points", 1_usize, true),
        ("control_points", 2_usize, false),
        ("adjustment_points", 3_usize, false),
    ] {
        let query = if physical_only {
            format!(
                "SELECT p.channel_id, MAX(p.point_id) + 1 \
                 FROM {table} p JOIN channels c ON c.channel_id = p.channel_id \
                 WHERE c.protocol != 'virtual' GROUP BY p.channel_id"
            )
        } else {
            format!("SELECT channel_id, MAX(point_id) + 1 FROM {table} GROUP BY channel_id")
        };
        let rows: Vec<(i64, i64)> = sqlx::query_as(&query)
            .fetch_all(pool)
            .await
            .with_context(|| format!("load history SHM counts from {table}"))?;
        for (channel_id, count) in rows {
            counts
                .entry(config_u32(channel_id, "channel_id")?)
                .or_insert([0; 4])[type_index] = config_u32(count, "point count")?;
        }
    }
    Ok(ChannelPointManifest::from_map(counts))
}

async fn load_history_series(
    pool: &SqlitePool,
    manifest: &ChannelPointManifest,
) -> anyhow::Result<Vec<HistorySeries>> {
    let mut series = BTreeMap::<(String, String), usize>::new();
    for (table, code, kind, physical_only) in [
        ("telemetry_points", "T", PointKind::Telemetry, true),
        ("signal_points", "S", PointKind::Status, true),
        ("control_points", "C", PointKind::Command, false),
        ("adjustment_points", "A", PointKind::Action, false),
    ] {
        let query = if physical_only {
            format!(
                "SELECT p.channel_id, p.point_id \
                 FROM {table} p JOIN channels c ON c.channel_id = p.channel_id \
                 WHERE c.protocol != 'virtual' ORDER BY p.channel_id, p.point_id"
            )
        } else {
            format!("SELECT channel_id, point_id FROM {table} ORDER BY channel_id, point_id")
        };
        let rows: Vec<(i64, i64)> = sqlx::query_as(&query)
            .fetch_all(pool)
            .await
            .with_context(|| format!("load configured history series from {table}"))?;
        for (channel_id, point_id) in rows {
            let channel_id = config_u32(channel_id, "series channel_id")?;
            let point_id = config_u32(point_id, "series point_id")?;
            if let Some(slot) = manifest.slot(channel_id, kind, point_id) {
                series.insert(
                    (format!("io:{channel_id}:{code}"), point_id.to_string()),
                    slot,
                );
            }
        }
    }

    let measurements: Vec<(i64, i64, String, i64, i64)> = sqlx::query_as(
        "SELECT instance_id, channel_id, channel_type, channel_point_id, measurement_id \
         FROM measurement_routing WHERE enabled = TRUE",
    )
    .fetch_all(pool)
    .await
    .context("load history measurement catalogue")?;
    for (instance_id, channel_id, channel_type, channel_point_id, measurement_id) in measurements {
        add_instance_series(
            &mut series,
            manifest,
            "M",
            instance_id,
            measurement_id,
            channel_id,
            &channel_type,
            channel_point_id,
        )?;
    }

    let actions: Vec<(i64, i64, String, i64, i64)> = sqlx::query_as(
        "SELECT instance_id, channel_id, channel_type, channel_point_id, action_id \
         FROM action_routing WHERE enabled = TRUE",
    )
    .fetch_all(pool)
    .await
    .context("load history action catalogue")?;
    for (instance_id, channel_id, channel_type, channel_point_id, action_id) in actions {
        add_instance_series(
            &mut series,
            manifest,
            "A",
            instance_id,
            action_id,
            channel_id,
            &channel_type,
            channel_point_id,
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

#[allow(clippy::too_many_arguments)]
fn add_instance_series(
    series: &mut BTreeMap<(String, String), usize>,
    manifest: &ChannelPointManifest,
    instance_code: &str,
    instance_id: i64,
    logical_point_id: i64,
    channel_id: i64,
    channel_type: &str,
    channel_point_id: i64,
) -> anyhow::Result<()> {
    let Some(kind) = parse_channel_kind(channel_type) else {
        warn!(channel_type, "skipping invalid history route point type");
        return Ok(());
    };
    let instance_id = config_u32(instance_id, "history instance_id")?;
    let logical_point_id = config_u32(logical_point_id, "history logical point_id")?;
    let channel_id = config_u32(channel_id, "history route channel_id")?;
    let channel_point_id = config_u32(channel_point_id, "history route point_id")?;
    if let Some(slot) = manifest.slot(channel_id, kind, channel_point_id) {
        series.insert(
            (
                format!("inst:{instance_id}:{instance_code}"),
                logical_point_id.to_string(),
            ),
            slot,
        );
    }
    Ok(())
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
            // Series globs use only `*` and `?`; all other characters remain
            // literal so logical IDs cannot inject regex.
            literal => regex.push_str(&regex::escape(&literal.to_string())),
        }
    }
    regex.push('$');
    Regex::new(&regex)
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

fn config_u32(value: i64, label: &str) -> anyhow::Result<u32> {
    u32::try_from(value).with_context(|| format!("{label} must fit in u32, got {value}"))
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use aether_ports::{PortError, PortErrorKind, PortResult};
    use aether_shm_bridge::SlotSnapshot;

    use super::*;

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
            "INSERT INTO channels VALUES (10, 'modbus')",
            "INSERT INTO telemetry_points VALUES (10, 0)",
            "INSERT INTO measurement_routing VALUES (42, 10, 'T', 0, 7, TRUE)",
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

        let points = collector.collect_patterns(&cfg, &cfg.subscribe_patterns);

        assert_eq!(points.len(), 1);
        assert_eq!(points[0].series_key, "inst:1:M");
        assert_eq!(points[0].point_id, "100");
        assert_eq!(points[0].value, Some(42.5));
    }

    #[tokio::test]
    async fn sqlite_catalog_discovers_instance_series_without_scan() {
        let pool = config_pool().await;
        let manifest = load_channel_manifest(&pool).await.expect("load manifest");

        let series = load_history_series(&pool, &manifest)
            .await
            .expect("load logical series");

        assert!(series.iter().any(|series| {
            series.logical_key == "inst:42:M" && series.point_id == "7" && series.slot == 0
        }));
    }

    #[tokio::test]
    async fn production_collector_builds_before_shm_writer() {
        let pool = config_pool().await;
        let unique = format!("history-missing-writer-{}", std::process::id());
        let config = EnvConfig {
            shm_path: std::env::temp_dir()
                .join(unique)
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
                .is_empty()
        );
    }
}
