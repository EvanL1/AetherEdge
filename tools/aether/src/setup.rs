//! Conservative first-run planning and application.
//!
//! `aether setup` is deliberately a read-only planner. Persistent changes are
//! available only through `aether setup apply --plan-id ...`, and only for a
//! fresh site or a site containing an exact subset of the distribution's safe
//! empty configuration. Existing commissioned sites are never rewritten.

use crate::core::{AetherCore, schema};
use anyhow::{Context, Result, bail};
use clap::Subcommand;
use serde::Serialize;
use sha2::{Digest, Sha256};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use std::collections::BTreeSet;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

const SETUP_PLAN_SCHEMA_VERSION: u32 = 2;

#[derive(Clone, Copy)]
struct SafeConfigFile {
    relative_path: &'static str,
    contents: &'static str,
}

const SAFE_CONFIG_FILES: [SafeConfigFile; 4] = [
    SafeConfigFile {
        relative_path: "global.yaml",
        contents: include_str!("../../../config.template/global.yaml"),
    },
    SafeConfigFile {
        relative_path: "io/io.yaml",
        contents: include_str!("../../../config.template/io/io.yaml"),
    },
    SafeConfigFile {
        relative_path: "automation/automation.yaml",
        contents: include_str!("../../../config.template/automation/automation.yaml"),
    },
    SafeConfigFile {
        relative_path: "automation/instances.yaml",
        contents: include_str!("../../../config.template/automation/instances.yaml"),
    },
];

// Composition metadata is verified by runtime consumers and is never authored
// or rewritten by site setup. An explicitly supplied manifest may coexist with
// the four safe site files without making the site look commissioned.
const NON_RUNTIME_DISTRIBUTION_FILES: [&str; 2] = [
    "io/README.md",
    aether_runtime_catalog::RUNTIME_MANIFEST_FILE_NAME,
];
const CORE_SCHEMA_TABLES: [&str; 14] = [
    "action_routing",
    "adjustment_points",
    "channel_templates",
    "channels",
    "control_points",
    "instance_properties",
    "instances",
    "measurement_routing",
    "rule_history",
    "rules",
    "service_config",
    "signal_points",
    "sync_metadata",
    "telemetry_points",
];

#[derive(Debug, Subcommand)]
pub(crate) enum SetupCommand {
    /// Recompute and print the read-only setup plan
    Plan,
    /// Apply an unchanged safe plan after explicit confirmation
    Apply {
        /// SHA-256 plan identifier printed by `aether setup`
        #[arg(long, value_name = "PLAN_ID")]
        plan_id: String,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum SiteState {
    Fresh,
    SafePartial,
    SafeReady,
    Existing,
    Blocked,
}

#[derive(Clone, Debug, Serialize)]
struct SetupAction {
    id: &'static str,
    description: String,
    persistent_write: bool,
    physical_side_effects: bool,
}

#[derive(Debug, Serialize)]
struct SetupPlan {
    plan_id: String,
    plan_schema_version: u32,
    aether_version: &'static str,
    core_schema_version: i64,
    site_state: SiteState,
    read_only: bool,
    physical_side_effects: bool,
    config_path: String,
    data_path: String,
    database_path: String,
    actions: Vec<SetupAction>,
    blockers: Vec<String>,
    requires_confirmation: bool,
    apply_argv: Option<Vec<String>>,
    next_step: String,
}

struct SetupAnalysis {
    plan: SetupPlan,
    missing_files: Vec<SafeConfigFile>,
    database_guard: String,
}

struct SetupLock {
    path: PathBuf,
    created_data_directory: bool,
}

impl SetupLock {
    fn acquire(data_path: &Path) -> Result<Self> {
        validate_no_symlink_components(data_path, "setup data directory")?;
        let created_data_directory = !data_path.exists();
        fs::create_dir_all(data_path).with_context(|| {
            format!(
                "failed to create setup lock directory {}",
                data_path.display()
            )
        })?;
        let path = data_path.join(".aether-setup.lock");
        validate_no_symlink_components(&path, "setup lock file")?;
        let mut file = match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                bail!(
                    "another setup apply holds {}; wait for it to finish",
                    path.display()
                )
            },
            Err(error) => {
                if created_data_directory {
                    let _ = fs::remove_dir(data_path);
                }
                return Err(error)
                    .with_context(|| format!("failed to acquire setup lock {}", path.display()));
            },
        };
        if let Err(error) =
            writeln!(file, "pid={}", std::process::id()).and_then(|()| file.sync_all())
        {
            let _ = fs::remove_file(&path);
            if created_data_directory {
                let _ = fs::remove_dir(data_path);
            }
            return Err(error)
                .with_context(|| format!("failed to persist setup lock {}", path.display()));
        }
        Ok(Self {
            path,
            created_data_directory,
        })
    }
}

