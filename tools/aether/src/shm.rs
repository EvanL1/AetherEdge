//! Shared Memory CLI - Interactive REPL for aether-rtdb
//!
//! Provides a mysql-cli style interactive interface for reading/writing
//! shared memory data with zero-latency access.

use aether_dataplane::SlotReader;
use aether_domain::PointKind;
use aether_routing::{RoutingCache, load_routing_maps};
use aether_shm_bridge::{ChannelPointManifest, ShmChannelReader, default_shm_path};
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

    /// Show shared memory statistics
    Info,

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
            let point_type = match role.to_uppercase().as_str() {
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
            let point_type = match ptype.to_uppercase().as_str() {
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
pub async fn handle_command(cmd: Option<ShmCommands>, data_directory: &Path) -> Result<()> {
    match cmd {
        None => run_repl(data_directory).await,
        Some(cmd) => handle_single_command(cmd, data_directory).await,
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
                let (channel_id, point_type, channel_point_id) = self
                    .routing_cache
                    .lookup_c2m_reverse(*instance_id, *point_id)?;
                let kind = match point_type {
                    PointType::Telemetry => PointKind::Telemetry,
                    PointType::Signal => PointKind::Status,
                    PointType::Control | PointType::Adjustment => return None,
                };
                Some((channel_id, kind, channel_point_id))
            },
            ShmKey::Instance {
                instance_id,
                point_type: 1,
                point_id,
            } => {
                let target = self
                    .routing_cache
                    .lookup_m2c_by_parts(*instance_id, PointType::Control, *point_id)
                    .or_else(|| {
                        self.routing_cache.lookup_m2c_by_parts(
                            *instance_id,
                            PointType::Adjustment,
                            *point_id,
                        )
                    })?;
                let kind = match target.point_type {
                    PointType::Control => PointKind::Command,
                    PointType::Adjustment => PointKind::Action,
                    PointType::Telemetry | PointType::Signal => return None,
                };
                Some((target.channel_id, kind, target.point_id))
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
        if let Some(manifest) = self.reader.manifest() {
            for (_, address) in manifest.iter_physical_points() {
                let key = ShmKey::Channel {
                    channel_id: address.channel_id().get(),
                    point_type: model_point_type(address.kind()),
                    point_id: address.point_id().get(),
                };
                keys.insert(key.to_string(), key);
            }
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

    pub(crate) fn slot_count(&self) -> usize {
        self.reader.slot_count()
    }

    pub(crate) fn max_slots(&self) -> u32 {
        self.reader.max_slots()
    }

    pub(crate) fn writer_heartbeat(&self) -> u64 {
        self.reader.writer_heartbeat()
    }

    pub(crate) fn generation(&self) -> u64 {
        self.reader.generation()
    }

    pub(crate) fn is_writer_alive(&self, timeout: Duration) -> bool {
        self.reader.is_writer_alive(timeout)
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
    let mut counts = BTreeMap::<u32, [u32; 4]>::new();
    for (table, kind_index) in [
        ("telemetry_points", 0_usize),
        ("signal_points", 1),
        ("control_points", 2),
        ("adjustment_points", 3),
    ] {
        let query =
            format!("SELECT channel_id, MAX(point_id) + 1 FROM {table} GROUP BY channel_id");
        let rows = sqlx::query_as::<_, (i64, i64)>(&query)
            .fetch_all(pool)
            .await
            .with_context(|| format!("failed to load channel layout from {table}"))?;
        for (channel_id, point_count) in rows {
            let channel_id = u32::try_from(channel_id)
                .with_context(|| format!("invalid channel id {channel_id} in {table}"))?;
            let point_count = u32::try_from(point_count)
                .with_context(|| format!("invalid point count {point_count} in {table}"))?;
            counts.entry(channel_id).or_insert([0; 4])[kind_index] = point_count;
        }
    }
    Ok(ChannelPointManifest::from_map(counts))
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

fn open_raw_reader() -> Result<SlotReader> {
    let path = default_shm_path();
    SlotReader::open(&path)
        .with_context(|| format!("failed to open shared memory at {}", path.display()))
}

/// Handle single command (one-shot mode)
async fn handle_single_command(cmd: ShmCommands, data_directory: &Path) -> Result<()> {
    match cmd {
        ShmCommands::Get { key } => {
            let reader = open_reader(data_directory).await?;
            let parsed = parse_key(&key)?;
            let value = get_value(&reader, &parsed)?;
            match value {
                Some(v) => println!("{}", v),
                None => println!("(nil)"),
            }
        },
        ShmCommands::Info => {
            print_raw_info(&open_raw_reader()?);
        },
        ShmCommands::Watch { key, interval_ms } => {
            let reader = open_reader(data_directory).await?;
            let parsed = parse_key(&key)?;
            watch_key(&reader, &parsed, interval_ms)?;
        },
        ShmCommands::Top => run_dashboard(data_directory).await?,
    }

    Ok(())
}

/// Get value from shared memory
pub(crate) fn get_value(reader: &ShmRuntimeView, key: &ShmKey) -> Result<Option<f64>> {
    let Some((channel_id, kind, point_id)) = reader.resolve_key(key) else {
        return Ok(None);
    };
    reader
        .reader
        .read_channel(channel_id, kind, point_id)
        .map(|sample| sample.map(|sample| sample.value()))
        .with_context(|| format!("failed to read named SHM point {key}"))
}

fn print_raw_info(reader: &SlotReader) {
    let path = default_shm_path();

    println!("{}", "=== Shared Memory Stats ===".bright_cyan());
    println!("Path:          {}", path.display());
    println!("Total Slots:   {}", reader.slot_count());
    println!("Max Slots:     {}", reader.max_slots());
    let header = reader.header();
    println!("Generation:    {}", header.writer_generation);
    println!("Layout Hash:   0x{:016x}", header.routing_hash);
    let heartbeat = reader.writer_heartbeat();
    let heartbeat_age = aether_dataplane::core::config::timestamp_ms().saturating_sub(heartbeat);
    let alive = reader.is_writer_alive(5000);
    let status = if alive {
        format!("{} ({}ms ago)", "alive".green(), heartbeat_age)
    } else {
        format!("{} ({}ms ago)", "dead/stale".red(), heartbeat_age)
    };
    println!("Writer:        {}", status);
}

fn print_runtime_info(reader: &ShmRuntimeView) {
    let path = default_shm_path();
    println!("{}", "=== Shared Memory Stats ===".bright_cyan());
    println!("Path:          {}", path.display());
    println!(
        "Instances:     {} (via routing)",
        reader.instance_ids().len()
    );
    println!("Channels:      {}", reader.channel_ids().len());
    println!("Total Slots:   {}", reader.slot_count());
    println!("Max Slots:     {}", reader.max_slots());
    println!("Generation:    {}", reader.generation());
    let heartbeat = reader.writer_heartbeat();
    let heartbeat_age = aether_dataplane::core::config::timestamp_ms().saturating_sub(heartbeat);
    let status = if reader.is_writer_alive(Duration::from_secs(5)) {
        format!("{} ({}ms ago)", "alive".green(), heartbeat_age)
    } else {
        format!("{} ({}ms ago)", "dead/stale".red(), heartbeat_age)
    };
    println!("Writer:        {}", status);
}

/// Watch a key for changes with polling
fn watch_key(reader: &ShmRuntimeView, key: &ShmKey, interval_ms: u64) -> Result<()> {
    println!(
        "Watching {} ({} to stop)",
        key.to_string().bright_yellow(),
        "Ctrl+C".bright_cyan()
    );

    let interval = Duration::from_millis(interval_ms);

    loop {
        let value = get_value(reader, key)?;
        let now = format_current_time();

        match value {
            Some(v) => println!("[{}] {}", now, v),
            None => println!("[{}] (nil)", now),
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
                match execute_repl_command(&reader, line) {
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
fn execute_repl_command(reader: &ShmRuntimeView, input: &str) -> Result<bool> {
    let parts: Vec<&str> = input.split_whitespace().collect();
    let cmd = parts.first().map(|s| s.to_uppercase());

    match cmd.as_deref() {
        Some("GET") => {
            if parts.len() < 2 {
                println!("Usage: GET <key>");
                println!("  Key format: inst:<id>:M|A:<point_id> or ch:<id>:T|S|C|A:<point_id>");
            } else {
                let key = parse_key(parts[1])?;
                match get_value(reader, &key)? {
                    Some(v) => println!("{}", v),
                    None => println!("(nil)"),
                }
            }
        },
        Some("INFO") => {
            print_runtime_info(reader);
        },
        Some("WATCH") => {
            if parts.len() < 2 {
                println!("Usage: WATCH <key> [interval_ms]");
            } else {
                let key = parse_key(parts[1])?;
                let interval = parts.get(2).and_then(|s| s.parse().ok()).unwrap_or(500);
                // Note: WATCH will block until Ctrl+C
                watch_key(reader, &key, interval)?;
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

    use aether_dataplane::SlotWriter;
    use aether_domain::PointKind;
    use aether_shm_bridge::ChannelPointManifest;

    fn typed_runtime_view() -> (tempfile::TempDir, ShmRuntimeView) {
        let directory = tempfile::tempdir().expect("create SHM fixture directory");
        let shm_path = directory.path().join("aether-rtdb.shm");
        let manifest = Arc::new(ChannelPointManifest::from_entries([(7, [1, 0, 0, 1])]));
        let writer = SlotWriter::create(
            &shm_path,
            manifest.slot_count() as u32,
            manifest.slot_count(),
            manifest.layout_hash(),
        )
        .expect("create typed SHM fixture");
        writer.set_direct(
            manifest
                .slot(7, PointKind::Telemetry, 0)
                .expect("telemetry slot"),
            12.5,
            125.0,
            100,
        );
        writer.set_direct(
            manifest.slot(7, PointKind::Action, 0).expect("action slot"),
            7.5,
            75.0,
            101,
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
        let shm_path = directory.path().join("aether-rtdb.shm");
        let actual = ChannelPointManifest::from_entries([(7, [1, 0, 0, 0])]);
        let _writer = SlotWriter::create(
            &shm_path,
            actual.slot_count() as u32,
            actual.slot_count(),
            actual.layout_hash(),
        )
        .expect("create typed SHM fixture");
        let mismatched = Arc::new(ChannelPointManifest::from_entries([(7, [2, 0, 0, 0])]));

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

        let manifest = ChannelPointManifest::from_entries([(7, [1, 0, 0, 0])]);
        let shm_path = directory.path().join("aether-rtdb.shm");
        let writer = SlotWriter::create(
            &shm_path,
            manifest.slot_count() as u32,
            manifest.slot_count(),
            manifest.layout_hash(),
        )
        .expect("create runtime SHM");
        writer.set_direct(
            manifest
                .slot(7, PointKind::Telemetry, 0)
                .expect("telemetry slot"),
            48.0,
            480.0,
            200,
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
    fn test_parse_key_instance_lowercase() {
        // Test case insensitivity
        let key = parse_key("inst:1:m:2").unwrap();
        match key {
            ShmKey::Instance { point_type, .. } => {
                assert_eq!(point_type, 0); // Measurement
            },
            _ => panic!("Expected Instance key"),
        }
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
    fn test_parse_key_channel_lowercase() {
        let key = parse_key("ch:1:t:2").unwrap();
        match key {
            ShmKey::Channel { point_type, .. } => {
                assert_eq!(point_type, PointType::Telemetry);
            },
            _ => panic!("Expected Channel key"),
        }
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
