//! Shared Memory CLI for the authoritative live-state plane.
//!
//! Provides a mysql-cli style interactive interface for reading/writing
//! shared memory data with zero-latency access.

use aether_domain::{PointKind, PointQuality};
use aether_routing::{RoutingCache, load_routing_maps};
use aether_shm_bridge::{
    ChannelPointManifest, DEFAULT_MAX_SLOTS, PhysicalPointAddress, ShmChannelReader,
    ShmObservationStatus, ShmObserver, ShmPlaneObservation, ShmTopologyObservation,
    default_shm_path,
};
use anyhow::{Context, Result, bail};
use clap::Subcommand;
use colored::*;
use common::PointType;
use rustyline::completion::{Completer, Pair};
use rustyline::error::ReadlineError;
use rustyline::highlight::Highlighter;
use rustyline::hint::Hinter;
use rustyline::validate::Validator;
use rustyline::{Editor, Helper};
use std::collections::{BTreeMap, BTreeSet};
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use crate::shm_dashboard::run_dashboard;

/// Clap subcommands (for one-shot mode)
#[derive(Subcommand)]
pub enum ShmCommands {
    /// Get point value
    Get {
        /// Key format: `inst:<id>:M|A:<point_id>` or `ch:<id>:T|S|C|A:<point_id>`
        key: String,
    },

    /// Inspect committed point/health planes and writer liveness
    Info {
        /// Validate headers and commit without scanning every slot
        #[arg(long)]
        no_scan: bool,
    },

    /// Watch key for changes (real-time monitoring)
    Watch {
        /// Key to watch
        key: String,

        /// Polling interval in milliseconds
        #[arg(short, long, default_value = "500")]
        interval_ms: u64,
    },

    /// Real-time TUI dashboard (like htop)
    Top,

    /// Serve the read-only SHM observability dashboard over loopback HTTP
    Serve {
        /// Loopback address for the local dashboard
        #[arg(long, default_value = "127.0.0.1:6070")]
        bind: SocketAddr,

        /// Validate headers and commit without scanning every slot
        #[arg(long)]
        no_scan: bool,
    },
}

/// Parsed shared memory key
#[derive(Debug, Clone)]
pub(crate) enum ShmKey {
    /// Instance point: `inst:<id>:M|A:<point_id>`
    Instance {
        instance_id: u32,
        point_type: u8, // 0=Measurement, 1=Action
        point_id: u32,
    },
    /// Channel point: `ch:<id>:T|S|C|A:<point_id>`
    Channel {
        channel_id: u32,
        point_type: PointType,
        point_id: u32,
    },
}

impl std::fmt::Display for ShmKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ShmKey::Instance {
                instance_id,
                point_type,
                point_id,
            } => {
                let role = if *point_type == 0 { "M" } else { "A" };
                write!(f, "inst:{}:{}:{}", instance_id, role, point_id)
            },
            ShmKey::Channel {
                channel_id,
                point_type,
                point_id,
            } => {
                let ptype = match point_type {
                    PointType::Telemetry => "T",
                    PointType::Signal => "S",
                    PointType::Control => "C",
                    PointType::Adjustment => "A",
                };
                write!(f, "ch:{}:{}:{}", channel_id, ptype, point_id)
            },
        }
    }
}

/// Parse key string into ShmKey
///
/// Formats:
/// - `inst:<id>:M:<point_id>` - Instance measurement
/// - `inst:<id>:A:<point_id>` - Instance action
/// - `ch:<id>:T:<point_id>`   - Channel telemetry
/// - `ch:<id>:S:<point_id>`   - Channel signal
/// - `ch:<id>:C:<point_id>`   - Channel control
/// - `ch:<id>:A:<point_id>`   - Channel adjustment
pub(crate) fn parse_key(key: &str) -> Result<ShmKey> {
    let parts: Vec<&str> = key.split(':').collect();

    match parts.as_slice() {
        ["inst", id, role, point_id] => {
            let instance_id: u32 = id.parse().context("Invalid instance ID")?;
            let point_id: u32 = point_id.parse().context("Invalid point ID")?;
            let point_type = match *role {
                "M" => 0,
                "A" => 1,
                _ => bail!("Invalid role '{}'. Use M (Measurement) or A (Action)", role),
            };
            Ok(ShmKey::Instance {
                instance_id,
                point_type,
                point_id,
            })
        },
        ["ch", id, ptype, point_id] => {
            let channel_id: u32 = id.parse().context("Invalid channel ID")?;
            let point_id: u32 = point_id.parse().context("Invalid point ID")?;
            let point_type = match *ptype {
                "T" => PointType::Telemetry,
                "S" => PointType::Signal,
                "C" => PointType::Control,
                "A" => PointType::Adjustment,
                _ => bail!(
                    "Invalid point type '{}'. Use T/S/C/A (Telemetry/Signal/Control/Adjustment)",
                    ptype
                ),
            };
            Ok(ShmKey::Channel {
                channel_id,
                point_type,
                point_id,
            })
        },
        _ => bail!(
            "Invalid key format '{}'\n\
             Use: inst:<id>:M|A:<point_id> or ch:<id>:T|S|C|A:<point_id>",
            key
        ),
    }
}

// ============================================================================
// Tab Completion Helper
// ============================================================================

/// REPL helper providing Tab completion for commands and keys
struct ShmHelper;

impl Helper for ShmHelper {}

impl Hinter for ShmHelper {
    type Hint = String;

    fn hint(&self, _line: &str, _pos: usize, _ctx: &rustyline::Context<'_>) -> Option<String> {
        None
    }
}

impl Highlighter for ShmHelper {}

impl Validator for ShmHelper {}

impl Completer for ShmHelper {
    type Candidate = Pair;

    fn complete(
        &self,
        line: &str,
        pos: usize,
        _ctx: &rustyline::Context<'_>,
    ) -> rustyline::Result<(usize, Vec<Pair>)> {
        let line = &line[..pos];

        // 1. Command completion (no space yet)
        if !line.contains(' ') {
            return Ok(complete_command(line));
        }

        // 2. Key completion for GET/WATCH commands
        let parts: Vec<&str> = line.split_whitespace().collect();
        if !parts.is_empty() {
            let cmd = parts[0].to_uppercase();
            if matches!(cmd.as_str(), "GET" | "WATCH") {
                // Complete key if we're still typing the second argument
                if parts.len() == 1 || (parts.len() == 2 && !line.ends_with(' ')) {
                    let key_part = parts.get(1).copied().unwrap_or("");
                    let start = line.len() - key_part.len();
                    return Ok(complete_key(key_part, start));
                }
            }
        }

        Ok((pos, vec![]))
    }
}

