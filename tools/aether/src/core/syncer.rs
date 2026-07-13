//! Configuration synchronization module
//!
//! This module is responsible for syncing configuration from YAML/CSV files
//! to the SQLite database.

use aether_config::automation::AutomationConfig;
use aether_config::io::IoConfig;
use anyhow::{Context, Result};
use common::validation::CsvFields;
use serde::de::DeserializeOwned;
use serde_json::Value as JsonValue;
use sqlx::{Sqlite, Transaction};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tracing::{debug, info, warn};

use super::file_utils::{flatten_json, load_csv, load_csv_typed_with_errors, load_csv_with_errors};
use super::schema;

const ACTION_ROUTING_SYNC_GUARD: &str = "action routing requires the governed action-routing command; configuration sync supports measurement routing only";

/// Try parsing a string as a JSON number. Empty strings default to 0.
fn str_to_json_number(s: &str) -> Option<JsonValue> {
    use serde_json::Number;
    let trimmed = s.trim();
    if trimmed.is_empty() {
        Some(JsonValue::Number(Number::from(0)))
    } else if let Ok(n) = trimmed.parse::<i64>() {
        Some(JsonValue::Number(Number::from(n)))
    } else if let Ok(f) = trimmed.parse::<f64>() {
        Number::from_f64(f).map(JsonValue::Number)
    } else {
        None
    }
}

/// Convert mapping entries: numeric_fields become JSON numbers, rest stay strings.
fn convert_fields(
    mapping: HashMap<String, String>,
    numeric_fields: &[&str],
) -> HashMap<String, JsonValue> {
    mapping
        .into_iter()
        .map(|(k, v)| {
            let json_val = if numeric_fields.contains(&k.as_str()) {
                str_to_json_number(&v).unwrap_or(JsonValue::String(v))
            } else {
                JsonValue::String(v)
            };
            (k, json_val)
        })
        .collect()
}

/// Normalize protocol_data numeric fields to JSON numbers (not strings)
///
/// Ensures type consistency between CSV import and runtime API operations.
/// Modbus/CAN numeric fields (slave_id, function_code, etc.) should be numbers.
#[allow(clippy::disallowed_methods)] // from_f64(1.0/0.0).unwrap() is safe for valid f64 constants
fn normalize_protocol_mapping(
    protocol: &str,
    mut mapping: HashMap<String, String>,
) -> HashMap<String, JsonValue> {
    use serde_json::Number;

    // point_id is stored in a separate column
    mapping.remove("point_id");

    match protocol {
        "modbus_tcp" | "modbus_rtu" | "sunspec_tcp" | "sunspec_rtu" => {
            let mut normalized = convert_fields(
                mapping,
                &[
                    "slave_id",
                    "function_code",
                    "register_address",
                    "bit_position",
                ],
            );
            // Ensure bit_position exists and is an integer (convert 0.0 → 0)
            let bp = normalized
                .entry("bit_position".to_string())
                .or_insert(JsonValue::Number(Number::from(0)));
            if let Some(f) = bp.as_f64() {
                *bp = JsonValue::Number(Number::from(f.round() as i64));
            }
            normalized
        },
        "can" => {
            let mut normalized = convert_fields(
                mapping,
                &["can_id", "start_bit", "bit_length", "scale", "offset"],
            );
            normalized
                .entry("signed".to_string())
                .or_insert(JsonValue::Bool(false));
            normalized
                .entry("scale".to_string())
                .or_insert(JsonValue::Number(Number::from(1)));
            normalized
                .entry("offset".to_string())
                .or_insert(JsonValue::Number(Number::from(0)));
            normalized
        },
        "di_do" | "gpio" | "dido" => convert_fields(mapping, &["gpio_number"]),
        _ => convert_fields(mapping, &[]),
    }
}

/// Error that occurred during sync
#[derive(Debug, Clone)]
pub struct SyncError {
    /// Item that caused the error
    pub item: String,
    /// Error message
    pub error: String,
}

impl SyncError {
    /// Convert CSV row error to sync error
    pub fn from_csv_error(csv_error: &crate::core::file_utils::CsvRowError, context: &str) -> Self {
        Self {
            item: format!("{}:row-{}", context, csv_error.row_number),
            error: csv_error.error.clone(),
        }
    }
}

// ============================================================================
// Point Type Sync Helpers
// ============================================================================

/// Configuration for syncing a specific point type (T/S/C/A)
struct PointSyncConfig<'a> {
    /// CSV filename (e.g., "telemetry.csv")
    csv_filename: &'a str,
    /// Mapping filename (e.g., "mapping/telemetry_mapping.csv")
    mapping_filename: &'a str,
    /// Database table name (e.g., "telemetry_points")
    table_name: &'a str,
    /// Type label for error messages (e.g., "telemetry")
    type_label: &'a str,
}

/// Extracted point fields for database insertion
struct PointFields {
    point_id: u32,
    signal_name: String,
    unit: Option<String>,
    description: Option<String>,
    scale: f64,
    offset: f64,
    reverse: bool,
    data_type: String,
}

/// Generic point type sync function
///
/// Syncs points of a specific type (T/S/C/A) from CSV to SQLite.
/// Handles CSV loading, mapping file loading, and database insertion.
#[allow(clippy::too_many_arguments)]
async fn sync_point_type<T, F>(
    tx: &mut Transaction<'_, Sqlite>,
    path: &Path,
    channel_id: i32,
    protocol: &str,
    config: &PointSyncConfig<'_>,
    extract_fields: F,
    errors: &mut Vec<SyncError>,
) -> Result<usize>
where
    T: DeserializeOwned + CsvFields,
    F: Fn(&T) -> PointFields,
{
    let csv_file = path.join(config.csv_filename);
    if !csv_file.exists() {
        return Ok(0);
    }

    let (points, csv_errors) = load_csv_typed_with_errors::<T, _>(&csv_file)?;

    // Collect CSV parsing errors
    for csv_error in &csv_errors {
        errors.push(SyncError::from_csv_error(
            csv_error,
            &format!("channel-{}/{}", channel_id, config.csv_filename),
        ));
    }

    // Load corresponding mappings if they exist
    let mapping_file = path.join(config.mapping_filename);
    let mappings_json = if mapping_file.exists() {
        let (mappings, mapping_csv_errors) = load_csv_with_errors(&mapping_file)?;

        // Collect mapping CSV errors
        for csv_error in &mapping_csv_errors {
            errors.push(SyncError::from_csv_error(
                csv_error,
                &format!("channel-{}/{}", channel_id, config.mapping_filename),
            ));
        }

        // Normalize and convert to JSON, indexed by point_id
        let mut mapping_map = HashMap::new();
        for mapping in mappings {
            if let Some(point_id) = mapping.get("point_id") {
                let point_id = point_id.clone();
                let normalized = normalize_protocol_mapping(protocol, mapping);
                mapping_map.insert(point_id, normalized);
            }
        }
        Some(mapping_map)
    } else {
        None
    };

    let mut count = 0;

    // Update in place so dependent rows are not affected by SQLite REPLACE's
    // implicit DELETE. Full replacement is handled explicitly before this
    // function when force mode is enabled.
    let insert_sql = format!(
        "INSERT INTO {} (point_id, channel_id, signal_name, scale, offset, unit, reverse, data_type, description, protocol_mappings)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT(channel_id, point_id) DO UPDATE SET
             signal_name = excluded.signal_name,
             scale = excluded.scale,
             offset = excluded.offset,
             unit = excluded.unit,
             reverse = excluded.reverse,
             data_type = excluded.data_type,
             description = excluded.description,
             protocol_mappings = excluded.protocol_mappings",
        config.table_name
    );

    for point in points {
        let fields = extract_fields(&point);

        let protocol_mappings = mappings_json
            .as_ref()
            .and_then(|m| m.get(&fields.point_id.to_string()))
            .map(|m| serde_json::to_string(m).unwrap_or_else(|_| "{}".to_string()))
            .unwrap_or_else(|| "null".to_string());

        if let Err(e) = sqlx::query(&insert_sql)
            .bind(fields.point_id)
            .bind(channel_id)
            .bind(&fields.signal_name)
            .bind(fields.scale)
            .bind(fields.offset)
            .bind(&fields.unit)
            .bind(fields.reverse)
            .bind(&fields.data_type)
            .bind(&fields.description)
            .bind(&protocol_mappings)
            .execute(&mut **tx)
            .await
        {
            errors.push(SyncError {
                item: format!(
                    "channel-{}/{}/point-{}",
                    channel_id, config.type_label, fields.point_id
                ),
                error: e.to_string(),
            });
            continue;
        }
        count += 1;
    }

    Ok(count)
}