impl Drop for SetupLock {
    fn drop(&mut self) {
        if let Err(error) = fs::remove_file(&self.path) {
            tracing::warn!(path = %self.path.display(), %error, "failed to remove setup lock");
        }
        if self.created_data_directory
            && let Some(directory) = self.path.parent()
        {
            let _ = fs::remove_dir(directory);
        }
    }
}

#[derive(Clone, Debug, Serialize)]
struct FileFingerprint {
    relative_path: String,
    status: &'static str,
    expected_sha256: String,
    sha256: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize)]
struct DatabaseInspection {
    exists: bool,
    readable: bool,
    user_version: Option<i64>,
    channels: i64,
    instances: i64,
    rules: i64,
    sync_metadata_rows: i64,
    synced_services: Vec<String>,
    tables: Vec<String>,
    sha256: Option<String>,
    wal_sha256: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct DatabaseFingerprint {
    sha256: String,
    wal_sha256: Option<String>,
}

struct DatabaseSnapshot {
    _directory: tempfile::TempDir,
    path: PathBuf,
    fingerprint: DatabaseFingerprint,
}

impl DatabaseInspection {
    fn has_commissioned_rows(&self) -> bool {
        self.channels > 0 || self.instances > 0 || self.rules > 0
    }

    fn has_complete_core_schema(&self) -> bool {
        CORE_SCHEMA_TABLES
            .iter()
            .all(|expected| self.tables.iter().any(|table| table == expected))
    }

    fn has_completed_sync(&self) -> bool {
        ["automation", "global", "io"].iter().all(|expected| {
            self.synced_services
                .iter()
                .any(|service| service == expected)
        })
    }
}

#[derive(Serialize)]
struct PlanHashInput<'a> {
    plan_schema_version: u32,
    aether_version: &'static str,
    core_schema_version: i64,
    config_path: String,
    data_path: String,
    site_state: SiteState,
    files: &'a [FileFingerprint],
    extra_files: &'a [String],
    database: &'a DatabaseInspection,
    actions: &'a [SetupAction],
    blockers: &'a [String],
}

#[derive(Debug, Serialize)]
struct ApplyResult {
    plan_id: String,
    applied: bool,
    configured: bool,
    ready: bool,
    physical_side_effects: bool,
    created_files: Vec<String>,
    next_steps: [&'static str; 2],
}

pub(crate) async fn handle(
    command: Option<SetupCommand>,
    config_path: &Path,
    data_path: &Path,
    json: bool,
) -> Result<()> {
    match command.unwrap_or(SetupCommand::Plan) {
        SetupCommand::Plan => {
            let analysis = analyze(config_path, data_path).await?;
            print_plan(&analysis.plan, json);
        },
        SetupCommand::Apply { plan_id } => {
            let result = apply(config_path, data_path, &plan_id).await?;
            print_apply_result(&result, json);
        },
    }
    Ok(())
}

fn record_path_validation(path: &Path, label: &str, blockers: &mut Vec<String>) -> bool {
    match validate_no_symlink_components(path, label) {
        Ok(()) => true,
        Err(error) => {
            blockers.push(error.to_string());
            false
        },
    }
}

/// Rejects user-controlled path indirection without rejecting a normal path
/// whose final components have not been created yet.
///
/// Only macOS's fixed root aliases (`/etc`, `/tmp`, `/var`) are trusted as
/// part of the host filesystem layout. Every other component must be a real
/// directory/file rather than a symlink.
fn validate_no_symlink_components(path: &Path, label: &str) -> Result<()> {
    use std::path::Component;

    if !path.is_absolute() {
        bail!("{label} must be absolute: {}", path.display());
    }

    let mut cursor = PathBuf::new();
    let mut missing_prefix = false;
    for component in path.components() {
        match component {
            Component::Prefix(_) => {
                cursor.push(component.as_os_str());
                continue;
            },
            Component::RootDir | Component::Normal(_) => {
                cursor.push(component.as_os_str());
            },
            Component::CurDir => continue,
            Component::ParentDir => {
                bail!(
                    "{label} contains a parent-directory component: {}",
                    path.display()
                );
            },
        }

        if missing_prefix {
            continue;
        }
        match fs::symlink_metadata(&cursor) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                if !is_trusted_root_filesystem_alias(&cursor, &metadata) {
                    bail!("{label} contains symlink component: {}", cursor.display());
                }
            },
            Ok(_) => {},
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                missing_prefix = true;
            },
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("cannot inspect {label} component {}", cursor.display())
                });
            },
        }
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn is_trusted_root_filesystem_alias(path: &Path, metadata: &fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt as _;

    matches!(path.to_str(), Some("/etc" | "/tmp" | "/var")) && metadata.uid() == 0
}