/// Complete command names
fn complete_command(prefix: &str) -> (usize, Vec<Pair>) {
    let commands = ["GET", "INFO", "WATCH", "HELP", "QUIT", "EXIT"];
    let prefix_upper = prefix.to_uppercase();

    let matches: Vec<Pair> = commands
        .iter()
        .filter(|cmd| cmd.starts_with(&prefix_upper))
        .map(|cmd| Pair {
            display: (*cmd).to_string(),
            replacement: (*cmd).to_string(),
        })
        .collect();

    (0, matches)
}

/// Complete key format: `inst:<id>:M|A:<point_id>` or `ch:<id>:T|S|C|A:<point_id>`
fn complete_key(key_prefix: &str, start_pos: usize) -> (usize, Vec<Pair>) {
    let parts: Vec<&str> = key_prefix.split(':').collect();

    match parts.as_slice() {
        // Empty or just started -> suggest inst: or ch:
        [] | [""] => (
            start_pos,
            vec![
                Pair {
                    display: "inst:".into(),
                    replacement: "inst:".into(),
                },
                Pair {
                    display: "ch:".into(),
                    replacement: "ch:".into(),
                },
            ],
        ),
        // Partial prefix -> complete to inst: or ch:
        [prefix] if "inst".starts_with(*prefix) || "ch".starts_with(*prefix) => {
            let mut matches = vec![];
            if "inst".starts_with(*prefix) {
                matches.push(Pair {
                    display: "inst:".into(),
                    replacement: "inst:".into(),
                });
            }
            if "ch".starts_with(*prefix) {
                matches.push(Pair {
                    display: "ch:".into(),
                    replacement: "ch:".into(),
                });
            }
            (start_pos, matches)
        },
        // inst:<id>: -> complete M or A
        ["inst", _id, ""] | ["inst", _id] if key_prefix.ends_with(':') => (
            start_pos,
            vec![
                Pair {
                    display: "M (Measurement)".into(),
                    replacement: format!("{}M:", key_prefix),
                },
                Pair {
                    display: "A (Action)".into(),
                    replacement: format!("{}A:", key_prefix),
                },
            ],
        ),
        // ch:<id>: -> complete T/S/C/A
        ["ch", _id, ""] | ["ch", _id] if key_prefix.ends_with(':') => (
            start_pos,
            vec![
                Pair {
                    display: "T (Telemetry)".into(),
                    replacement: format!("{}T:", key_prefix),
                },
                Pair {
                    display: "S (Signal)".into(),
                    replacement: format!("{}S:", key_prefix),
                },
                Pair {
                    display: "C (Control)".into(),
                    replacement: format!("{}C:", key_prefix),
                },
                Pair {
                    display: "A (Adjustment)".into(),
                    replacement: format!("{}A:", key_prefix),
                },
            ],
        ),
        _ => (start_pos, vec![]),
    }
}

/// Main entry point - handles both REPL and one-shot modes
pub async fn handle_command(
    cmd: Option<ShmCommands>,
    data_directory: &Path,
    json: bool,
) -> Result<()> {
    match cmd {
        None if json => bail!("--json requires an explicit 'shm' subcommand"),
        None => run_repl(data_directory).await,
        Some(cmd) => handle_single_command(cmd, data_directory, json).await,
    }
}

/// Validated channel reader paired with the routing snapshot used for named
/// channel and instance lookups.
pub(crate) struct ShmRuntimeView {
    reader: ShmChannelReader,
    routing_cache: RoutingCache,
}

impl ShmRuntimeView {
    fn open(
        shm_path: &Path,
        manifest: Arc<ChannelPointManifest>,
        routing_cache: RoutingCache,
    ) -> Result<Self> {
        let reader = ShmChannelReader::open(shm_path, manifest)
            .with_context(|| format!("failed to open typed SHM at {}", shm_path.display()))?;
        Ok(Self {
            reader,
            routing_cache,
        })
    }

    fn resolve_key(&self, key: &ShmKey) -> Option<(u32, PointKind, u32)> {
        match key {
            ShmKey::Channel {
                channel_id,
                point_type,
                point_id,
            } => Some((*channel_id, point_kind(*point_type), *point_id)),
            ShmKey::Instance {
                instance_id,
                point_type: 0,
                point_id,
            } => {
                let (channel_id, kind, channel_point_id) = self
                    .routing_cache
                    .lookup_c2m_reverse(*instance_id, *point_id)?;
                kind.is_acquisition_owned()
                    .then_some((channel_id, kind, channel_point_id))
            },
            ShmKey::Instance {
                instance_id,
                point_type: 1,
                point_id,
            } => {
                let target = self
                    .routing_cache
                    .lookup_m2c_by_parts(*instance_id, PointKind::Command, *point_id)
                    .or_else(|| {
                        self.routing_cache.lookup_m2c_by_parts(
                            *instance_id,
                            PointKind::Action,
                            *point_id,
                        )
                    })?;
                target.point_kind.is_writable().then_some((
                    target.channel_id,
                    target.point_kind,
                    target.point_id,
                ))
            },
            ShmKey::Instance { .. } => None,
        }
    }

    pub(crate) fn named_keys(&self) -> Vec<ShmKey> {
        let mut keys = BTreeMap::<String, ShmKey>::new();
        for (_, target) in self.routing_cache.c2m_iter() {
            let key = ShmKey::Instance {
                instance_id: target.instance_id,
                point_type: 0,
                point_id: target.point_id,
            };
            keys.insert(key.to_string(), key);
        }
        for ((instance_id, _, point_id), _) in self.routing_cache.m2c_iter() {
            let key = ShmKey::Instance {
                instance_id,
                point_type: 1,
                point_id,
            };
            keys.insert(key.to_string(), key);
        }
        for (_, address) in self.reader.manifest().iter_physical_points() {
            let key = ShmKey::Channel {
                channel_id: address.channel_id().get(),
                point_type: model_point_type(address.kind()),
                point_id: address.point_id().get(),
            };
            keys.insert(key.to_string(), key);
        }
        keys.into_values().collect()
    }

    pub(crate) fn instance_ids(&self) -> Vec<u32> {
        let mut instance_ids = BTreeSet::new();
        instance_ids.extend(
            self.routing_cache
                .c2m_iter()
                .into_iter()
                .map(|(_, target)| target.instance_id),
        );
        instance_ids.extend(
            self.routing_cache
                .m2c_iter()
                .into_iter()
                .map(|((instance_id, _, _), _)| instance_id),
        );
        instance_ids.into_iter().collect()
    }

    pub(crate) fn channel_ids(&self) -> Vec<u32> {
        self.reader.channel_ids().collect()
    }