/// Result of a sync operation
#[derive(Debug, Default)]
pub struct SyncResult {
    /// Number of items synced
    pub items_synced: usize,
    /// Number of items deleted
    pub items_deleted: usize,
    /// Errors encountered during sync
    pub errors: Vec<SyncError>,
}

/// Configuration syncer
pub struct ConfigSyncer {
    config_path: PathBuf,
    db_path: PathBuf,
    /// When true, DELETE all rows before INSERT (full replace). Default: false (UPSERT).
    force: bool,
    /// Require site-level entities to remain empty until this sync commits.
    require_empty_site: bool,
}

impl ConfigSyncer {
    /// Create a new syncer (default: UPSERT mode)
    pub fn new(config_path: impl AsRef<Path>, db_path: impl AsRef<Path>) -> Self {
        Self {
            config_path: config_path.as_ref().to_path_buf(),
            db_path: db_path.as_ref().to_path_buf(),
            force: false,
            require_empty_site: false,
        }
    }

    /// Set force mode: DELETE all rows before INSERT (destructive full replace)
    pub fn with_force(mut self, force: bool) -> Self {
        self.force = force;
        self
    }

    /// Reserve the SQLite writer lock and reject commissioned rows before any
    /// configuration writes. This guard is intended only for first-run setup.
    pub fn requiring_empty_site(mut self) -> Self {
        self.require_empty_site = true;
        self
    }

    /// Sync every configuration domain inside one SQLite transaction.
    ///
    /// This is the site-level apply primitive used by the CLI. Readers either
    /// observe the previous configuration or the complete replacement; a
    /// parser, filesystem, or database error in a later domain rolls back all
    /// earlier writes.
    pub async fn sync_all(&self) -> Result<Vec<(&'static str, SyncResult)>> {
        let db_file = self.db_path.join("aether.db");
        schema::init_database(&db_file).await?;
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .connect_with(common::bootstrap_database::sqlite_connect_options(
                db_file.to_str().unwrap_or_default(),
            ))
            .await
            .context("Failed to connect to unified configuration database")?;
        let mut tx = if self.require_empty_site {
            pool.begin_with("BEGIN IMMEDIATE").await?
        } else {
            pool.begin().await?
        };

        if self.force {
            let action_route_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM action_routing")
                .fetch_one(&mut *tx)
                .await?;
            if action_route_count != 0 {
                anyhow::bail!(
                    "{ACTION_ROUTING_SYNC_GUARD}; --force would mutate \
                     {action_route_count} existing action route(s)"
                );
            }
        }

        if self.require_empty_site {
            for table in ["channels", "instances", "rules"] {
                let count: i64 = sqlx::query_scalar(&format!("SELECT COUNT(*) FROM {table}"))
                    .fetch_one(&mut *tx)
                    .await?;
                if count != 0 {
                    anyhow::bail!(
                        "setup requires an empty site, but {table} contains {count} row(s)"
                    );
                }
            }
        }

        let global = self.sync_global_in_transaction(&mut tx).await?;
        let io = self.sync_io_in_transaction(&mut tx).await?;
        let automation = self.sync_automation_in_transaction(&mut tx).await?;

        let results = vec![
            ("global", global),
            ("aether-io", io),
            ("aether-automation", automation),
        ];
        if let Some((service, result)) =
            results.iter().find(|(_, result)| !result.errors.is_empty())
        {
            let details = result
                .errors
                .iter()
                .map(|error| format!("{}: {}", error.item, error.error))
                .collect::<Vec<_>>()
                .join("; ");
            anyhow::bail!("Configuration errors in {service}: {details}");
        }

        if self.require_empty_site {
            for table in ["channels", "instances", "rules"] {
                let count: i64 = sqlx::query_scalar(&format!("SELECT COUNT(*) FROM {table}"))
                    .fetch_one(&mut *tx)
                    .await?;
                if count != 0 {
                    anyhow::bail!(
                        "setup requires an empty site, but the synced configuration would leave {count} row(s) in {table}"
                    );
                }
            }
        }

        tx.commit().await?;
        Ok(results)
    }

    async fn sync_global_in_transaction(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
    ) -> Result<SyncResult> {
        let mut stats = SyncResult::default();
        let global_yaml_path = self.config_path.join("global.yaml");

        // If global.yaml doesn't exist, skip (optional configuration)
        if !global_yaml_path.exists() {
            debug!("No global.yaml, skip");
            return Ok(stats);
        }

        debug!("Sync global: {:?}", global_yaml_path);

        // Read and parse YAML
        let yaml_content = std::fs::read_to_string(&global_yaml_path)
            .with_context(|| format!("Failed to read {:?}", global_yaml_path))?;
        let yaml_config: JsonValue =
            serde_yml::from_str(&yaml_content).context("Failed to parse global.yaml")?;

        // Insert global configuration
        let config_count = self
            .insert_service_config(tx, "global", &yaml_config)
            .await?;
        stats.items_synced += config_count;

        debug!("Global: {} items", config_count);

        // Update sync timestamp
        self.update_sync_timestamp(tx, "global").await?;

        info!("Global: {} synced", stats.items_synced);

        Ok(stats)
    }

    async fn sync_io_in_transaction(&self, tx: &mut Transaction<'_, Sqlite>) -> Result<SyncResult> {
        let mut stats = SyncResult::default();
        let config_dir = self.config_path.join("io");

        debug!("Sync io: {:?}", config_dir);

        let yaml_path = config_dir.join("io.yaml");
        let yaml_content = std::fs::read_to_string(&yaml_path)
            .with_context(|| format!("Failed to read {:?}", yaml_path))?;
        let io_config: IoConfig =
            serde_yml::from_str(&yaml_content).context("Failed to parse io.yaml")?;
        let mut yaml_config =
            serde_json::to_value(&io_config).context("Failed to convert config to JSON")?;
        let channels = yaml_config
            .as_object_mut()
            .and_then(|obj| obj.remove("channels"))
            .and_then(|value| value.as_array().cloned())
            .unwrap_or_default();

        let mut channel_names = std::collections::HashMap::new();
        for (index, channel) in channels.iter().enumerate() {
            if let Some(name) = channel.get("name").and_then(|value| value.as_str())
                && let Some(existing_index) = channel_names.insert(name.to_string(), index)
            {
                return Err(anyhow::anyhow!(
                    "Duplicate channel name '{}' found at indices {} and {}. \
                         Channel names must be unique. Please rename one of the channels in io.yaml.",
                    name,
                    existing_index,
                    index
                ));
            }
        }

        // In force mode: DELETE all rows before INSERT (full replace).
        // In default mode, rows are updated in place and unmatched rows remain.
        let mut deleted: u64 = 0;
        if self.force {
            for table in [
                "telemetry_points",
                "signal_points",
                "control_points",
                "adjustment_points",
                "channels",
            ] {
                deleted += sqlx::query(&format!("DELETE FROM {table}"))
                    .execute(&mut **tx)
                    .await?
                    .rows_affected();
            }
            deleted += sqlx::query("DELETE FROM service_config WHERE service_name = ?")
                .bind("aether-io")
                .execute(&mut **tx)
                .await?
                .rows_affected();
        }

        stats.items_deleted = deleted as usize;

        // Insert service configuration
        let config_count = self
            .insert_service_config(tx, "aether-io", &yaml_config)
            .await?;
        stats.items_synced += config_count;

        debug!("Config: {} items", config_count);

        // Insert channels first (before points, due to foreign key constraints)
        let channels_count = self.insert_channels(tx, &channels).await?;
        stats.items_synced += channels_count;

        debug!("Channels: {}", channels_count);

        // No global point definitions to insert - all points are channel-specific

        // Load and insert channel-specific points
        let channel_points_count = self
            .insert_channel_specific_points(tx, &config_dir, &mut stats.errors)
            .await?;
        stats.items_synced += channel_points_count;

        debug!("Points: {}", channel_points_count);

        // Update sync timestamp
        self.update_sync_timestamp(tx, "io").await?;

        info!(
            "Io: {} synced, {} del, {} err",
            stats.items_synced,
            stats.items_deleted,
            stats.errors.len()
        );

        Ok(stats)
    }