#[cfg(not(target_os = "macos"))]
fn is_trusted_root_filesystem_alias(_path: &Path, _metadata: &fs::Metadata) -> bool {
    false
}

async fn analyze(config_path: &Path, data_path: &Path) -> Result<SetupAnalysis> {
    let mut blockers = Vec::new();
    let mut missing_files = Vec::new();
    let mut fingerprints = Vec::with_capacity(SAFE_CONFIG_FILES.len());
    let mut custom_file_count = 0;
    let config_path_is_safe =
        record_path_validation(config_path, "setup configuration directory", &mut blockers);
    let data_path_is_safe =
        record_path_validation(data_path, "setup data directory", &mut blockers);

    for safe_file in SAFE_CONFIG_FILES {
        let path = config_path.join(safe_file.relative_path);
        if !config_path_is_safe {
            fingerprints.push(FileFingerprint {
                relative_path: safe_file.relative_path.to_owned(),
                status: "unsafe",
                expected_sha256: digest_bytes(safe_file.contents.as_bytes()),
                sha256: None,
            });
            continue;
        }
        if !record_path_validation(&path, "safe configuration file", &mut blockers) {
            fingerprints.push(FileFingerprint {
                relative_path: safe_file.relative_path.to_owned(),
                status: "unsafe",
                expected_sha256: digest_bytes(safe_file.contents.as_bytes()),
                sha256: None,
            });
            continue;
        }
        match fs::read(&path) {
            Ok(contents) => {
                let is_safe = contents == safe_file.contents.as_bytes();
                if !is_safe {
                    custom_file_count += 1;
                }
                fingerprints.push(FileFingerprint {
                    relative_path: safe_file.relative_path.to_owned(),
                    status: if is_safe { "safe" } else { "custom" },
                    expected_sha256: digest_bytes(safe_file.contents.as_bytes()),
                    sha256: Some(digest_bytes(&contents)),
                });
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                missing_files.push(safe_file);
                fingerprints.push(FileFingerprint {
                    relative_path: safe_file.relative_path.to_owned(),
                    status: "missing",
                    expected_sha256: digest_bytes(safe_file.contents.as_bytes()),
                    sha256: None,
                });
            },
            Err(error) => {
                blockers.push(format!("cannot read {}: {error}", path.display()));
                fingerprints.push(FileFingerprint {
                    relative_path: safe_file.relative_path.to_owned(),
                    status: "unreadable",
                    expected_sha256: digest_bytes(safe_file.contents.as_bytes()),
                    sha256: None,
                });
            },
        }
    }

    let extra_files = if config_path_is_safe {
        collect_extra_config_files(config_path, &mut blockers)
    } else {
        Vec::new()
    };
    let database_path = data_path.join("aether.db");
    let database_wal_path = sqlite_sidecar_path(&database_path, "-wal");
    let database_paths_are_safe = data_path_is_safe
        && record_path_validation(&database_path, "setup database", &mut blockers)
        && record_path_validation(&database_wal_path, "setup database WAL", &mut blockers);
    let database = if database_paths_are_safe {
        inspect_database(&database_path, &mut blockers).await
    } else {
        DatabaseInspection::default()
    };

    if database
        .user_version
        .is_some_and(|version| version > i64::from(schema::SCHEMA_VERSION))
    {
        blockers.push(format!(
            "database schema v{} is newer than this aether build (v{})",
            database.user_version.unwrap_or_default(),
            schema::SCHEMA_VERSION
        ));
    }

    let has_custom_site_files = custom_file_count > 0 || !extra_files.is_empty();
    let all_required_missing = missing_files.len() == SAFE_CONFIG_FILES.len();
    let all_required_present = missing_files.is_empty();

    let site_state = if !blockers.is_empty() {
        SiteState::Blocked
    } else if database.has_commissioned_rows() {
        SiteState::Existing
    } else if has_custom_site_files && !all_required_present {
        blockers.push(
            "custom site content is mixed with missing required files; setup will not infer the missing configuration"
                .to_owned(),
        );
        SiteState::Blocked
    } else if has_custom_site_files {
        SiteState::Existing
    } else if all_required_missing && !database.exists {
        SiteState::Fresh
    } else if all_required_present
        && database.exists
        && database.user_version == Some(i64::from(schema::SCHEMA_VERSION))
        && database.has_complete_core_schema()
        && database.has_completed_sync()
    {
        SiteState::SafeReady
    } else {
        SiteState::SafePartial
    };

    let actions = actions_for(site_state, &missing_files, &database);
    let requires_confirmation = matches!(site_state, SiteState::Fresh | SiteState::SafePartial);
    let database_guard = digest_bytes(
        &serde_json::to_vec(&database)
            .context("failed to serialize setup database guard fingerprint")?,
    );
    let hash_input = PlanHashInput {
        plan_schema_version: SETUP_PLAN_SCHEMA_VERSION,
        aether_version: env!("CARGO_PKG_VERSION"),
        core_schema_version: i64::from(schema::SCHEMA_VERSION),
        config_path: config_path.display().to_string(),
        data_path: data_path.display().to_string(),
        site_state,
        files: &fingerprints,
        extra_files: &extra_files,
        database: &database,
        actions: &actions,
        blockers: &blockers,
    };
    let plan_id = digest_bytes(
        &serde_json::to_vec(&hash_input).context("failed to serialize setup plan fingerprint")?,
    );
    let apply_argv = requires_confirmation.then(|| {
        vec![
            "aether".to_owned(),
            "--config-path".to_owned(),
            config_path.display().to_string(),
            "--db-path".to_owned(),
            data_path.display().to_string(),
            "setup".to_owned(),
            "apply".to_owned(),
            "--plan-id".to_owned(),
            plan_id.clone(),
        ]
    });
    let next_step = match site_state {
        SiteState::Fresh | SiteState::SafePartial => {
            "review and execute the structured apply_argv".to_owned()
        },
        SiteState::SafeReady => "aether services start".to_owned(),
        SiteState::Existing => "aether doctor".to_owned(),
        SiteState::Blocked => {
            "resolve the reported blockers, then run aether setup again".to_owned()
        },
    };

    Ok(SetupAnalysis {
        plan: SetupPlan {
            plan_id,
            plan_schema_version: SETUP_PLAN_SCHEMA_VERSION,
            aether_version: env!("CARGO_PKG_VERSION"),
            core_schema_version: i64::from(schema::SCHEMA_VERSION),
            site_state,
            read_only: true,
            physical_side_effects: false,
            config_path: config_path.display().to_string(),
            data_path: data_path.display().to_string(),
            database_path: database_path.display().to_string(),
            actions,
            blockers,
            requires_confirmation,
            apply_argv,
            next_step,
        },
        missing_files,
        database_guard,
    })
}