    #[allow(clippy::disallowed_methods)] // JSON is the bounded read-only dashboard DTO.
    pub(crate) fn point_preview(&self, limit: usize) -> Result<Vec<serde_json::Value>> {
        self.reader
            .manifest()
            .iter_physical_points()
            .take(limit)
            .map(|(slot, address)| {
                let sample = self
                    .reader
                    .read_physical(address)
                    .map_err(|error| anyhow::anyhow!(error.to_string()))?;
                let kind = match address.kind() {
                    PointKind::Telemetry => "T",
                    PointKind::Status => "S",
                    PointKind::Command => "C",
                    PointKind::Action => "A",
                };
                let mut value = serde_json::json!({
                    "slot": slot,
                    "channel_id": address.channel_id().get(),
                    "kind": kind,
                    "point_id": address.point_id().get(),
                    "present": sample.is_some(),
                });
                if let Some(sample) = sample {
                    let quality = match sample.quality() {
                        PointQuality::Good => "good",
                        PointQuality::Uncertain => "uncertain",
                        PointQuality::Bad => "bad",
                        PointQuality::Unavailable => "unavailable",
                    };
                    value["value"] = serde_json::json!(sample.value());
                    value["raw"] = serde_json::json!(sample.raw());
                    value["timestamp_ms"] = serde_json::json!(sample.timestamp_ms());
                    value["quality"] = serde_json::json!(quality);
                }
                Ok(value)
            })
            .collect()
    }
}

fn point_kind(point_type: PointType) -> PointKind {
    match point_type {
        PointType::Telemetry => PointKind::Telemetry,
        PointType::Signal => PointKind::Status,
        PointType::Control => PointKind::Command,
        PointType::Adjustment => PointKind::Action,
    }
}

fn model_point_type(kind: PointKind) -> PointType {
    match kind {
        PointKind::Telemetry => PointType::Telemetry,
        PointKind::Status => PointType::Signal,
        PointKind::Command => PointType::Control,
        PointKind::Action => PointType::Adjustment,
    }
}

async fn load_channel_point_manifest(pool: &sqlx::SqlitePool) -> Result<ChannelPointManifest> {
    let mut addresses = Vec::new();
    for (table, kind) in [
        ("telemetry_points", PointKind::Telemetry),
        ("signal_points", PointKind::Status),
        ("control_points", PointKind::Command),
        ("adjustment_points", PointKind::Action),
    ] {
        let query =
            format!("SELECT channel_id, point_id FROM {table} ORDER BY channel_id, point_id");
        let rows = sqlx::query_as::<_, (i64, i64)>(&query)
            .fetch_all(pool)
            .await
            .with_context(|| format!("failed to load configured points from {table}"))?;
        for (channel_id, point_id) in rows {
            let channel_id = u32::try_from(channel_id)
                .with_context(|| format!("invalid channel id {channel_id} in {table}"))?;
            let point_id = u32::try_from(point_id)
                .with_context(|| format!("invalid point id {point_id} in {table}"))?;
            addresses.push(PhysicalPointAddress::from_raw_ids(
                channel_id, kind, point_id,
            ));
        }
    }
    let max_slots = sqlx::query_scalar::<_, String>(
        "SELECT value FROM service_config \
         WHERE service_name = 'global' AND key = 'shared_memory.max_slots'",
    )
    .fetch_optional(pool)
    .await
    .ok()
    .flatten()
    .map(|value| {
        value
            .parse::<u32>()
            .context("invalid shared_memory.max_slots value")
    })
    .transpose()?
    .unwrap_or(DEFAULT_MAX_SLOTS);
    ChannelPointManifest::compile(addresses, max_slots as usize)
        .map_err(|error| anyhow::anyhow!(error.to_string()))
}

pub(crate) async fn open_reader(data_directory: &Path) -> Result<ShmRuntimeView> {
    open_reader_at(data_directory, &default_shm_path()).await
}

async fn open_reader_at(data_directory: &Path, shm_path: &Path) -> Result<ShmRuntimeView> {
    let database_path = data_directory.join("aether.db");
    let database_options = sqlx::sqlite::SqliteConnectOptions::new()
        .filename(&database_path)
        .read_only(true);
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(database_options)
        .await
        .with_context(|| {
            format!(
                "failed to open runtime database {} for named SHM queries",
                database_path.display()
            )
        })?;
    let manifest = Arc::new(load_channel_point_manifest(&pool).await?);
    let maps = load_routing_maps(&pool)
        .await
        .context("failed to load routing metadata for named SHM queries")?;
    pool.close().await;
    ShmRuntimeView::open(
        shm_path,
        manifest,
        RoutingCache::from_maps(maps.c2m, maps.m2c, maps.c2c),
    )
}

pub(crate) fn default_observer(scan_slots: bool) -> Result<ShmObserver> {
    let point_path = default_shm_path();
    let mut observer = ShmObserver::new(point_path).with_slot_scan(scan_slots);
    if let Ok(path) = std::env::var("AETHER_CHANNEL_HEALTH_SHM_PATH") {
        observer = observer.with_health_path(path);
    }
    let stale_after_ms = sample_stale_after_ms();
    let healthy_after_ms = (stale_after_ms / 10).min(3_000);
    observer
        .with_liveness_thresholds(healthy_after_ms, stale_after_ms)
        .map_err(|error| anyhow::anyhow!(error.to_string()))
}

/// Handle single command (one-shot mode)
async fn handle_single_command(cmd: ShmCommands, data_directory: &Path, json: bool) -> Result<()> {
    match cmd {
        ShmCommands::Get { key } => {
            let reader = open_reader(data_directory).await?;
            let parsed = parse_key(&key)?;
            let sample = get_sample(&reader, &parsed)?;
            let now_ms = chrono::Utc::now().timestamp_millis();
            if json {
                crate::output::print_success(sample_json(
                    &key,
                    sample,
                    now_ms,
                    sample_stale_after_ms(),
                ));
            } else {
                println!("{}", render_sample(sample, now_ms, sample_stale_after_ms()));
            }
        },
        ShmCommands::Info { no_scan } => {
            let observation = default_observer(!no_scan)?.inspect();
            print_observation(&observation, json);
        },
        ShmCommands::Watch { key, interval_ms } => {
            let reader = open_reader(data_directory).await?;
            let parsed = parse_key(&key)?;
            watch_key(&reader, &parsed, interval_ms, json)?;
        },
        ShmCommands::Top if json => bail!("--json cannot be combined with 'shm top'"),
        ShmCommands::Top => run_dashboard(data_directory).await?,
        ShmCommands::Serve { .. } if json => {
            bail!("--json cannot be combined with 'shm serve'")
        },
        ShmCommands::Serve { bind, no_scan } => {
            let point_view = match open_reader(data_directory).await {
                Ok(reader) => Some(Arc::new(reader)),
                Err(error) => {
                    tracing::warn!(error = %error, "typed point preview unavailable");
                    None
                },
            };
            crate::shm_web::serve_dashboard(default_observer(!no_scan)?, point_view, bind).await?;
        },
    }

    Ok(())
}