    async fn sync_automation_in_transaction(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
    ) -> Result<SyncResult> {
        let mut stats = SyncResult::default();
        let config_dir = self.config_path.join("automation");

        debug!("Sync automation: {:?}", config_dir);

        let yaml_path = config_dir.join("automation.yaml");
        let yaml_content = std::fs::read_to_string(&yaml_path)
            .with_context(|| format!("Failed to read {:?}", yaml_path))?;
        let _automation_config: AutomationConfig =
            serde_yml::from_str(&yaml_content).context("Failed to parse automation.yaml")?;
        let yaml_config = serde_yml::from_str::<JsonValue>(&yaml_content)
            .context("Failed to parse automation.yaml as JSON")?;

        // In force mode: DELETE all rows before INSERT (full replace).
        // In default mode, rows are updated in place and unmatched rows remain.
        if self.force {
            sqlx::query("DELETE FROM service_config WHERE service_name = ?")
                .bind("aether-automation")
                .execute(&mut **tx)
                .await?;

            // Delete in correct order: child tables first, parent tables last
            sqlx::query("DELETE FROM measurement_routing")
                .execute(&mut **tx)
                .await?;

            // Product definitions are filesystem/Pack inputs, not rows owned by
            // the generic kernel schema, so only instance state is cleared.
            sqlx::query("DELETE FROM instances")
                .execute(&mut **tx)
                .await?;

            stats.items_deleted = 3; // Cleared config, measurement routing, and instances
        }

        // Insert service configuration
        let config_count = self
            .insert_service_config(tx, "aether-automation", &yaml_config)
            .await?;
        stats.items_synced += config_count;

        debug!("Config: {}", config_count);

        // Validate external product JSON files if directory exists
        let products_dir = config_dir.join("products");
        if products_dir.is_dir() {
            let product_errors = aether_model::product_lib::validate_product_dir(&products_dir);
            for (filename, error) in &product_errors {
                stats.errors.push(SyncError {
                    item: format!("products/{}", filename),
                    error: error.clone(),
                });
            }
            if product_errors.is_empty() {
                // Load once to verify the explicit site-library contract.
                match aether_model::product_lib::ProductLibrary::load(Some(&products_dir)) {
                    Ok(lib) => {
                        info!("Products: {} (explicit site library)", lib.len());
                    },
                    Err(e) => {
                        stats.errors.push(SyncError {
                            item: "products/".to_string(),
                            error: format!("Failed to load product library: {}", e),
                        });
                    },
                }
            }
        }

        // Load and sync instances
        let instances_path = config_dir.join("instances.yaml");
        if instances_path.exists() {
            let instances_count = self
                .sync_instances(tx, &instances_path, &config_dir, &mut stats.errors)
                .await?;
            stats.items_synced += instances_count;
            debug!("Instances: {}", instances_count);
        }

        // Load and sync rules (part of automation)
        let rules_dir = config_dir.join("rules");
        // Force mode mirrors the configured rule set exactly. An absent rules
        // directory represents an empty set, so stale database rules must be
        // removed even when there are no files to iterate.
        if self.force {
            sqlx::query("DELETE FROM rules").execute(&mut **tx).await?;
            stats.items_deleted += 1;
        }
        if rules_dir.exists() {
            let rules_count = self.sync_rules(tx, &rules_dir).await?;
            stats.items_synced += rules_count;
            debug!("Rules: {}", rules_count);
        }

        // Update sync timestamp
        self.update_sync_timestamp(tx, "automation").await?;

        // Report errors if any
        if !stats.errors.is_empty() {
            warn!("{} sync errors:", stats.errors.len());
            for error in &stats.errors {
                warn!("  {}: {}", error.item, error.error);
            }
        }

        info!(
            "Automation: {} synced, {} del, {} err",
            stats.items_synced,
            stats.items_deleted,
            stats.errors.len()
        );

        Ok(stats)
    }

    // Helper methods

    /// Insert service configuration into database
    async fn insert_service_config(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        service_name: &str,
        config: &JsonValue,
    ) -> Result<usize> {
        if self.force {
            sqlx::query("DELETE FROM service_config WHERE service_name = ?")
                .bind(service_name)
                .execute(&mut **tx)
                .await?;
        }

        let flattened = flatten_json(config, None);
        let mut count = 0;

        for (key, value) in flattened {
            // Skip null values to prevent service-specific empty fields from overwriting global config
            if value.is_null() {
                continue;
            }

            let (value_str, value_type) = match value {
                JsonValue::String(s) => (s, "string"),
                JsonValue::Bool(b) => (b.to_string(), "boolean"),
                JsonValue::Number(n) => (n.to_string(), "number"),
                JsonValue::Array(a) => (serde_json::to_string(&JsonValue::Array(a))?, "array"),
                JsonValue::Object(o) => (serde_json::to_string(&JsonValue::Object(o))?, "object"),
                JsonValue::Null => continue,
            };

            sqlx::query(
                "INSERT INTO service_config (service_name, key, value, type) VALUES (?, ?, ?, ?)
                 ON CONFLICT(service_name, key) DO UPDATE SET
                     value = excluded.value,
                     type = excluded.type,
                     updated_at = CURRENT_TIMESTAMP",
            )
            .bind(service_name)
            .bind(&key)
            .bind(&value_str)
            .bind(value_type)
            .execute(&mut **tx)
            .await?;

            count += 1;
        }

        Ok(count)
    }

    /// Insert channels
    async fn insert_channels(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        channels: &[JsonValue],
    ) -> Result<usize> {
        let mut count = 0;
        for channel in channels {
            // Parse channel ID (must be u16 as defined in ChannelConfig)
            let channel_id = match channel.get("id").and_then(|v| v.as_u64()) {
                Some(id) if id > 0 && id <= u16::MAX as u64 => id as i32,
                Some(id) => {
                    warn!("Invalid channel ID {}: skip", id);
                    continue;
                },
                None => {
                    warn!("Channel missing id: skip");
                    continue;
                },
            };

            let name = channel
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let protocol = channel
                .get("protocol")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let enabled = channel
                .get("enabled")
                .and_then(|v| v.as_bool())
                .unwrap_or(true);

            // Only serialize parameters, logging, and description (not core fields)
            // Core fields (id, name, protocol, enabled) are stored in dedicated columns
            let mut config_obj = serde_json::Map::new();

            if let Some(params) = channel.get("parameters") {
                config_obj.insert("parameters".to_string(), params.clone());
            }

            if let Some(logging) = channel.get("logging") {
                config_obj.insert("logging".to_string(), logging.clone());
            }

            if let Some(desc) = channel.get("description") {
                config_obj.insert("description".to_string(), desc.clone());
            }

            let config = serde_json::to_string(&config_obj)?;

            sqlx::query(
                "INSERT INTO channels (channel_id, name, protocol, enabled, config)
                 VALUES (?, ?, ?, ?, ?)
                 ON CONFLICT(channel_id) DO UPDATE SET
                     name = excluded.name,
                     protocol = excluded.protocol,
                     enabled = excluded.enabled,
                     config = excluded.config,
                     updated_at = CURRENT_TIMESTAMP",
            )
            .bind(channel_id)
            .bind(&name)
            .bind(&protocol)
            .bind(enabled)
            .bind(&config)
            .execute(&mut **tx)
            .await?;

            count += 1;
        }

        Ok(count)
    }