fn actions_for(
    site_state: SiteState,
    missing_files: &[SafeConfigFile],
    database: &DatabaseInspection,
) -> Vec<SetupAction> {
    match site_state {
        SiteState::Fresh | SiteState::SafePartial => vec![
            SetupAction {
                id: "config.ensure_safe_files",
                description: if missing_files.is_empty() {
                    "Keep the existing exact safe configuration files".to_owned()
                } else {
                    format!(
                        "Create only these missing safe files: {}",
                        missing_files
                            .iter()
                            .map(|file| file.relative_path)
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                },
                persistent_write: !missing_files.is_empty(),
                physical_side_effects: false,
            },
            SetupAction {
                id: "config.validate_staged",
                description: "Validate the complete staged site with the normal atomic sync path"
                    .to_owned(),
                persistent_write: false,
                physical_side_effects: false,
            },
            SetupAction {
                id: "database.initialize",
                description: "Initialize or safely migrate the local SQLite schema".to_owned(),
                persistent_write: !database.exists
                    || database.user_version != Some(i64::from(schema::SCHEMA_VERSION))
                    || !database.has_complete_core_schema(),
                physical_side_effects: false,
            },
            SetupAction {
                id: "config.sync_empty_runtime",
                description: "Atomically sync the safe empty configuration into local SQLite"
                    .to_owned(),
                persistent_write: true,
                physical_side_effects: false,
            },
        ],
        SiteState::SafeReady => vec![SetupAction {
            id: "site.already_configured",
            description: "No persistent change; the safe empty runtime is already configured"
                .to_owned(),
            persistent_write: false,
            physical_side_effects: false,
        }],
        SiteState::Existing => vec![SetupAction {
            id: "site.preserve_existing",
            description: "No persistent change; preserve the existing commissioned site".to_owned(),
            persistent_write: false,
            physical_side_effects: false,
        }],
        SiteState::Blocked => vec![SetupAction {
            id: "site.resolve_blockers",
            description: "No persistent change; resolve blockers before setup can continue"
                .to_owned(),
            persistent_write: false,
            physical_side_effects: false,
        }],
    }
}

async fn inspect_database(path: &Path, blockers: &mut Vec<String>) -> DatabaseInspection {
    let exists = match path.try_exists() {
        Ok(exists) => exists,
        Err(error) => {
            blockers.push(format!(
                "cannot inspect database path {}: {error}",
                path.display()
            ));
            return DatabaseInspection::default();
        },
    };
    if !exists {
        return DatabaseInspection {
            readable: true,
            ..DatabaseInspection::default()
        };
    }

    let mut inspection = DatabaseInspection {
        exists: true,
        readable: false,
        ..DatabaseInspection::default()
    };
    let snapshot = match create_stable_database_snapshot(path) {
        Ok(snapshot) => snapshot,
        Err(error) => {
            blockers.push(format!(
                "cannot create a stable read-only snapshot of database {}: {error:#}",
                path.display()
            ));
            return inspection;
        },
    };
    inspection.sha256 = Some(snapshot.fingerprint.sha256.clone());
    inspection.wal_sha256 = snapshot.fingerprint.wal_sha256.clone();

    // The connection may create or update SQLite sidecars, but only beside the
    // temporary copy. The live site database and its WAL remain untouched.
    let options = SqliteConnectOptions::new().filename(&snapshot.path);
    let pool = match SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
    {
        Ok(pool) => pool,
        Err(error) => {
            blockers.push(format!(
                "cannot open the read-only snapshot of database {}: {error}",
                path.display()
            ));
            return inspection;
        },
    };

    let result = async {
        inspection.user_version = Some(sqlx::query_scalar("PRAGMA user_version").fetch_one(&pool).await?);
        inspection.tables = sqlx::query_scalar::<_, String>(
            "SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%' ORDER BY name",
        )
        .fetch_all(&pool)
        .await?;
        inspection.channels = table_count(&pool, &inspection.tables, "channels").await?;
        inspection.instances = table_count(&pool, &inspection.tables, "instances").await?;
        inspection.rules = table_count(&pool, &inspection.tables, "rules").await?;
        inspection.sync_metadata_rows =
            table_count(&pool, &inspection.tables, "sync_metadata").await?;
        if inspection
            .tables
            .iter()
            .any(|table| table == "sync_metadata")
        {
            inspection.synced_services = sqlx::query_scalar::<_, String>(
                "SELECT service FROM sync_metadata \
                 WHERE service IN ('automation', 'global', 'io') ORDER BY service",
            )
            .fetch_all(&pool)
            .await?;
        }
        Result::<(), sqlx::Error>::Ok(())
    }
    .await;

    match result {
        Ok(()) => {
            inspection.readable = true;
            let known_core_tables = ["channels", "instances", "rules", "service_config"];
            let known_count = known_core_tables
                .iter()
                .filter(|name| inspection.tables.iter().any(|table| table == **name))
                .count();
            if known_count != 0 && known_count != known_core_tables.len() {
                blockers.push(format!(
                    "database {} contains only part of the Aether core schema",
                    path.display()
                ));
            } else if known_count == 0 && !inspection.tables.is_empty() {
                blockers.push(format!(
                    "database {} is not recognized as an Aether database",
                    path.display()
                ));
            }
        },
        Err(error) => blockers.push(format!(
            "cannot inspect the read-only snapshot of database {}: {error}",
            path.display()
        )),
    }
    pool.close().await;
    inspection
}

fn create_stable_database_snapshot(source: &Path) -> Result<DatabaseSnapshot> {
    const MAX_ATTEMPTS: usize = 4;

    let mut last_error = None;
    for _ in 0..MAX_ATTEMPTS {
        match try_create_database_snapshot(source) {
            Ok(Some(snapshot)) => return Ok(snapshot),
            Ok(None) => {},
            Err(error) => last_error = Some(error),
        }
    }

    if let Some(error) = last_error {
        bail!(
            "database changed or could not be copied consistently after {MAX_ATTEMPTS} attempts: {error:#}"
        );
    }
    bail!("database changed during all {MAX_ATTEMPTS} snapshot attempts")
}

fn try_create_database_snapshot(source: &Path) -> Result<Option<DatabaseSnapshot>> {
    let fingerprint_before = fingerprint_database(source)?;
    let directory = tempfile::tempdir().context("failed to create database snapshot directory")?;
    let snapshot_path = directory.path().join("aether.db");
    fs::copy(source, &snapshot_path).with_context(|| {
        format!(
            "failed to copy database {} to a private snapshot",
            source.display()
        )
    })?;

    let source_wal = sqlite_sidecar_path(source, "-wal");
    if fingerprint_before.wal_sha256.is_some() {
        let snapshot_wal = sqlite_sidecar_path(&snapshot_path, "-wal");
        fs::copy(&source_wal, &snapshot_wal).with_context(|| {
            format!(
                "failed to copy live WAL {} to a private snapshot",
                source_wal.display()
            )
        })?;
    }

    let fingerprint_after = fingerprint_database(source)?;
    let snapshot_fingerprint = fingerprint_database(&snapshot_path)?;
    if fingerprint_before != fingerprint_after || fingerprint_after != snapshot_fingerprint {
        return Ok(None);
    }

    Ok(Some(DatabaseSnapshot {
        _directory: directory,
        path: snapshot_path,
        fingerprint: fingerprint_after,
    }))
}

fn fingerprint_database(path: &Path) -> Result<DatabaseFingerprint> {
    Ok(DatabaseFingerprint {
        sha256: digest_file(path)
            .with_context(|| format!("failed to fingerprint database {}", path.display()))?,
        wal_sha256: digest_optional_file(&sqlite_sidecar_path(path, "-wal"))?,
    })
}

fn sqlite_sidecar_path(database: &Path, suffix: &str) -> PathBuf {
    let mut sidecar = database.as_os_str().to_os_string();
    sidecar.push(suffix);
    PathBuf::from(sidecar)
}

async fn table_count(
    pool: &sqlx::SqlitePool,
    tables: &[String],
    table: &str,
) -> std::result::Result<i64, sqlx::Error> {
    if !tables.iter().any(|candidate| candidate == table) {
        return Ok(0);
    }
    sqlx::query_scalar(&format!("SELECT COUNT(*) FROM {table}"))
        .fetch_one(pool)
        .await
}

fn collect_extra_config_files(config_path: &Path, blockers: &mut Vec<String>) -> Vec<String> {
    if !config_path.exists() {
        return Vec::new();
    }

    let allowed = SAFE_CONFIG_FILES
        .iter()
        .map(|file| file.relative_path)
        .chain(NON_RUNTIME_DISTRIBUTION_FILES)
        .collect::<BTreeSet<_>>();
    let mut extras = Vec::new();
    collect_extra_config_files_in(config_path, config_path, &allowed, &mut extras, blockers);
    extras.sort();
    extras
}

fn collect_extra_config_files_in(
    root: &Path,
    directory: &Path,
    allowed: &BTreeSet<&str>,
    extras: &mut Vec<String>,
    blockers: &mut Vec<String>,
) {
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) => {
            blockers.push(format!("cannot read {}: {error}", directory.display()));
            return;
        },
    };

    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                blockers.push(format!("cannot enumerate {}: {error}", directory.display()));
                continue;
            },
        };
        let path = entry.path();
        let relative = match path.strip_prefix(root) {
            Ok(relative) => relative,
            Err(error) => {
                blockers.push(format!("cannot classify {}: {error}", path.display()));
                continue;
            },
        };
        let relative = relative.to_string_lossy().replace('\\', "/");
        let file_type = match entry.file_type() {
            Ok(file_type) => file_type,
            Err(error) => {
                blockers.push(format!("cannot inspect {}: {error}", path.display()));
                continue;
            },
        };
        if file_type.is_dir() {
            collect_extra_config_files_in(root, &path, allowed, extras, blockers);
        } else if !allowed.contains(relative.as_str()) {
            extras.push(relative);
        }
    }
}