/// Get value from shared memory
pub(crate) fn get_value(reader: &ShmRuntimeView, key: &ShmKey) -> Result<Option<f64>> {
    Ok(get_sample(reader, key)?.map(|(value, _)| value))
}

/// Reads a point's newest value together with the timestamp it was stamped at.
///
/// `get_value` alone cannot tell a live reading from one frozen by a channel
/// that stopped answering, because both are the same number. Callers that show
/// a value to a person need the timestamp too.
pub(crate) fn get_sample(reader: &ShmRuntimeView, key: &ShmKey) -> Result<Option<(f64, u64)>> {
    let Some((channel_id, kind, point_id)) = reader.resolve_key(key) else {
        return Ok(None);
    };
    reader
        .reader
        .read_channel(channel_id, kind, point_id)
        .map(|sample| sample.map(|sample| (sample.value(), sample.timestamp_ms())))
        .with_context(|| format!("failed to read named SHM point {key}"))
}

/// Renders a value with the freshness of the sample it came from.
///
/// A frozen reading is the same number as a live one, so printing the number
/// by itself is what let a dead device look healthy in `shm get` and `watch`.
fn render_sample(sample: Option<(f64, u64)>, now_ms: i64, stale_after_ms: u64) -> String {
    let Some((value, sample_ms)) = sample else {
        return "(nil)".to_string();
    };
    let age_ms = now_ms - i64::try_from(sample_ms).unwrap_or(i64::MAX);
    match PointQuality::for_sample_age(age_ms, stale_after_ms) {
        PointQuality::Good => format!("{value}  [good, {age_ms}ms ago]"),
        _ => format!("{}  [{}, {}ms ago]", value, "stale".red(), age_ms.max(0)),
    }
}

#[allow(clippy::disallowed_methods)] // `json!` only serializes validated scalar observations.
fn sample_json(
    key: &str,
    sample: Option<(f64, u64)>,
    observed_at_ms: i64,
    stale_after_ms: u64,
) -> serde_json::Value {
    let Some((value, sample_ms)) = sample else {
        return serde_json::json!({
            "key": key,
            "observed_at_ms": observed_at_ms,
            "present": false,
        });
    };
    let age_ms = observed_at_ms - i64::try_from(sample_ms).unwrap_or(i64::MAX);
    let quality = match PointQuality::for_sample_age(age_ms, stale_after_ms) {
        PointQuality::Good => "good",
        _ => "stale",
    };
    serde_json::json!({
        "key": key,
        "observed_at_ms": observed_at_ms,
        "present": true,
        "value": value,
        "sample_timestamp_ms": sample_ms,
        "age_ms": age_ms,
        "freshness": quality,
    })
}

#[allow(clippy::disallowed_methods)] // `json!` only serializes read-only observer values.
pub(crate) fn observation_json(observation: &ShmTopologyObservation) -> serde_json::Value {
    let findings = observation
        .findings
        .iter()
        .map(|finding| {
            serde_json::json!({
                "severity": finding.severity.as_str(),
                "code": finding.code,
                "message": finding.message,
            })
        })
        .collect::<Vec<_>>();
    serde_json::json!({
        "status": observation.status.as_str(),
        "observed_at_ms": observation.observed_at_ms,
        "publication_epoch": observation.publication_epoch,
        "point": observation.point.as_ref().map(plane_json),
        "health": observation.health.as_ref().map(plane_json),
        "findings": findings,
    })
}

#[allow(clippy::disallowed_methods)] // `json!` only serializes read-only observer values.
fn plane_json(plane: &ShmPlaneObservation) -> serde_json::Value {
    let slots = plane.slots.map(|slots| {
        serde_json::json!({
            "total": slots.total,
            "present": slots.present,
            "unwritten": slots.unwritten,
            "contended": slots.contended,
            "good": slots.good,
            "uncertain": slots.uncertain,
            "bad": slots.bad,
            "unavailable": slots.unavailable,
            "unknown_quality": slots.unknown_quality,
            "online": slots.online,
            "offline": slots.offline,
            "invalid_values": slots.invalid_values,
        })
    });
    serde_json::json!({
        "path": plane.path.display().to_string(),
        "slot_count": plane.slot_count,
        "file_size": plane.file_size,
        "layout_hash": format!("0x{:016x}", plane.layout_hash),
        "writer_generation": plane.writer_generation,
        "publication_epoch": plane.publication_epoch,
        "writer_heartbeat_ms": plane.writer_heartbeat_ms,
        "heartbeat_age_ms": plane.heartbeat_age_ms,
        "slots": slots,
    })
}