    /// Insert channel-specific points from CSV files
    async fn insert_channel_specific_points(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        config_dir: &Path,
        errors: &mut Vec<SyncError>,
    ) -> Result<usize> {
        use aether_config::io::{AdjustmentPoint, ControlPoint, SignalPoint, TelemetryPoint};

        let mut total_count = 0;

        // Iterate over every channel directory.
        for entry in std::fs::read_dir(config_dir)? {
            let entry = entry?;
            let path = entry.path();

            // Only process directories with numeric names (channel IDs).
            if !path.is_dir() {
                continue;
            }

            let dir_name = match path.file_name().and_then(|n| n.to_str()) {
                Some(name) if name.chars().all(|c| c.is_numeric()) => name,
                _ => continue,
            };

            let channel_id = match dir_name.parse::<i32>() {
                Ok(id) => id,
                Err(_) => continue,
            };

            // Query protocol for this channel (needed for normalization)
            let protocol: String =
                sqlx::query_scalar("SELECT protocol FROM channels WHERE channel_id = ?")
                    .bind(channel_id)
                    .fetch_one(&mut **tx)
                    .await
                    .unwrap_or_else(|_| "modbus_tcp".to_string()); // Default fallback

            // Load point definitions and mappings for each type (T/S/C/A)
            // Telemetry points
            total_count += sync_point_type::<TelemetryPoint, _>(
                tx,
                &path,
                channel_id,
                &protocol,
                &PointSyncConfig {
                    csv_filename: "telemetry.csv",
                    mapping_filename: "mapping/telemetry_mapping.csv",
                    table_name: "telemetry_points",
                    type_label: "telemetry",
                },
                |p| PointFields {
                    point_id: p.base.point_id,
                    signal_name: p.base.signal_name.clone(),
                    unit: p.base.unit.clone(),
                    description: p.base.description.clone(),
                    scale: p.scale,
                    offset: p.offset,
                    reverse: p.reverse,
                    data_type: p.data_type.clone(),
                },
                errors,
            )
            .await?;

            // Signal points (with defaults for scale/offset/data_type)
            total_count += sync_point_type::<SignalPoint, _>(
                tx,
                &path,
                channel_id,
                &protocol,
                &PointSyncConfig {
                    csv_filename: "signal.csv",
                    mapping_filename: "mapping/signal_mapping.csv",
                    table_name: "signal_points",
                    type_label: "signal",
                },
                |p| PointFields {
                    point_id: p.base.point_id,
                    signal_name: p.base.signal_name.clone(),
                    unit: p.base.unit.clone(),
                    description: p.base.description.clone(),
                    scale: 1.0,
                    offset: 0.0,
                    reverse: p.reverse,
                    data_type: "int".to_string(),
                },
                errors,
            )
            .await?;

            // Control points (with defaults)
            total_count += sync_point_type::<ControlPoint, _>(
                tx,
                &path,
                channel_id,
                &protocol,
                &PointSyncConfig {
                    csv_filename: "control.csv",
                    mapping_filename: "mapping/control_mapping.csv",
                    table_name: "control_points",
                    type_label: "control",
                },
                |p| PointFields {
                    point_id: p.base.point_id,
                    signal_name: p.base.signal_name.clone(),
                    unit: p.base.unit.clone(),
                    description: p.base.description.clone(),
                    scale: 1.0,
                    offset: 0.0,
                    reverse: false,
                    data_type: "bool".to_string(),
                },
                errors,
            )
            .await?;

            // Adjustment points (with default reverse)
            total_count += sync_point_type::<AdjustmentPoint, _>(
                tx,
                &path,
                channel_id,
                &protocol,
                &PointSyncConfig {
                    csv_filename: "adjustment.csv",
                    mapping_filename: "mapping/adjustment_mapping.csv",
                    table_name: "adjustment_points",
                    type_label: "adjustment",
                },
                |p| PointFields {
                    point_id: p.base.point_id,
                    signal_name: p.base.signal_name.clone(),
                    unit: p.base.unit.clone(),
                    description: p.base.description.clone(),
                    scale: p.scale,
                    offset: p.offset,
                    reverse: false,
                    data_type: p.data_type.clone(),
                },
                errors,
            )
            .await?;
        }

        Ok(total_count)
    }

    /// Sync instances and their mappings
    async fn sync_instances(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        instances_path: &Path,
        config_dir: &Path,
        errors: &mut Vec<SyncError>,
    ) -> Result<usize> {
        let mut count = 0;

        let yaml_content = std::fs::read_to_string(instances_path)?;
        let instances_data: JsonValue = serde_yml::from_str(&yaml_content)?;

        // Support both array format (recommended) and legacy object format
        if let Some(instances_array) = instances_data.get("instances").and_then(|v| v.as_array()) {
            // Array format: instances: [{instance_id: 1, instance_name: "x", product_name: "y", ...}]
            for instance_data in instances_array {
                // Parse and validate instance_id (required, must be > 0)
                let instance_id = match instance_data.get("instance_id").and_then(|v| v.as_u64()) {
                    Some(id) if id > 0 => id as u32,
                    _ => {
                        errors.push(SyncError {
                            item: "Instance definition".to_string(),
                            error: format!(
                                "Invalid or missing instance_id: {:?}",
                                instance_data.get("instance_id")
                            ),
                        });
                        continue;
                    },
                };

                let instance_name = instance_data
                    .get("instance_name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");

                let product_name = instance_data
                    .get("product_name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");

                // Validate required fields
                if instance_name.is_empty() {
                    errors.push(SyncError {
                        item: format!("Instance with id {}", instance_id),
                        error: "Missing instance_name".to_string(),
                    });
                    continue;
                }

                if product_name.is_empty() {
                    errors.push(SyncError {
                        item: format!("Instance: {}", instance_name),
                        error: "Missing product_name".to_string(),
                    });
                    continue;
                }

                // Parse optional parent_id for topology hierarchy
                let parent_id = instance_data
                    .get("parent_id")
                    .and_then(|v| v.as_u64())
                    .map(|v| v as u32);

                count += self
                    .process_single_instance(
                        tx,
                        instance_id,
                        instance_name,
                        product_name,
                        parent_id,
                        config_dir,
                        errors,
                    )
                    .await?;
            }
        } else if let Some(instances) = instances_data.get("instances").and_then(|v| v.as_object())
        {
            // Legacy object format: instances: {instance_name: {product_name: "x", ...}}
            for (instance_name, instance_data) in instances {
                let product_name = instance_data
                    .get("product_name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");

                // Generate a new instance_id for legacy format
                let instance_id = self.get_next_instance_id(tx).await?;

                count += self
                    .process_single_instance(
                        tx,
                        instance_id,
                        instance_name,
                        product_name,
                        None,
                        config_dir,
                        errors,
                    )
                    .await?;
            }
        }

        Ok(count)
    }

    /// Process a single instance: load properties, insert into DB, load mappings
    async fn process_single_instance(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        instance_id: u32,
        instance_name: &str,
        product_name: &str,
        parent_id: Option<u32>,
        config_dir: &Path,
        errors: &mut Vec<SyncError>,
    ) -> Result<usize> {
        let mut count = 0;

        // Load properties from instance directory CSV
        let instance_dir = config_dir.join("instances").join(instance_name);
        debug!(
            "Instance: {}, dir exists: {}",
            instance_name,
            instance_dir.exists()
        );
        let properties = if instance_dir.exists() {
            self.load_instance_properties(&instance_dir)
                .with_context(|| {
                    format!("Failed to load properties for instance {instance_name}")
                })?
        } else {
            debug!("Instance directory does not exist: {:?}", instance_dir);
            "{}".to_string()
        };

        // Since schema v5 the `instances` table no longer has a `properties` column.
        // Properties are stored in the `instance_properties` table keyed by property_id.
        if let Err(e) = sqlx::query(
            "INSERT INTO instances (instance_id, instance_name, product_name, parent_id)
             VALUES (?, ?, ?, ?)
             ON CONFLICT(instance_id) DO UPDATE SET
                 instance_name = excluded.instance_name,
                 product_name = excluded.product_name,
                 parent_id = excluded.parent_id,
                 updated_at = CURRENT_TIMESTAMP",
        )
        .bind(instance_id)
        .bind(instance_name)
        .bind(product_name)
        .bind(parent_id.map(|id| id as i64))
        .execute(&mut **tx)
        .await
        {
            errors.push(SyncError {
                item: format!("Instance: {}", instance_name),
                error: e.to_string(),
            });
            return Ok(0);
        }

        count += 1;

        // Write property values into `instance_properties` (schema v5+).
        // The CSV uses integer `point_index` as property_id. Insert each value
        // as a JSON scalar; skip entries that cannot be parsed as integer ids.
        let properties_map =
            serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(&properties)
                .with_context(|| format!("Invalid properties for instance {instance_name}"))?;
        for (key, value) in &properties_map {
            if let Ok(property_id) = key.parse::<i64>() {
                let value_json = serde_json::to_string(value).with_context(|| {
                    format!("Failed to serialize property {key} for instance {instance_name}")
                })?;
                sqlx::query(
                    "INSERT INTO instance_properties (instance_id, property_id, value_json)
                     VALUES (?, ?, ?)
                     ON CONFLICT(instance_id, property_id) DO UPDATE SET
                         value_json = excluded.value_json,
                         updated_at = CURRENT_TIMESTAMP",
                )
                .bind(instance_id as i64)
                .bind(property_id)
                .bind(&value_json)
                .execute(&mut **tx)
                .await
                .with_context(|| {
                    format!("Failed to write property {key} for instance {instance_name}")
                })?;
            }
        }

        // Load instance mappings
        if instance_dir.exists() {
            let mappings_csv = instance_dir.join("channel_routing.csv");
            if mappings_csv.exists() {
                count += self
                    .insert_instance_mappings(tx, instance_name, &mappings_csv, errors)
                    .await?;
            }
        }

        Ok(count)
    }