async fn apply(
    config_path: &Path,
    data_path: &Path,
    expected_plan_id: &str,
) -> Result<ApplyResult> {
    apply_with_precommit_hook(config_path, data_path, expected_plan_id, || Ok(())).await
}

async fn apply_with_precommit_hook<F>(
    config_path: &Path,
    data_path: &Path,
    expected_plan_id: &str,
    precommit_hook: F,
) -> Result<ApplyResult>
where
    F: FnOnce() -> Result<()>,
{
    let analysis = analyze(config_path, data_path).await?;
    if analysis.plan.plan_id != expected_plan_id {
        bail!(
            "stale setup plan: expected {}, but the current site plan is {}; run `aether setup` again",
            expected_plan_id,
            analysis.plan.plan_id
        );
    }

    match analysis.plan.site_state {
        SiteState::Existing => {
            bail!("setup will not modify an existing site; use `aether doctor` to inspect it")
        },
        SiteState::Blocked => {
            bail!("setup is blocked: {}", analysis.plan.blockers.join("; "))
        },
        SiteState::SafeReady => {
            return Ok(ApplyResult {
                plan_id: analysis.plan.plan_id,
                applied: false,
                configured: true,
                ready: false,
                physical_side_effects: false,
                created_files: Vec::new(),
                next_steps: ["aether services start", "aether doctor"],
            });
        },
        SiteState::Fresh | SiteState::SafePartial => {},
    }

    let _lock = SetupLock::acquire(data_path)?;
    let analysis_after_lock = analyze(config_path, data_path).await?;
    if analysis_after_lock.plan.plan_id != analysis.plan.plan_id {
        bail!("site changed during setup lock acquisition; no database changes were applied");
    }

    let staged_config = validate_complete_safe_site().await?;

    let mut created_files = Vec::new();
    for safe_file in analysis_after_lock.missing_files {
        let destination = config_path.join(safe_file.relative_path);
        create_safe_file(&destination, safe_file.contents.as_bytes())
            .with_context(|| format!("failed to create safe config {}", destination.display()))?;
        created_files.push(destination.display().to_string());
    }

    precommit_hook()?;
    let precommit_analysis = analyze(config_path, data_path).await?;
    if precommit_analysis.plan.site_state != SiteState::SafePartial
        || !precommit_analysis.missing_files.is_empty()
        || precommit_analysis.database_guard != analysis_after_lock.database_guard
    {
        bail!("site changed during setup; refusing to write the database");
    }

    let database_path = data_path.join("aether.db");
    validate_no_symlink_components(data_path, "setup data directory")?;
    validate_no_symlink_components(&database_path, "setup database")?;
    validate_no_symlink_components(
        &sqlite_sidecar_path(&database_path, "-wal"),
        "setup database WAL",
    )?;
    schema::init_database(&database_path)
        .await
        .with_context(|| format!("failed to initialize {}", database_path.display()))?;
    let core = AetherCore::readwrite(data_path, staged_config.path(), "all").await?;
    core.sync_empty_site()
        .await
        .context("failed to atomically sync the still-empty safe runtime")?;

    Ok(ApplyResult {
        plan_id: analysis_after_lock.plan.plan_id,
        applied: true,
        configured: true,
        ready: false,
        physical_side_effects: false,
        created_files,
        next_steps: ["aether services start", "aether doctor"],
    })
}