/// Freshness bound shared with the read-side SHM adapters.
///
/// Reuses `SHM_WRITER_STALE_AFTER_MS` rather than adding a second dial, so
/// every surface that grades a sample answers with one operator setting.
fn sample_stale_after_ms() -> u64 {
    std::env::var("SHM_WRITER_STALE_AFTER_MS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(30_000)
}

fn print_observation(observation: &ShmTopologyObservation, json: bool) {
    if json {
        crate::output::print_success(observation_json(observation));
        return;
    }
    let status = match observation.status {
        ShmObservationStatus::Healthy => observation.status.as_str().to_uppercase().green(),
        ShmObservationStatus::Degraded => observation.status.as_str().to_uppercase().yellow(),
        ShmObservationStatus::Unhealthy => observation.status.as_str().to_uppercase().red(),
    };
    println!("{} {status}", "Aether SHM".bright_cyan().bold());
    println!(
        "Publication:   {}",
        observation
            .publication_epoch
            .map_or_else(|| "unverified".to_owned(), |epoch| epoch.to_string())
    );
    if let Some(point) = &observation.point {
        print_plane("Point", point);
    }
    if let Some(health) = &observation.health {
        print_plane("Health", health);
    }
    if !observation.findings.is_empty() {
        println!("Findings:");
        for finding in &observation.findings {
            let marker = match finding.severity {
                aether_shm_bridge::ShmObservationSeverity::Degraded => "!".yellow(),
                aether_shm_bridge::ShmObservationSeverity::Unhealthy => "✗".red(),
            };
            println!("  {marker} {}: {}", finding.code, finding.message);
        }
    }
}

fn print_plane(label: &str, plane: &ShmPlaneObservation) {
    println!("{label} plane:");
    println!("  path:        {}", plane.path.display());
    println!(
        "  slots/size:  {} / {} bytes",
        plane.slot_count, plane.file_size
    );
    println!(
        "  generation:  {}  heartbeat: {}ms  hash: 0x{:016x}",
        plane.writer_generation, plane.heartbeat_age_ms, plane.layout_hash
    );
    if let Some(slots) = plane.slots {
        println!(
            "  contents:    present={} unwritten={} good={} uncertain={} bad={} unavailable={} contended={}",
            slots.present,
            slots.unwritten,
            slots.good,
            slots.uncertain,
            slots.bad,
            slots.unavailable,
            slots.contended,
        );
        if label == "Health" {
            println!(
                "  channels:    online={} offline={}",
                slots.online, slots.offline
            );
        }
    }
}

/// Watch a key for changes with polling
fn watch_key(reader: &ShmRuntimeView, key: &ShmKey, interval_ms: u64, json: bool) -> Result<()> {
    if !json {
        println!(
            "Watching {} ({} to stop)",
            key.to_string().bright_yellow(),
            "Ctrl+C".bright_cyan()
        );
    }

    let interval = Duration::from_millis(interval_ms);

    let stale_after_ms = sample_stale_after_ms();

    loop {
        let sample = get_sample(reader, key)?;
        let now = format_current_time();

        let observed_at_ms = chrono::Utc::now().timestamp_millis();
        if json {
            println!(
                "{}",
                serde_json::to_string(&sample_json(
                    &key.to_string(),
                    sample,
                    observed_at_ms,
                    stale_after_ms,
                ))?
            );
        } else {
            println!(
                "[{}] {}",
                now,
                render_sample(sample, observed_at_ms, stale_after_ms)
            );
        }

        std::thread::sleep(interval);
    }
}

/// Format current time as HH:MM:SS
fn format_current_time() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    // Calculate local time components (simplified - assumes UTC for now)
    let secs_in_day = now % 86400;
    let hours = secs_in_day / 3600;
    let minutes = (secs_in_day % 3600) / 60;
    let seconds = secs_in_day % 60;
    format!("{:02}:{:02}:{:02}", hours, minutes, seconds)
}

/// Format epoch seconds as a human-readable time string
#[cfg(test)]
fn format_epoch_secs(epoch_secs: u64) -> String {
    // Calculate time components
    let secs_in_day = epoch_secs % 86400;
    let hours = secs_in_day / 3600;
    let minutes = (secs_in_day % 3600) / 60;
    let seconds = secs_in_day % 60;
    format!("{:02}:{:02}:{:02} UTC", hours, minutes, seconds)
}

/// Interactive REPL loop
async fn run_repl(data_directory: &Path) -> Result<()> {
    let reader = open_reader(data_directory).await?;
    let observer = default_observer(true)?;

    // Create editor with Tab completion helper
    let config = rustyline::Config::builder()
        .completion_type(rustyline::CompletionType::List)
        .build();
    let mut rl = Editor::with_config(config).context("Failed to initialize readline")?;
    rl.set_helper(Some(ShmHelper));

    println!("{}", "Aether Shared Memory CLI".bright_cyan().bold());
    println!(
        "Type '{}' for commands, {} for completion\n",
        "help".bright_yellow(),
        "Tab".bright_cyan()
    );

    loop {
        match rl.readline("aether-shm> ") {
            Ok(line) => {
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }

                // Add to history (ignore errors)
                let _ = rl.add_history_entry(line);

                // Parse and execute
                match execute_repl_command(&reader, &observer, line) {
                    Ok(true) => continue, // Normal command, continue REPL
                    Ok(false) => break,   // QUIT command
                    Err(e) => eprintln!("{} {}", "Error:".red(), e),
                }
            },
            Err(ReadlineError::Interrupted) => {
                // Ctrl+C - ignore and continue
                println!("^C");
                continue;
            },
            Err(ReadlineError::Eof) => {
                // Ctrl+D - exit
                break;
            },
            Err(e) => {
                eprintln!("{} {}", "Readline error:".red(), e);
                break;
            },
        }
    }

    println!("Bye!");
    Ok(())
}

/// Execute a single REPL command
/// Returns Ok(true) to continue, Ok(false) to quit
fn execute_repl_command(
    reader: &ShmRuntimeView,
    observer: &ShmObserver,
    input: &str,
) -> Result<bool> {
    let parts: Vec<&str> = input.split_whitespace().collect();
    let cmd = parts.first().map(|s| s.to_uppercase());

    match cmd.as_deref() {
        Some("GET") => {
            if parts.len() < 2 {
                println!("Usage: GET <key>");
                println!("  Key format: inst:<id>:M|A:<point_id> or ch:<id>:T|S|C|A:<point_id>");
            } else {
                let key = parse_key(parts[1])?;
                println!(
                    "{}",
                    render_sample(
                        get_sample(reader, &key)?,
                        chrono::Utc::now().timestamp_millis(),
                        sample_stale_after_ms()
                    )
                );
            }
        },
        Some("INFO") => {
            print_observation(&observer.inspect(), false);
        },
        Some("WATCH") => {
            if parts.len() < 2 {
                println!("Usage: WATCH <key> [interval_ms]");
            } else {
                let key = parse_key(parts[1])?;
                let interval = parts.get(2).and_then(|s| s.parse().ok()).unwrap_or(500);
                // Note: WATCH will block until Ctrl+C
                watch_key(reader, &key, interval, false)?;
            }
        },
        Some("HELP") | Some("?") => {
            print_help();
        },
        Some("QUIT") | Some("EXIT") | Some("Q") => {
            return Ok(false);
        },
        Some(unknown) => {
            println!(
                "Unknown command '{}'. Type '{}' for available commands.",
                unknown.red(),
                "help".bright_yellow()
            );
        },
        None => {},
    }

    Ok(true)
}

/// Print help message
fn print_help() {
    println!("{}", "=== Available Commands ===".bright_cyan());
    println!();
    println!("  {}     Read point value", "GET <key>".bright_yellow());
    println!(
        "  {}          Show shared memory statistics",
        "INFO".bright_yellow()
    );
    println!(
        "  {}   Monitor point value in real-time",
        "WATCH <key>".bright_yellow()
    );
    println!(
        "  {}          Show this help message",
        "HELP".bright_yellow()
    );
    println!("  {}          Exit the CLI", "QUIT".bright_yellow());
    println!();
    println!("{}", "=== Key Format ===".bright_cyan());
    println!();
    println!("  Instance points:");
    println!("    inst:<id>:M:<point_id>   Measurement point");
    println!("    inst:<id>:A:<point_id>   Action point");
    println!();
    println!("  Channel points:");
    println!("    ch:<id>:T:<point_id>     Telemetry point");
    println!("    ch:<id>:S:<point_id>     Signal point");
    println!("    ch:<id>:C:<point_id>     Control point");
    println!("    ch:<id>:A:<point_id>     Adjustment point");
    println!();
    println!("{}", "=== Examples ===".bright_cyan());
    println!();
    println!("  GET inst:5:M:1          Get instance 5, measurement point 1");
    println!("  GET ch:1001:T:2         Get channel 1001, telemetry point 2");
    println!("  WATCH inst:5:M:1        Watch instance 5, measurement point 1");
    println!("  WATCH inst:5:M:1 100    Watch with 100ms interval");
}