    /// Get next available instance_id
    async fn get_next_instance_id(&self, tx: &mut Transaction<'_, Sqlite>) -> Result<u32> {
        let max_id: Option<u32> = sqlx::query_scalar("SELECT MAX(instance_id) FROM instances")
            .fetch_optional(&mut **tx)
            .await?;

        Ok(max_id.unwrap_or(0) + 1)
    }

    /// Load instance properties from properties.csv
    /// Format: point_index,value
    /// Returns JSON string: {"1": "500.0", "2": "380.0", ...}
    fn load_instance_properties(&self, instance_dir: &Path) -> Result<String> {
        let properties_path = instance_dir.join("properties.csv");

        if !properties_path.exists() {
            return Ok("{}".to_string());
        }

        let properties_csv = load_csv(&properties_path)?;

        let mut properties_map: serde_json::Map<String, serde_json::Value> = serde_json::Map::new();

        for row in properties_csv.iter() {
            if let (Some(point_index), Some(value)) = (row.get("point_index"), row.get("value")) {
                properties_map.insert(
                    point_index.clone(),
                    serde_json::Value::String(value.clone()),
                );
            }
        }

        let properties_json = serde_json::Value::Object(properties_map);
        let json_string = serde_json::to_string(&properties_json)?;
        Ok(json_string)
    }

    /// Insert instance mappings
    async fn insert_instance_mappings(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        instance_name: &str,
        mappings_path: &Path,
        errors: &mut Vec<SyncError>,
    ) -> Result<usize> {
        let mappings = match load_csv(mappings_path) {
            Ok(m) => m,
            Err(e) => {
                errors.push(SyncError {
                    item: format!("CSV file: {}", mappings_path.display()),
                    error: e.to_string(),
                });
                return Ok(0);
            },
        };

        let mut success_count = 0;
        for mapping in mappings.iter() {
            // Parse required positive integer fields
            let parse_id = |field: &str| -> Option<i32> {
                mapping
                    .get(field)
                    .and_then(|v| v.parse::<i32>().ok())
                    .filter(|&id| id > 0)
            };

            let Some(channel_id) = parse_id("channel_id") else {
                errors.push(SyncError {
                    item: format!("Routing for {}", instance_name),
                    error: format!(
                        "Invalid or missing channel_id: {:?}",
                        mapping.get("channel_id")
                    ),
                });
                continue;
            };

            let channel_type = mapping.get("channel_type").cloned().unwrap_or_default();
            if !["T", "S", "C", "A"].contains(&channel_type.as_str()) {
                errors.push(SyncError {
                    item: format!(
                        "Routing {}:{} for {}",
                        channel_id, channel_type, instance_name
                    ),
                    error: format!(
                        "Invalid channel_type '{}': must be T, S, C, or A",
                        channel_type
                    ),
                });
                continue;
            }

            let route_ctx = format!("{}:{} for {}", channel_id, channel_type, instance_name);

            let Some(channel_point_id) = parse_id("channel_point_id") else {
                errors.push(SyncError {
                    item: format!("Routing {}", route_ctx),
                    error: format!(
                        "Invalid or missing channel_point_id: {:?}",
                        mapping.get("channel_point_id")
                    ),
                });
                continue;
            };

            let instance_type = mapping.get("instance_type").cloned().unwrap_or_default();

            let Some(instance_point_id) = parse_id("instance_point_id") else {
                errors.push(SyncError {
                    item: format!("Routing {}", route_ctx),
                    error: format!(
                        "Invalid or missing instance_point_id: {:?}",
                        mapping.get("instance_point_id")
                    ),
                });
                continue;
            };

            // Measurement routes remain configuration-owned. Action routes are
            // commands and must pass through the governed application boundary.
            let insert_result = match instance_type.as_str() {
                "M" => {
                    sqlx::query(
                        "INSERT INTO measurement_routing (instance_id, instance_name, channel_id, channel_type, channel_point_id, measurement_id)
                         VALUES ((SELECT instance_id FROM instances WHERE instance_name = ?), ?, ?, ?, ?, ?)
                         ON CONFLICT(instance_id, measurement_id) DO UPDATE SET
                             instance_name = excluded.instance_name,
                             channel_id = excluded.channel_id,
                             channel_type = excluded.channel_type,
                             channel_point_id = excluded.channel_point_id,
                             updated_at = CURRENT_TIMESTAMP"
                    )
                    .bind(instance_name).bind(instance_name)
                    .bind(channel_id).bind(&channel_type).bind(channel_point_id).bind(instance_point_id)
                    .execute(&mut **tx).await
                },
                "A" => {
                    errors.push(SyncError {
                        item: format!("Action routing {route_ctx}:{channel_point_id}"),
                        error: ACTION_ROUTING_SYNC_GUARD.to_owned(),
                    });
                    continue;
                },
                _ => Err(sqlx::Error::Configuration(
                    format!("Invalid instance_type: {}. Must be 'M' or 'A'", instance_type).into(),
                )),
            };

            if let Err(e) = insert_result {
                let kind = if instance_type == "M" {
                    "Measurement"
                } else {
                    "Action"
                };
                errors.push(SyncError {
                    item: format!("{} routing {}:{}", kind, route_ctx, channel_point_id),
                    error: e.to_string(),
                });
                continue;
            }

            success_count += 1;
        }

        Ok(success_count)
    }

    /// Sync rules from JSON/YAML files (vue-flow/node-red compatible)
    async fn sync_rules(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        rules_dir: &Path,
    ) -> Result<usize> {
        let mut count = 0;

        for entry in std::fs::read_dir(rules_dir)? {
            let entry = entry?;
            let path = entry.path();

            let extension = path.extension().and_then(|e| e.to_str());

            // Support both JSON and YAML formats
            let rule_data: JsonValue = match extension {
                Some("json") => {
                    let json_content = std::fs::read_to_string(&path)?;
                    serde_json::from_str(&json_content)?
                },
                Some("yaml") | Some("yml") => {
                    let yaml_content = std::fs::read_to_string(&path)?;
                    serde_yml::from_str(&yaml_content)?
                },
                _ => continue, // Skip non-JSON/YAML files
            };

            // id is auto-generated by SQLite (INTEGER PRIMARY KEY AUTOINCREMENT)
            let name = rule_data
                .get("name")
                .and_then(|value| value.as_str())
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .ok_or_else(|| {
                    anyhow::anyhow!("Rule file '{}': expected a non-empty name", path.display())
                })?;
            let description = rule_data
                .get("description")
                .and_then(|v| v.as_str())
                .map(String::from);
            let enabled = rule_data
                .get("enabled")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let priority = rule_data
                .get("priority")
                .and_then(|v| v.as_i64())
                .unwrap_or(0);

            // Store the complete flow_json (entire rule content for vue-flow/node-red)
            let flow_json = match rule_data.get("flow_json") {
                Some(v) => serde_json::to_string(v).map_err(|e| {
                    anyhow::anyhow!("Rule '{}': Failed to serialize flow_json: {}", name, e)
                })?,
                None => serde_json::to_string(&rule_data).map_err(|e| {
                    anyhow::anyhow!(
                        "Rule '{}': Failed to serialize rule_data as flow_json: {}",
                        name,
                        e
                    )
                })?,
            };

            // Parse flow_json → compact RuleFlow for execution engine
            let flow_value: JsonValue = serde_json::from_str(&flow_json).map_err(|e| {
                anyhow::anyhow!("Rule '{}': Failed to parse flow_json: {}", name, e)
            })?;
            // Both flow columns come from the single sanctioned producer so
            // flow_json/nodes_json can never diverge.
            let columns = aether_rules::flow_column_values(&flow_value).with_context(|| {
                format!(
                    "Rule '{}' ({}): invalid Vue Flow structure",
                    name,
                    path.file_name().unwrap_or_default().to_string_lossy()
                )
            })?;

            let existing_rule_id: Option<i64> =
                sqlx::query_scalar("SELECT id FROM rules WHERE name = ? ORDER BY id LIMIT 1")
                    .bind(name)
                    .fetch_optional(&mut **tx)
                    .await?;

            if let Some(rule_id) = existing_rule_id {
                sqlx::query(
                    "UPDATE rules SET
                         description = ?,
                         flow_json = ?,
                         nodes_json = ?,
                         enabled = ?,
                         priority = ?,
                         updated_at = CURRENT_TIMESTAMP
                     WHERE id = ?",
                )
                .bind(description)
                .bind(&columns.flow_json)
                .bind(&columns.nodes_json)
                .bind(enabled)
                .bind(priority)
                .bind(rule_id)
                .execute(&mut **tx)
                .await?;
            } else {
                sqlx::query(
                    "INSERT INTO rules (name, description, flow_json, nodes_json, enabled, priority)
                     VALUES (?, ?, ?, ?, ?, ?)",
                )
                .bind(name)
                .bind(description)
                .bind(&columns.flow_json)
                .bind(&columns.nodes_json)
                .bind(enabled)
                .bind(priority)
                .execute(&mut **tx)
                .await?;
            }

            count += 1;
        }

        Ok(count)
    }