async fn validate_complete_safe_site() -> Result<tempfile::TempDir> {
    let staged_config = tempfile::tempdir().context("failed to create staged config directory")?;
    for safe_file in SAFE_CONFIG_FILES {
        let path = staged_config.path().join(safe_file.relative_path);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).with_context(|| {
                format!("failed to create staged directory {}", parent.display())
            })?;
        }
        fs::write(&path, safe_file.contents)
            .with_context(|| format!("failed to stage {}", path.display()))?;
    }

    let core = AetherCore::new(staged_config.path());
    for service in ["global", "aether-io", "aether-automation"] {
        let validation = core
            .validate(service)
            .await
            .with_context(|| format!("failed to validate staged {service} configuration"))?;
        if !validation.is_valid {
            bail!(
                "embedded safe {service} configuration is invalid: {}",
                validation.errors.join("; ")
            );
        }
    }

    let staged_data = tempfile::tempdir().context("failed to create staged database directory")?;
    let staged_core =
        AetherCore::readwrite(staged_data.path(), staged_config.path(), "all").await?;
    staged_core
        .sync_all(false)
        .await
        .context("embedded safe configuration failed the complete atomic dry-run")?;
    Ok(staged_config)
}

fn create_safe_file(path: &Path, contents: &[u8]) -> Result<()> {
    validate_no_symlink_components(path, "safe configuration file")?;
    let parent = path
        .parent()
        .with_context(|| format!("{} has no parent directory", path.display()))?;
    fs::create_dir_all(parent).with_context(|| format!("failed to create {}", parent.display()))?;

    let mut temporary = tempfile::NamedTempFile::new_in(parent)
        .with_context(|| format!("failed to stage a file in {}", parent.display()))?;
    temporary
        .write_all(contents)
        .with_context(|| format!("failed to write staged file for {}", path.display()))?;
    temporary
        .flush()
        .with_context(|| format!("failed to flush staged file for {}", path.display()))?;
    temporary
        .as_file()
        .sync_all()
        .with_context(|| format!("failed to sync staged file for {}", path.display()))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        temporary
            .as_file()
            .set_permissions(fs::Permissions::from_mode(0o644))
            .with_context(|| format!("failed to set permissions for {}", path.display()))?;
    }

    temporary.persist_noclobber(path).map_err(|error| {
        anyhow::anyhow!(
            "refusing to overwrite {} after planning: {}",
            path.display(),
            error.error
        )
    })?;
    Ok(())
}