// ============================================================================
// Unit Tests
// ============================================================================

#[cfg(test)]
#[allow(clippy::disallowed_methods)] // Test code - unwrap is acceptable
mod tests {
    use super::*;
    use std::collections::{BTreeSet, HashMap};
    use std::sync::Arc;

    #[test]
    fn a_fresh_reading_is_printed_with_its_age() {
        let rendered = render_sample(Some((24.6, 95_000)), 100_000, 30_000);

        assert!(rendered.contains("24.6"), "{rendered}");
        assert!(rendered.contains("good"), "{rendered}");
        assert!(rendered.contains("5000ms ago"), "{rendered}");
    }

    #[test]
    fn a_frozen_reading_is_marked_stale_instead_of_looking_identical() {
        // `shm watch` printed the bare number every second, so a dead device
        // and a healthy one produced visually identical output forever.
        let rendered = render_sample(Some((24.6, 95_000)), 300_000, 30_000);

        assert!(rendered.contains("24.6"), "{rendered}");
        assert!(rendered.contains("stale"), "{rendered}");
        assert!(!rendered.contains("good"), "{rendered}");
    }

    #[test]
    fn an_unwritten_point_still_reads_as_nil() {
        assert_eq!(render_sample(None, 100_000, 30_000), "(nil)");
    }

    #[test]
    fn observer_json_is_stable_and_keeps_typed_findings() {
        let observation = ShmTopologyObservation {
            observed_at_ms: 100,
            status: ShmObservationStatus::Degraded,
            point: None,
            health: None,
            publication_epoch: Some(7),
            findings: vec![aether_shm_bridge::ShmObservationFinding {
                severity: aether_shm_bridge::ShmObservationSeverity::Degraded,
                code: "writer_heartbeat_delayed",
                message: "point writer heartbeat is 5000ms old".to_owned(),
            }],
        };

        let value = observation_json(&observation);

        assert_eq!(value["status"], "degraded");
        assert_eq!(value["publication_epoch"], 7);
        assert_eq!(value["findings"][0]["code"], "writer_heartbeat_delayed");
        assert!(value["point"].is_null());
        assert!(value["health"].is_null());
    }

    #[test]
    fn point_json_reports_absence_without_forging_a_value() {
        let value = sample_json("ch:7:T:0", None, 100, 30_000);

        assert_eq!(value["present"], false);
        assert!(value.get("value").is_none());
    }

    use aether_dataplane::SlotWriter;
    use aether_domain::PointKind;
    use aether_shm_bridge::ChannelPointManifest;

    fn typed_runtime_view() -> (tempfile::TempDir, ShmRuntimeView) {
        let directory = tempfile::tempdir().expect("create SHM fixture directory");
        let shm_path = directory.path().join("aether-live-state.shm");
        let manifest = Arc::new(ChannelPointManifest::dense_test_fixture([(
            7,
            [1, 0, 0, 1],
        )]));
        let writer =
            SlotWriter::create(&shm_path, manifest.slot_count(), manifest.layout_hash(), 1)
                .expect("create typed SHM fixture");
        writer.set_direct(
            manifest
                .slot_for(PhysicalPointAddress::from_raw_ids(
                    7,
                    PointKind::Telemetry,
                    0,
                ))
                .expect("telemetry slot"),
            12.5,
            125.0,
            100,
            0,
        );
        writer.set_direct(
            manifest
                .slot_for(PhysicalPointAddress::from_raw_ids(7, PointKind::Action, 0))
                .expect("action slot"),
            7.5,
            75.0,
            101,
            0,
        );

        let routing_cache = RoutingCache::from_maps(
            HashMap::from([("7:T:0".to_owned(), "9:M:4".to_owned())]),
            HashMap::from([("9:A:5".to_owned(), "7:A:0".to_owned())]),
            HashMap::new(),
        );
        let view = ShmRuntimeView::open(&shm_path, manifest, routing_cache)
            .expect("open typed runtime view");
        (directory, view)
    }

    #[test]
    fn typed_view_resolves_channel_and_instance_keys_from_manifest() {
        let (_directory, view) = typed_runtime_view();

        let channel = ShmKey::Channel {
            channel_id: 7,
            point_type: PointType::Telemetry,
            point_id: 0,
        };
        let measurement = ShmKey::Instance {
            instance_id: 9,
            point_type: 0,
            point_id: 4,
        };
        let action = ShmKey::Instance {
            instance_id: 9,
            point_type: 1,
            point_id: 5,
        };

        assert_eq!(
            get_value(&view, &channel).expect("read channel"),
            Some(12.5)
        );
        assert_eq!(
            get_value(&view, &measurement).expect("read measurement"),
            Some(12.5)
        );
        assert_eq!(get_value(&view, &action).expect("read action"), Some(7.5));
        let preview = view.point_preview(1).expect("point preview");
        assert_eq!(preview.len(), 1);
        assert_eq!(preview[0]["channel_id"], 7);
        assert_eq!(preview[0]["kind"], "T");
        assert_eq!(preview[0]["point_id"], 0);
        assert_eq!(preview[0]["value"], 12.5);
        assert_eq!(preview[0]["raw"], 125.0);
        assert_eq!(preview[0]["quality"], "good");
        assert_eq!(
            view.named_keys()
                .into_iter()
                .map(|key| key.to_string())
                .collect::<BTreeSet<_>>(),
            BTreeSet::from([
                "ch:7:A:0".to_owned(),
                "ch:7:T:0".to_owned(),
                "inst:9:A:5".to_owned(),
                "inst:9:M:4".to_owned(),
            ])
        );
    }

    #[test]
    fn typed_view_rejects_manifest_that_does_not_match_shm_header() {
        let directory = tempfile::tempdir().expect("create SHM fixture directory");
        let shm_path = directory.path().join("aether-live-state.shm");
        let actual = ChannelPointManifest::dense_test_fixture([(7, [1, 0, 0, 0])]);
        let _writer = SlotWriter::create(&shm_path, actual.slot_count(), actual.layout_hash(), 1)
            .expect("create typed SHM fixture");
        let mismatched = Arc::new(ChannelPointManifest::dense_test_fixture([(
            7,
            [2, 0, 0, 0],
        )]));

        let result = ShmRuntimeView::open(&shm_path, mismatched, RoutingCache::default());

        assert!(result.is_err());
    }