    /// Update sync timestamp in sync_metadata table
    async fn update_sync_timestamp(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        service_name: &str,
    ) -> Result<()> {
        let timestamp = sqlx::types::chrono::Utc::now()
            .format("%Y-%m-%d %H:%M:%S")
            .to_string();

        sqlx::query(
            "INSERT INTO sync_metadata (service, last_sync) VALUES (?, ?)
             ON CONFLICT(service) DO UPDATE SET last_sync = excluded.last_sync",
        )
        .bind(service_name)
        .bind(&timestamp)
        .execute(&mut **tx)
        .await?;

        Ok(())
    }
}

#[cfg(test)]
#[allow(clippy::disallowed_methods)]
mod atomic_sync_tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn write_site_config_with_managed_entities(workspace: &TempDir) -> (PathBuf, PathBuf) {
        let config_path = workspace.path().join("config");
        let database_path = workspace.path().join("data");
        fs::create_dir_all(config_path.join("io")).unwrap();
        fs::create_dir_all(config_path.join("automation/rules")).unwrap();

        fs::write(config_path.join("global.yaml"), "site_name: commissioned\n").unwrap();
        fs::write(
            config_path.join("io/io.yaml"),
            r#"
channels:
  - id: 1
    name: configured-channel
    protocol: virtual
    enabled: false
"#,
        )
        .unwrap();
        fs::write(
            config_path.join("automation/automation.yaml"),
            "auto_load_instances: false\n",
        )
        .unwrap();
        fs::write(
            config_path.join("automation/instances.yaml"),
            r#"
instances:
  - instance_id: 1
    instance_name: configured-device
    product_name: ExampleDevice
"#,
        )
        .unwrap();
        write_managed_rule(&config_path, "first revision");

