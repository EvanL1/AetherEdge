//! Cloud-facing logical groups backed by SQLite configuration and SHM values.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::time::Duration;

use aether_domain::PointKind;
use aether_ports::{PortError, PortErrorKind, PortResult};
use aether_shm_bridge::{
    ChannelPointManifest, ReconnectingSlotSource, ShmClientConfig, SlotSource,
};
use anyhow::Context;
use regex::Regex;
use sqlx::SqlitePool;
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

impl ShmNetValueSource {
    #[must_use]
    pub fn new<S>(slots: Arc<S>, groups: Vec<LogicalGroup>) -> Self
    where
        S: SlotSource,
    {
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
}

/// Rebuilds logical discovery from SQLite and creates a lazy SHM reader.
pub async fn build_net_value_source(
    pool: &SqlitePool,
    config: &EnvConfig,
) -> anyhow::Result<ShmNetValueSource> {
    let manifest = load_channel_manifest(pool).await?;
    let groups = load_logical_groups(pool, &manifest).await?;
    let slots = Arc::new(ReconnectingSlotSource::new(
        ShmClientConfig::new(&config.shm_path, manifest.layout_hash())
            .with_writer_stale_after(Duration::from_millis(config.shm_writer_stale_after_ms))
            .with_identity_check_interval(Duration::from_millis(
                config.shm_identity_check_interval_ms,
            )),
    ));
    Ok(ShmNetValueSource::new(slots, groups))
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
            .with_context(|| format!("load uplink SHM counts from {table}"))?;
        for (channel_id, count) in rows {
            counts
                .entry(config_u32(channel_id, "channel_id")?)
                .or_insert([0; 4])[type_index] = config_u32(count, "point count")?;
        }
    }
    Ok(ChannelPointManifest::from_map(counts))
}

async fn load_logical_groups(
    pool: &SqlitePool,
    manifest: &ChannelPointManifest,
) -> anyhow::Result<Vec<LogicalGroup>> {
    let mut groups = BTreeMap::<String, (String, String, String, BTreeMap<String, usize>)>::new();
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
            .with_context(|| format!("load uplink groups from {table}"))?;
        for (channel_id, point_id) in rows {
            let channel_id = config_u32(channel_id, "group channel_id")?;
            let point_id = config_u32(point_id, "group point_id")?;
            if let Some(slot) = manifest.slot(channel_id, kind, point_id) {
                add_group_point(
                    &mut groups,
                    "io",
                    &channel_id.to_string(),
                    code,
                    point_id.to_string(),
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
    .context("load uplink measurement groups")?;
    for (instance_id, channel_id, channel_type, channel_point_id, measurement_id) in measurements {
        add_routed_group_point(
            &mut groups,
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
    .context("load uplink action groups")?;
    for (instance_id, channel_id, channel_type, channel_point_id, action_id) in actions {
        add_routed_group_point(
            &mut groups,
            manifest,
            "A",
            instance_id,
            action_id,
            channel_id,
            &channel_type,
            channel_point_id,
        )?;
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

#[allow(clippy::too_many_arguments)]
fn add_routed_group_point(
    groups: &mut GroupMap,
    manifest: &ChannelPointManifest,
    instance_data_type: &str,
    instance_id: i64,
    logical_point_id: i64,
    channel_id: i64,
    channel_type: &str,
    channel_point_id: i64,
) -> anyhow::Result<()> {
    let Some(kind) = parse_channel_kind(channel_type) else {
        warn!(channel_type, "skipping invalid uplink route point type");
        return Ok(());
    };
    let instance_id = config_u32(instance_id, "uplink instance_id")?;
    let logical_point_id = config_u32(logical_point_id, "uplink logical point_id")?;
    let channel_id = config_u32(channel_id, "uplink route channel_id")?;
    let channel_point_id = config_u32(channel_point_id, "uplink route point_id")?;
    if let Some(slot) = manifest.slot(channel_id, kind, channel_point_id) {
        add_group_point(
            groups,
            "inst",
            &instance_id.to_string(),
            instance_data_type,
            logical_point_id.to_string(),
            slot,
        );
    }
    Ok(())
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
            "CREATE TABLE channels (channel_id INTEGER PRIMARY KEY, protocol TEXT NOT NULL)",
            "CREATE TABLE telemetry_points (channel_id INTEGER, point_id INTEGER)",
            "CREATE TABLE signal_points (channel_id INTEGER, point_id INTEGER)",
            "CREATE TABLE control_points (channel_id INTEGER, point_id INTEGER)",
            "CREATE TABLE adjustment_points (channel_id INTEGER, point_id INTEGER)",
            "CREATE TABLE measurement_routing (instance_id INTEGER, channel_id INTEGER, channel_type TEXT, channel_point_id INTEGER, measurement_id INTEGER, enabled BOOLEAN)",
            "CREATE TABLE action_routing (instance_id INTEGER, channel_id INTEGER, channel_type TEXT, channel_point_id INTEGER, action_id INTEGER, enabled BOOLEAN)",
            "INSERT INTO channels VALUES (10, 'modbus')",
            "INSERT INTO telemetry_points VALUES (10, 0)",
            "INSERT INTO measurement_routing VALUES (12, 10, 'T', 0, 5, TRUE)",
        ] {
            sqlx::query(statement)
                .execute(&pool)
                .await
                .expect("create minimal uplink catalogue");
        }
        pool
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
    fn logical_glob_supports_wildcards_and_escapes_literals() {
        let regex = logical_glob_regex("inst:*:M?").expect("compile logical glob");

        assert!(regex.is_match("inst:12:M1"));
        assert!(!regex.is_match("inst:12:M"));
        assert!(!regex.is_match("inst.12:M1"));
    }

    #[tokio::test]
    async fn sqlite_catalog_discovers_cloud_groups_without_scan() {
        let pool = config_pool().await;
        let manifest = load_channel_manifest(&pool).await.expect("load manifest");

        let groups = load_logical_groups(&pool, &manifest)
            .await
            .expect("load groups");

        assert!(groups.iter().any(|group| {
            group.key == "inst:12:M"
                && group.points.get("5") == manifest.slot(10, PointKind::Telemetry, 0).as_ref()
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

        let source = build_net_value_source(&pool, &config)
            .await
            .expect("build with embedded config only");
        let error = source
            .read_group("inst:12:M", None)
            .expect_err("missing writer is a retryable read-time condition");

        assert!(error.is_retryable());
    }
}