fn digest_file(path: &Path) -> Result<String> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 16 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn digest_optional_file(path: &Path) -> Result<Option<String>> {
    match path.try_exists() {
        Ok(true) => digest_file(path)
            .map(Some)
            .with_context(|| format!("failed to fingerprint {}", path.display())),
        Ok(false) => Ok(None),
        Err(error) => {
            Err(error).with_context(|| format!("failed to inspect sidecar path {}", path.display()))
        },
    }
}

fn digest_bytes(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn print_plan(plan: &SetupPlan, json: bool) {
    if json {
        crate::output::print_success(plan);
        return;
    }

    println!("Aether setup plan ({:?})", plan.site_state);
    println!("Plan ID: {}", plan.plan_id);
    println!("Read-only planning: yes");
    println!("Physical side effects: none");
    for action in &plan.actions {
        println!("- {}", action.description);
    }
    for blocker in &plan.blockers {
        println!("BLOCKED: {blocker}");
    }
    if let Some(argv) = &plan.apply_argv
        && let Ok(serialized) = serde_json::to_string(argv)
    {
        println!("Next argv: {serialized}");
    } else {
        println!("Next: {}", plan.next_step);
    }
}

fn print_apply_result(result: &ApplyResult, json: bool) {
    if json {
        crate::output::print_success(result);
        return;
    }

    if result.applied {
        println!("Safe empty Aether runtime configured.");
    } else {
        println!("Safe empty Aether runtime was already configured; no files were changed.");
    }
    println!("No service was started and no device, rule, or domain pack was enabled.");
    println!("Next: {}", result.next_steps.join("; "));
}

#[cfg(test)]
mod tests {
    use super::{SAFE_CONFIG_FILES, analyze, apply_with_precommit_hook};
    use std::fs;

    #[tokio::test]
    async fn concurrent_config_change_is_rejected_before_database_creation() {
        let workspace = tempfile::tempdir().unwrap();
        let config_path = workspace.path().join("config");
        let data_path = workspace.path().join("data");
        fs::create_dir_all(&config_path).unwrap();
        fs::write(
            config_path.join(SAFE_CONFIG_FILES[0].relative_path),
            SAFE_CONFIG_FILES[0].contents,
        )
        .unwrap();
        let plan = analyze(&config_path, &data_path).await.unwrap();

        let global_path = config_path.join("global.yaml");
        let error = apply_with_precommit_hook(&config_path, &data_path, &plan.plan.plan_id, || {
            fs::write(&global_path, "site_name: changed-during-apply\n")?;
            Ok(())
        })
        .await
        .unwrap_err();

        assert!(format!("{error:#}").contains("changed during setup"));
        assert!(!data_path.join("aether.db").exists());
    }
}