        (config_path, database_path)
    }

    fn write_managed_rule(config_path: &Path, description: &str) {
        let rule = serde_json::json!({
            "id": "configured-rule",
            "name": "Configured rule",
            "description": description,
            "enabled": false,
            "priority": 7,
            "flow_json": {
                "nodes": [
                    {
                        "id": "start",
                        "type": "start",
                        "position": { "x": 0, "y": 0 },
                        "data": { "config": { "wires": { "default": ["end"] } } }
                    },
                    {
                        "id": "end",
                        "type": "end",
                        "position": { "x": 100, "y": 0 }
                    }
                ],
                "edges": []
            }
        });
        fs::write(
            config_path.join("automation/rules/configured-rule.json"),
            serde_json::to_vec_pretty(&rule).unwrap(),
        )
        .unwrap();
    }

    fn write_instance_routing(config_path: &Path, rows: &str) {
        let instance_directory = config_path.join("automation/instances/configured-device");
        fs::create_dir_all(&instance_directory).unwrap();
        fs::write(
            instance_directory.join("channel_routing.csv"),
            format!(
                "channel_id,channel_type,channel_point_id,instance_type,instance_point_id\n{rows}"
            ),
        )
        .unwrap();
    }

    async fn connect_to_database(database_file: &Path) -> sqlx::SqlitePool {
        sqlx::sqlite::SqlitePoolOptions::new()
            .connect_with(common::bootstrap_database::sqlite_connect_options(
                database_file.to_str().unwrap(),
            ))
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn generic_sync_preserves_distribution_product_identity_verbatim() {
        let workspace = TempDir::new().unwrap();
        let (config_path, database_path) = write_site_config_with_managed_entities(&workspace);
        let instances_path = config_path.join("automation/instances.yaml");
        let instances = fs::read_to_string(&instances_path)
            .unwrap()
            .replace("ExampleDevice", "distribution_alias");
        fs::write(instances_path, instances).unwrap();

        ConfigSyncer::new(&config_path, &database_path)
            .sync_all()
            .await
            .unwrap();

        let pool = connect_to_database(&database_path.join("aether.db")).await;
        let product_name: String =
            sqlx::query_scalar("SELECT product_name FROM instances WHERE instance_id = 1")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(product_name, "distribution_alias");
    }

    #[tokio::test]
    async fn measurement_routing_remains_supported_by_configuration_sync() {
        let workspace = TempDir::new().unwrap();
        let (config_path, database_path) = write_site_config_with_managed_entities(&workspace);
        write_instance_routing(&config_path, "1,T,77,M,88\n");

        ConfigSyncer::new(&config_path, &database_path)
            .sync_all()
            .await
            .unwrap();

        let pool = connect_to_database(&database_path.join("aether.db")).await;
        let route: (i64, String, i64) = sqlx::query_as(
            "SELECT channel_id, channel_type, measurement_id FROM measurement_routing \
             WHERE instance_id = 1",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(route, (1, "T".to_owned(), 88));
    }

    #[tokio::test]
    async fn action_routing_in_configuration_fails_closed_and_rolls_back_measurements() {
        let workspace = TempDir::new().unwrap();
        let (config_path, database_path) = write_site_config_with_managed_entities(&workspace);
        write_instance_routing(&config_path, "1,T,77,M,88\n1,C,9,A,10\n");

        let error = ConfigSyncer::new(&config_path, &database_path)
            .sync_all()
            .await
            .unwrap_err();
        assert!(
            format!("{error:#}").contains("governed action-routing command"),
            "unexpected error: {error:#}"
        );

        let pool = connect_to_database(&database_path.join("aether.db")).await;
        let measurement_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM measurement_routing")
            .fetch_one(&pool)
            .await
            .unwrap();
        let action_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM action_routing")
            .fetch_one(&pool)
            .await
            .unwrap();
        let instance_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM instances")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(measurement_count, 0, "the site transaction must roll back");
        assert_eq!(action_count, 0);
        assert_eq!(instance_count, 0);
    }

    #[tokio::test]
    async fn force_sync_refuses_to_cascade_delete_existing_action_routing() {
        let workspace = TempDir::new().unwrap();
        let (config_path, database_path) = write_site_config_with_managed_entities(&workspace);
        ConfigSyncer::new(&config_path, &database_path)
            .sync_all()
            .await
            .unwrap();

        let database_file = database_path.join("aether.db");
        let pool = connect_to_database(&database_file).await;
        sqlx::query(
            "INSERT INTO control_points \
             (point_id, channel_id, signal_name, reverse, data_type) \
             VALUES (9, 1, 'start', 0, 'bool')",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO action_routing \
             (instance_id, instance_name, action_id, channel_id, channel_type, channel_point_id) \
             VALUES (1, 'configured-device', 10, 1, 'C', 9)",
        )
        .execute(&pool)
        .await
        .unwrap();
        pool.close().await;
        fs::write(config_path.join("global.yaml"), "site_name: replacement\n").unwrap();

        let error = ConfigSyncer::new(&config_path, &database_path)
            .with_force(true)
            .sync_all()
            .await
            .unwrap_err();
        assert!(
            format!("{error:#}").contains("governed action-routing command"),
            "unexpected error: {error:#}"
        );

        let pool = connect_to_database(&database_file).await;
        let action_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM action_routing")
            .fetch_one(&pool)
            .await
            .unwrap();
        let control_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM control_points")
            .fetch_one(&pool)
            .await
            .unwrap();
        let site_name: String = sqlx::query_scalar(
            "SELECT value FROM service_config \
             WHERE service_name = 'global' AND key = 'site_name'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(action_count, 1);
        assert_eq!(control_count, 1);
        assert_eq!(site_name, "commissioned");
    }

    #[tokio::test]
    async fn all_service_sync_rolls_back_earlier_writes_when_a_later_config_fails() {
        let workspace = TempDir::new().unwrap();
        let config_path = workspace.path().join("config");
        let database_path = workspace.path().join("data");
        fs::create_dir_all(config_path.join("io")).unwrap();
        fs::create_dir_all(config_path.join("automation")).unwrap();

        fs::write(config_path.join("global.yaml"), "site_name: replacement\n").unwrap();
        fs::write(
            config_path.join("io/io.yaml"),
            r#"
channels:
  - id: 1
    name: disabled-simulator
    protocol: virtual
    enabled: false
"#,
        )
        .unwrap();
        fs::write(
            config_path.join("automation/automation.yaml"),
            "auto_load_instances: [invalid\n",
        )
        .unwrap();

        let database_file = database_path.join("aether.db");
        schema::init_database(&database_file).await.unwrap();
        let pool =
            sqlx::SqlitePool::connect(&format!("sqlite:{}", database_file.to_string_lossy()))
                .await
                .unwrap();
        sqlx::query(
            "INSERT INTO service_config (service_name, key, value, type) VALUES (?, ?, ?, ?)",
        )
        .bind("global")
        .bind("site_name")
        .bind("original")
        .bind("string")
        .execute(&pool)
        .await
        .unwrap();
        pool.close().await;

        let syncer = ConfigSyncer::new(&config_path, &database_path);
        let error = syncer.sync_all().await.unwrap_err();
        let rendered_error = format!("{error:#}");
        assert!(
            rendered_error.contains("automation.yaml"),
            "unexpected error: {rendered_error}"
        );

        let pool =
            sqlx::SqlitePool::connect(&format!("sqlite:{}", database_file.to_string_lossy()))
                .await
                .unwrap();
        let global_value: String = sqlx::query_scalar(
            "SELECT value FROM service_config WHERE service_name = 'global' AND key = 'site_name'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        let channel_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM channels")
            .fetch_one(&pool)
            .await
            .unwrap();

        assert_eq!(global_value, "original");
        assert_eq!(channel_count, 0);
    }

    #[tokio::test]
    async fn all_service_sync_commits_every_domain_together() {
        let workspace = TempDir::new().unwrap();
        let config_path = workspace.path().join("config");
        let database_path = workspace.path().join("data");
        fs::create_dir_all(config_path.join("io")).unwrap();
        fs::create_dir_all(config_path.join("automation")).unwrap();

        fs::write(config_path.join("global.yaml"), "site_name: commissioned\n").unwrap();
        fs::write(
            config_path.join("io/io.yaml"),
            r#"
channels:
  - id: 1
    name: disabled-simulator
    protocol: virtual
    enabled: false
"#,
        )
        .unwrap();
        fs::write(
            config_path.join("automation/automation.yaml"),
            "auto_load_instances: false\n",
        )
        .unwrap();

        let syncer = ConfigSyncer::new(&config_path, &database_path);
        let results = syncer.sync_all().await.unwrap();
        assert_eq!(results.len(), 3);

        let database_file = database_path.join("aether.db");
        let pool =
            sqlx::SqlitePool::connect(&format!("sqlite:{}", database_file.to_string_lossy()))
                .await
                .unwrap();
        let global_value: String = sqlx::query_scalar(
            "SELECT value FROM service_config WHERE service_name = 'global' AND key = 'site_name'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        let channel_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM channels")
            .fetch_one(&pool)
            .await
            .unwrap();
        let automation_sync_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM sync_metadata WHERE service = 'automation'")
                .fetch_one(&pool)
                .await
                .unwrap();

        assert_eq!(global_value, "commissioned");
        assert_eq!(channel_count, 1);
        assert_eq!(automation_sync_count, 1);
    }

    #[tokio::test]
    async fn empty_site_guard_rejects_commissioned_rows_before_any_sync_write() {
        let workspace = TempDir::new().unwrap();
        let config_path = workspace.path().join("config");
        let database_path = workspace.path().join("data");
        fs::create_dir_all(config_path.join("io")).unwrap();
        fs::create_dir_all(config_path.join("automation")).unwrap();
        fs::write(config_path.join("global.yaml"), "api:\n  host: 127.0.0.1\n").unwrap();
        fs::write(config_path.join("io/io.yaml"), "channels: []\n").unwrap();
        fs::write(
            config_path.join("automation/automation.yaml"),
            "auto_load_instances: false\n",
        )
        .unwrap();
        fs::write(
            config_path.join("automation/instances.yaml"),
            "instances: []\n",
        )
        .unwrap();

        let database_file = database_path.join("aether.db");
        schema::init_database(&database_file).await.unwrap();
        let pool = connect_to_database(&database_file).await;
        sqlx::query(
            "INSERT INTO channels (channel_id, name, protocol, enabled, config) \
             VALUES (99, 'concurrent-channel', 'virtual', 1, '{}')",
        )
        .execute(&pool)
        .await
        .unwrap();
        pool.close().await;

        let error = ConfigSyncer::new(&config_path, &database_path)
            .requiring_empty_site()
            .sync_all()
            .await
            .unwrap_err();
        assert!(error.to_string().contains("channels contains 1 row"));

        let pool = connect_to_database(&database_file).await;
        let channel_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM channels")
            .fetch_one(&pool)
            .await
            .unwrap();
        let sync_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sync_metadata")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(channel_count, 1);
        assert_eq!(
            sync_count, 0,
            "guard failure must roll back all sync writes"
        );
    }

    #[tokio::test]
    async fn force_sync_clears_rules_when_the_rules_directory_is_absent() {
        let workspace = TempDir::new().unwrap();
        let (config_path, database_path) = write_site_config_with_managed_entities(&workspace);
        ConfigSyncer::new(&config_path, &database_path)
            .sync_all()
            .await
            .unwrap();

        fs::remove_dir_all(config_path.join("automation/rules")).unwrap();
        ConfigSyncer::new(&config_path, &database_path)
            .with_force(true)
            .sync_all()
            .await
            .unwrap();

        let pool = connect_to_database(&database_path.join("aether.db")).await;
        let rule_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM rules")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(rule_count, 0, "force sync must mirror an empty rule set");
    }

    #[tokio::test]
    async fn repeated_non_force_sync_preserves_api_owned_dependents_and_rule_identity() {
        let workspace = TempDir::new().unwrap();
        let (config_path, database_path) = write_site_config_with_managed_entities(&workspace);
        let syncer = ConfigSyncer::new(&config_path, &database_path);
        syncer.sync_all().await.unwrap();

        let database_file = database_path.join("aether.db");
        let pool = connect_to_database(&database_file).await;
        sqlx::query(
            "INSERT INTO channel_templates \
             (name, protocol, points_snapshot, mappings_snapshot, source_channel_id) \
             VALUES ('api-template', 'virtual', '[]', '[]', 1)",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO json_point_mappings \
             (channel_id, point_id, point_type, json_path) VALUES (1, 77, 'T', '$.value')",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO measurement_routing \
             (instance_id, instance_name, channel_id, channel_type, channel_point_id, measurement_id) \
             VALUES (1, 'configured-device', 1, 'T', 77, 88)",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO instance_properties (instance_id, property_id, value_json) \
             VALUES (1, 99, '\"api-owned\"')",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO channels (channel_id, name, protocol, enabled, config) \
             VALUES (99, 'api-channel', 'virtual', 0, '{}')",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO instances (instance_id, instance_name, product_name) \
             VALUES (99, 'api-device', 'ExampleDevice')",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO service_config (service_name, key, value, type) \
             VALUES ('aether-io', 'api.only', 'preserve', 'string')",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO rules (name, nodes_json, flow_json, enabled) \
             VALUES ('API-owned rule', '{}', '{}', 0)",
        )
        .execute(&pool)
        .await
        .unwrap();
        let configured_rule_id: i64 =
            sqlx::query_scalar("SELECT id FROM rules WHERE name = 'Configured rule'")
                .fetch_one(&pool)
                .await
                .unwrap();
        sqlx::query(
            "INSERT INTO rule_history (rule_id, triggered_at, execution_result) \
             VALUES (?, '2026-07-11T00:00:00Z', 'preserve')",
        )
        .bind(configured_rule_id)
        .execute(&pool)
        .await
        .unwrap();
        pool.close().await;

        write_managed_rule(&config_path, "second revision");
        syncer.sync_all().await.unwrap();

        let pool = connect_to_database(&database_file).await;
        let channel_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM channels")
            .fetch_one(&pool)
            .await
            .unwrap();
        let instance_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM instances")
            .fetch_one(&pool)
            .await
            .unwrap();
        let template_source: Option<i64> = sqlx::query_scalar(
            "SELECT source_channel_id FROM channel_templates WHERE name = 'api-template'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        let json_mapping_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM json_point_mappings WHERE channel_id = 1")
                .fetch_one(&pool)
                .await
                .unwrap();
        let routing_target: Option<i64> = sqlx::query_scalar(
            "SELECT channel_id FROM measurement_routing \
             WHERE instance_id = 1 AND measurement_id = 88",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        let property_value: String = sqlx::query_scalar(
            "SELECT value_json FROM instance_properties \
             WHERE instance_id = 1 AND property_id = 99",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        let api_config_value: String = sqlx::query_scalar(
            "SELECT value FROM service_config \
             WHERE service_name = 'aether-io' AND key = 'api.only'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        let rule_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM rules")
            .fetch_one(&pool)
            .await
            .unwrap();
        let (rule_id_after_sync, rule_description): (i64, String) =
            sqlx::query_as("SELECT id, description FROM rules WHERE name = 'Configured rule'")
                .fetch_one(&pool)
                .await
                .unwrap();
        let history_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM rule_history WHERE rule_id = ?")
                .bind(configured_rule_id)
                .fetch_one(&pool)
                .await
                .unwrap();

        assert_eq!(channel_count, 2, "API-created channel must be preserved");
        assert_eq!(instance_count, 2, "API-created instance must be preserved");
        assert_eq!(template_source, Some(1));
        assert_eq!(json_mapping_count, 1);
        assert_eq!(routing_target, Some(1));
        assert_eq!(property_value, "\"api-owned\"");
        assert_eq!(api_config_value, "preserve");
        assert_eq!(rule_count, 2, "repeated sync must not duplicate rules");
        assert_eq!(rule_id_after_sync, configured_rule_id);
        assert_eq!(rule_description, "second revision");
        assert_eq!(history_count, 1, "rule history must not be cascaded away");
    }

    #[tokio::test]
    async fn unreadable_instance_properties_roll_back_the_site_transaction() {
        let workspace = TempDir::new().unwrap();
        let (config_path, database_path) = write_site_config_with_managed_entities(&workspace);
        let properties_directory = config_path.join("automation/instances/configured-device");
        fs::create_dir_all(&properties_directory).unwrap();
        fs::write(properties_directory.join("properties.csv"), [0xff]).unwrap();

        let database_file = database_path.join("aether.db");
        schema::init_database(&database_file).await.unwrap();
        let pool = connect_to_database(&database_file).await;
        sqlx::query(
            "INSERT INTO service_config (service_name, key, value, type) \
             VALUES ('global', 'site_name', 'original', 'string')",
        )
        .execute(&pool)
        .await
        .unwrap();
        pool.close().await;

        let error = ConfigSyncer::new(&config_path, &database_path)
            .sync_all()
            .await
            .unwrap_err();
        assert!(format!("{error:#}").contains("properties.csv"));

        let pool = connect_to_database(&database_file).await;
        let site_name: String = sqlx::query_scalar(
            "SELECT value FROM service_config \
             WHERE service_name = 'global' AND key = 'site_name'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        let instance_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM instances")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(site_name, "original");
        assert_eq!(instance_count, 0);
    }

    #[tokio::test]
    async fn rejected_instance_property_insert_rolls_back_the_site_transaction() {
        let workspace = TempDir::new().unwrap();
        let (config_path, database_path) = write_site_config_with_managed_entities(&workspace);
        let properties_directory = config_path.join("automation/instances/configured-device");
        fs::create_dir_all(&properties_directory).unwrap();
        fs::write(
            properties_directory.join("properties.csv"),
            "point_index,value\n1,configured\n",
        )
        .unwrap();

        let database_file = database_path.join("aether.db");
        schema::init_database(&database_file).await.unwrap();
        let pool = connect_to_database(&database_file).await;
        sqlx::query(
            "INSERT INTO service_config (service_name, key, value, type) \
             VALUES ('global', 'site_name', 'original', 'string')",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "CREATE TRIGGER reject_instance_property_insert \
             BEFORE INSERT ON instance_properties \
             BEGIN SELECT RAISE(ABORT, 'property write rejected'); END",
        )
        .execute(&pool)
        .await
        .unwrap();
        pool.close().await;

        let error = ConfigSyncer::new(&config_path, &database_path)
            .sync_all()
            .await
            .unwrap_err();
        assert!(format!("{error:#}").contains("property write rejected"));

        let pool = connect_to_database(&database_file).await;
        let site_name: String = sqlx::query_scalar(
            "SELECT value FROM service_config \
             WHERE service_name = 'global' AND key = 'site_name'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        let instance_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM instances")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(site_name, "original");
        assert_eq!(instance_count, 0);
    }

    #[tokio::test]
    async fn structurally_invalid_rule_rolls_back_the_site_transaction() {
        let workspace = TempDir::new().unwrap();
        let (config_path, database_path) = write_site_config_with_managed_entities(&workspace);
        fs::write(
            config_path.join("automation/rules/configured-rule.json"),
            r#"{
                "name": "structurally-invalid",
                "enabled": true,
                "flow_json": {}
            }"#,
        )
        .unwrap();

        let database_file = database_path.join("aether.db");
        schema::init_database(&database_file).await.unwrap();
        let pool = connect_to_database(&database_file).await;
        sqlx::query(
            "INSERT INTO service_config (service_name, key, value, type) \
             VALUES ('global', 'site_name', 'original', 'string')",
        )
        .execute(&pool)
        .await
        .unwrap();
        pool.close().await;

        let error = ConfigSyncer::new(&config_path, &database_path)
            .sync_all()
            .await
            .unwrap_err();
        assert!(format!("{error:#}").contains("structurally-invalid"));

        let pool = connect_to_database(&database_file).await;
        let site_name: String = sqlx::query_scalar(
            "SELECT value FROM service_config \
             WHERE service_name = 'global' AND key = 'site_name'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(site_name, "original");
    }

    #[tokio::test]
    async fn rule_without_enabled_field_is_imported_disabled() {
        let workspace = TempDir::new().unwrap();
        let (config_path, database_path) = write_site_config_with_managed_entities(&workspace);
        let rule_path = config_path.join("automation/rules/configured-rule.json");
        let mut rule: JsonValue = serde_json::from_slice(&fs::read(&rule_path).unwrap()).unwrap();
        rule.as_object_mut().unwrap().remove("enabled");
        fs::write(&rule_path, serde_json::to_vec_pretty(&rule).unwrap()).unwrap();

        ConfigSyncer::new(&config_path, &database_path)
            .sync_all()
            .await
            .unwrap();

        let pool = connect_to_database(&database_path.join("aether.db")).await;
        let enabled: bool =
            sqlx::query_scalar("SELECT enabled FROM rules WHERE name = 'Configured rule'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert!(!enabled);
    }

    #[tokio::test]
    async fn rule_without_a_non_empty_name_rolls_back_with_file_context() {
        let workspace = TempDir::new().unwrap();
        let (config_path, database_path) = write_site_config_with_managed_entities(&workspace);
        let rule_path = config_path.join("automation/rules/configured-rule.json");
        let mut rule: JsonValue = serde_json::from_slice(&fs::read(&rule_path).unwrap()).unwrap();
        rule.as_object_mut()
            .unwrap()
            .insert("name".to_owned(), JsonValue::String("   ".to_owned()));
        fs::write(&rule_path, serde_json::to_vec_pretty(&rule).unwrap()).unwrap();

        let database_file = database_path.join("aether.db");
        schema::init_database(&database_file).await.unwrap();
        let pool = connect_to_database(&database_file).await;
        sqlx::query(
            "INSERT INTO service_config (service_name, key, value, type) \
             VALUES ('global', 'site_name', 'original', 'string')",
        )
        .execute(&pool)
        .await
        .unwrap();
        pool.close().await;

        let error = ConfigSyncer::new(&config_path, &database_path)
            .sync_all()
            .await
            .unwrap_err();
        let error = format!("{error:#}");
        assert!(error.contains("configured-rule.json"));
        assert!(error.contains("non-empty name"));

        let pool = connect_to_database(&database_file).await;
        let site_name: String = sqlx::query_scalar(
            "SELECT value FROM service_config \
             WHERE service_name = 'global' AND key = 'site_name'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(site_name, "original");
    }
}