    #[tokio::test]
    async fn open_reader_at_uses_database_manifest_and_instance_routing() {
        let directory = tempfile::tempdir().expect("create runtime fixture directory");
        let data_directory = directory.path().join("data");
        std::fs::create_dir_all(&data_directory).expect("create data directory");
        let database_path = data_directory.join("aether.db");
        let database_options = sqlx::sqlite::SqliteConnectOptions::new()
            .filename(&database_path)
            .create_if_missing(true);
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(database_options)
            .await
            .expect("create runtime database");
        for table in [
            "telemetry_points",
            "signal_points",
            "control_points",
            "adjustment_points",
        ] {
            sqlx::query(&format!(
                "CREATE TABLE {table} (channel_id INTEGER NOT NULL, point_id INTEGER NOT NULL)"
            ))
            .execute(&pool)
            .await
            .expect("create point table");
        }
        sqlx::query(
            "CREATE TABLE measurement_routing (
                instance_id INTEGER NOT NULL,
                instance_name TEXT NOT NULL,
                channel_id INTEGER NOT NULL,
                channel_type TEXT NOT NULL,
                channel_point_id INTEGER NOT NULL,
                measurement_id INTEGER NOT NULL,
                enabled INTEGER NOT NULL
            )",
        )
        .execute(&pool)
        .await
        .expect("create measurement routing table");
        sqlx::query(
            "CREATE TABLE action_routing (
                instance_id INTEGER NOT NULL,
                instance_name TEXT NOT NULL,
                action_id INTEGER NOT NULL,
                channel_id INTEGER NOT NULL,
                channel_type TEXT NOT NULL,
                channel_point_id INTEGER NOT NULL,
                enabled INTEGER NOT NULL
            )",
        )
        .execute(&pool)
        .await
        .expect("create action routing table");
        sqlx::query("INSERT INTO telemetry_points VALUES (7, 0)")
            .execute(&pool)
            .await
            .expect("insert telemetry point");
        sqlx::query("INSERT INTO measurement_routing VALUES (9, 'meter', 7, 'T', 0, 4, 1)")
            .execute(&pool)
            .await
            .expect("insert measurement route");
        pool.close().await;

        let manifest = ChannelPointManifest::dense_test_fixture([(7, [1, 0, 0, 0])]);
        let shm_path = directory.path().join("aether-live-state.shm");
        let writer =
            SlotWriter::create(&shm_path, manifest.slot_count(), manifest.layout_hash(), 1)
                .expect("create runtime SHM");
        writer.set_direct(
            manifest
                .slot_for(PhysicalPointAddress::from_raw_ids(
                    7,
                    PointKind::Telemetry,
                    0,
                ))
                .expect("telemetry slot"),
            48.0,
            480.0,
            200,
            0,
        );

        let view = open_reader_at(&data_directory, &shm_path)
            .await
            .expect("open runtime reader");
        let measurement = ShmKey::Instance {
            instance_id: 9,
            point_type: 0,
            point_id: 4,
        };

        assert_eq!(
            get_value(&view, &measurement).expect("read routed measurement"),
            Some(48.0)
        );
    }

    // ========================================================================
    // parse_key() Tests
    // ========================================================================

    #[test]
    fn test_parse_key_instance_measurement() {
        let key = parse_key("inst:5:M:10").unwrap();
        match key {
            ShmKey::Instance {
                instance_id,
                point_type,
                point_id,
            } => {
                assert_eq!(instance_id, 5);
                assert_eq!(point_type, 0); // Measurement
                assert_eq!(point_id, 10);
            },
            _ => panic!("Expected Instance key"),
        }
    }

    #[test]
    fn test_parse_key_instance_action() {
        let key = parse_key("inst:100:A:200").unwrap();
        match key {
            ShmKey::Instance {
                instance_id,
                point_type,
                point_id,
            } => {
                assert_eq!(instance_id, 100);
                assert_eq!(point_type, 1); // Action
                assert_eq!(point_id, 200);
            },
            _ => panic!("Expected Instance key"),
        }
    }

    #[test]
    fn test_parse_key_instance_lowercase_is_rejected() {
        assert!(parse_key("inst:1:m:2").is_err());
    }

    #[test]
    fn test_parse_key_channel_telemetry() {
        let key = parse_key("ch:1001:T:5").unwrap();
        match key {
            ShmKey::Channel {
                channel_id,
                point_type,
                point_id,
            } => {
                assert_eq!(channel_id, 1001);
                assert_eq!(point_type, PointType::Telemetry);
                assert_eq!(point_id, 5);
            },
            _ => panic!("Expected Channel key"),
        }
    }

    #[test]
    fn test_parse_key_channel_signal() {
        let key = parse_key("ch:2002:S:10").unwrap();
        match key {
            ShmKey::Channel {
                channel_id,
                point_type,
                point_id,
            } => {
                assert_eq!(channel_id, 2002);
                assert_eq!(point_type, PointType::Signal);
                assert_eq!(point_id, 10);
            },
            _ => panic!("Expected Channel key"),
        }
    }

    #[test]
    fn test_parse_key_channel_control() {
        let key = parse_key("ch:3003:C:15").unwrap();
        match key {
            ShmKey::Channel { point_type, .. } => {
                assert_eq!(point_type, PointType::Control);
            },
            _ => panic!("Expected Channel key"),
        }
    }

    #[test]
    fn test_parse_key_channel_adjustment() {
        let key = parse_key("ch:4004:A:20").unwrap();
        match key {
            ShmKey::Channel { point_type, .. } => {
                assert_eq!(point_type, PointType::Adjustment);
            },
            _ => panic!("Expected Channel key"),
        }
    }

    #[test]
    fn test_parse_key_channel_lowercase_is_rejected() {
        assert!(parse_key("ch:1:t:2").is_err());
    }

    #[test]
    fn test_parse_key_invalid_format() {
        // Missing parts
        assert!(parse_key("inst:5").is_err());
        assert!(parse_key("ch:1001").is_err());
        assert!(parse_key("inst").is_err());

        // Wrong prefix
        assert!(parse_key("invalid:5:M:10").is_err());

        // Invalid IDs
        assert!(parse_key("inst:abc:M:10").is_err());
        assert!(parse_key("ch:1001:T:xyz").is_err());

        // Invalid role/type
        assert!(parse_key("inst:5:X:10").is_err());
        assert!(parse_key("ch:1001:Z:5").is_err());
    }

    #[test]
    fn test_parse_key_empty_string() {
        assert!(parse_key("").is_err());
    }

    // ========================================================================
    // ShmKey Display Tests
    // ========================================================================

    #[test]
    fn test_shm_key_display_instance_measurement() {
        let key = ShmKey::Instance {
            instance_id: 5,
            point_type: 0,
            point_id: 10,
        };
        assert_eq!(format!("{}", key), "inst:5:M:10");
    }

    #[test]
    fn test_shm_key_display_instance_action() {
        let key = ShmKey::Instance {
            instance_id: 100,
            point_type: 1,
            point_id: 200,
        };
        assert_eq!(format!("{}", key), "inst:100:A:200");
    }

    #[test]
    fn test_shm_key_display_channel_telemetry() {
        let key = ShmKey::Channel {
            channel_id: 1001,
            point_type: PointType::Telemetry,
            point_id: 5,
        };
        assert_eq!(format!("{}", key), "ch:1001:T:5");
    }

    #[test]
    fn test_shm_key_display_channel_signal() {
        let key = ShmKey::Channel {
            channel_id: 2002,
            point_type: PointType::Signal,
            point_id: 10,
        };
        assert_eq!(format!("{}", key), "ch:2002:S:10");
    }

    #[test]
    fn test_shm_key_display_channel_control() {
        let key = ShmKey::Channel {
            channel_id: 3003,
            point_type: PointType::Control,
            point_id: 15,
        };
        assert_eq!(format!("{}", key), "ch:3003:C:15");
    }

    #[test]
    fn test_shm_key_display_channel_adjustment() {
        let key = ShmKey::Channel {
            channel_id: 4004,
            point_type: PointType::Adjustment,
            point_id: 20,
        };
        assert_eq!(format!("{}", key), "ch:4004:A:20");
    }

    #[test]
    fn test_shm_key_roundtrip() {
        // Test that Display -> parse_key roundtrips correctly
        let original = ShmKey::Instance {
            instance_id: 42,
            point_type: 0,
            point_id: 123,
        };
        let displayed = format!("{}", original);
        let parsed = parse_key(&displayed).unwrap();

        match parsed {
            ShmKey::Instance {
                instance_id,
                point_type,
                point_id,
            } => {
                assert_eq!(instance_id, 42);
                assert_eq!(point_type, 0);
                assert_eq!(point_id, 123);
            },
            _ => panic!("Roundtrip failed"),
        }
    }

    // ========================================================================
    // complete_command() Tests
    // ========================================================================

    #[test]
    fn test_complete_command_empty() {
        let (start, matches) = complete_command("");
        assert_eq!(start, 0);
        assert_eq!(matches.len(), 6); // GET, INFO, WATCH, HELP, QUIT, EXIT
    }

    #[test]
    fn test_complete_command_partial_g() {
        let (start, matches) = complete_command("G");
        assert_eq!(start, 0);
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].replacement, "GET");
    }

    #[test]
    fn test_complete_command_partial_q() {
        let (start, matches) = complete_command("Q");
        assert_eq!(start, 0);
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].replacement, "QUIT");
    }

    #[test]
    fn test_complete_command_partial_e() {
        let (start, matches) = complete_command("E");
        assert_eq!(start, 0);
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].replacement, "EXIT");
    }

    #[test]
    fn test_complete_command_case_insensitive() {
        let (_, matches_upper) = complete_command("G");
        let (_, matches_lower) = complete_command("g");
        assert_eq!(matches_upper.len(), matches_lower.len());
    }

    #[test]
    fn test_complete_command_no_match() {
        let (start, matches) = complete_command("XYZ");
        assert_eq!(start, 0);
        assert!(matches.is_empty());
    }

    // ========================================================================
    // complete_key() Tests
    // ========================================================================

    #[test]
    fn test_complete_key_empty() {
        let (start, matches) = complete_key("", 0);
        assert_eq!(start, 0);
        assert_eq!(matches.len(), 2); // inst:, ch:
    }

    #[test]
    fn test_complete_key_partial_inst() {
        let (start, matches) = complete_key("in", 5);
        assert_eq!(start, 5);
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].replacement, "inst:");
    }

    #[test]
    fn test_complete_key_partial_ch() {
        let (start, matches) = complete_key("c", 5);
        assert_eq!(start, 5);
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].replacement, "ch:");
    }

    #[test]
    fn test_complete_key_instance_role() {
        let (start, matches) = complete_key("inst:5:", 5);
        assert_eq!(start, 5);
        assert_eq!(matches.len(), 2); // M, A
        assert!(matches.iter().any(|m| m.replacement.contains("M:")));
        assert!(matches.iter().any(|m| m.replacement.contains("A:")));
    }

    #[test]
    fn test_complete_key_channel_type() {
        let (start, matches) = complete_key("ch:1001:", 5);
        assert_eq!(start, 5);
        assert_eq!(matches.len(), 4); // T, S, C, A
        assert!(matches.iter().any(|m| m.replacement.contains("T:")));
        assert!(matches.iter().any(|m| m.replacement.contains("S:")));
        assert!(matches.iter().any(|m| m.replacement.contains("C:")));
        assert!(matches.iter().any(|m| m.replacement.contains("A:")));
    }

    // ========================================================================
    // format_epoch_secs() Tests
    // ========================================================================

    #[test]
    fn test_format_epoch_secs_midnight() {
        // Midnight UTC
        assert_eq!(format_epoch_secs(0), "00:00:00 UTC");
    }

    #[test]
    fn test_format_epoch_secs_noon() {
        // 12:00:00 UTC (43200 seconds into the day)
        assert_eq!(format_epoch_secs(43200), "12:00:00 UTC");
    }

    #[test]
    fn test_format_epoch_secs_end_of_day() {
        // 23:59:59 UTC (86399 seconds into the day)
        assert_eq!(format_epoch_secs(86399), "23:59:59 UTC");
    }

    #[test]
    fn test_format_epoch_secs_wraps_days() {
        // 86400 seconds = 1 day, should wrap to 00:00:00
        assert_eq!(format_epoch_secs(86400), "00:00:00 UTC");
    }

    #[test]
    fn test_format_epoch_secs_multi_day() {
        // 90061 seconds = 1 day + 1 hour + 1 minute + 1 second
        // Should be 01:01:01 UTC (wrapping days)
        assert_eq!(format_epoch_secs(90061), "01:01:01 UTC");
    }

    #[test]
    fn test_format_epoch_secs_padding() {
        // 3661 seconds = 01:01:01, check zero padding
        assert_eq!(format_epoch_secs(3661), "01:01:01 UTC");
    }
}
