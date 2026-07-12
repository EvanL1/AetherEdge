//! Configuration export module
//!
//! This module provides functionality to export configuration from the SQLite
//! database back to YAML/CSV files.

use anyhow::{Context, Result};
use serde_yml;
use sqlx::{Row, SqlitePool};
use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::Arc;
use tracing::{debug, info};

// Cross-platform config schema (shared with io/automation via aether-config).
use aether_config::automation::{AutomationConfig, RuleConfig, RuleCore, RulesConfig};
use aether_config::io::{ChannelConfig, ChannelCore, IoConfig};
use aether_model::product_lib;

/// CSV column headers for point exports
const POINT_CSV_HEADERS: [&str; 8] = [
    "point_id",
    "signal_name",
    "scale",
    "offset",
    "unit",
    "reverse",
    "data_type",
    "description",
];

/// CSV column headers for instance mapping exports
const INSTANCE_MAPPING_CSV_HEADERS: [&str; 6] = [
    "channel_id",
    "channel_type",
    "channel_point_id",
    "instance_type",
    "instance_point_id",
    "description",
];

/// Result type for export operations
#[derive(Debug, Default)]
pub struct ExportResult {
    pub files_exported: Vec<String>,
    pub records_exported: usize,
}

/// Configuration exporter
pub struct ConfigExporter {
    pool: SqlitePool,
}

impl ConfigExporter {
    /// Create a new exporter
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// Export configuration for a specific service
    pub async fn export_service(
        &self,
        service: &str,
        output_dir: impl AsRef<Path>,
    ) -> Result<ExportResult> {
        info!("Export: {}", service);

        let output_dir = output_dir.as_ref();

        // Ensure output directory exists
        std::fs::create_dir_all(output_dir).context("Failed to create output directory")?;

        debug!("Exporting to directory: {:?}", output_dir);

        let result = match service {
            "global" => self.export_global(output_dir).await?,
            "aether-io" => self.export_io(output_dir).await?,
            "aether-automation" => self.export_automation(output_dir).await?,
            "rules" => self.export_rules(output_dir).await?,
            _ => {
                return Err(anyhow::anyhow!("Unknown service: {}", service));
            },
        };

        info!(
            "{}: {} files, {} records",
            service,
            result.files_exported.len(),
            result.records_exported
        );
        Ok(result)
    }

    async fn export_global(&self, output_dir: &Path) -> Result<ExportResult> {
        let mut result = ExportResult::default();

        let rows = sqlx::query(
            "SELECT key, value, type FROM service_config WHERE service_name = 'global' ORDER BY key",
        )
        .fetch_all(&self.pool)
        .await?;

        // Build nested structure from dot-separated keys
        let mut root = serde_yml::Mapping::new();
        for row in &rows {
            let key: String = row.try_get("key")?;
            let value: String = row.try_get("value")?;
            let type_hint: Option<String> = row.try_get("type").ok();

            let yaml_value = match type_hint.as_deref() {
                Some("integer") | Some("int") => value
                    .parse::<i64>()
                    .map(|n| serde_yml::Value::Number(n.into()))
                    .unwrap_or(serde_yml::Value::String(value)),
                Some("boolean") | Some("bool") => {
                    serde_yml::Value::Bool(value.eq_ignore_ascii_case("true") || value == "1")
                },
                Some("float") => value
                    .parse::<f64>()
                    .ok()
                    .map(|f| serde_yml::Value::Number(f.into()))
                    .unwrap_or(serde_yml::Value::String(value)),
                _ => {
                    if let Ok(n) = value.parse::<i64>() {
                        serde_yml::Value::Number(n.into())
                    } else if value.eq_ignore_ascii_case("true")
                        || value.eq_ignore_ascii_case("false")
                    {
                        serde_yml::Value::Bool(value.eq_ignore_ascii_case("true"))
                    } else {
                        serde_yml::Value::String(value)
                    }
                },
            };

            let parts: Vec<&str> = key.split('.').collect();
            insert_nested(&mut root, &parts, yaml_value);
        }

        let yaml_path = output_dir.join("global.yaml");
        let yaml_content = serde_yml::to_string(&root)?;
        std::fs::write(&yaml_path, yaml_content)?;
        result.files_exported.push("global.yaml".to_string());
        result.records_exported = rows.len();

        Ok(result)
    }

    async fn export_io(&self, output_dir: &Path) -> Result<ExportResult> {
        let mut result = ExportResult::default();

        // Export service configuration
        let mut service_config = self.export_io_config().await?;
        let yaml_path = output_dir.join("io.yaml");

        // Export channels
        let channels = self.export_channels().await?;
        result.records_exported += channels.len();
        // Wrap in Arc to match IoConfig.channels type, use iter + cloned to keep channels for later iteration
        service_config.channels = channels.iter().map(|c| Arc::new(c.clone())).collect();

        let yaml_content = serde_yml::to_string(&service_config)?;
        std::fs::write(&yaml_path, yaml_content)?;
        result.files_exported.push("io.yaml".to_string());

        // Export per-channel point CSVs and mapping CSVs
        for channel in &channels {
            let ch_id = channel.id();
            let channel_dir = output_dir.join(ch_id.to_string());
            std::fs::create_dir_all(&channel_dir)?;

            for point_type in ["telemetry", "signal", "control", "adjustment"] {
                // Export point definitions
                let points = self.export_points_by_channel(point_type, ch_id).await?;
                if !points.is_empty() {
                    let csv_path = channel_dir.join(format!("{}.csv", point_type));
                    self.write_csv(&csv_path, &POINT_CSV_HEADERS, &points)?;
                    result
                        .files_exported
                        .push(format!("{}/{}.csv", ch_id, point_type));
                    result.records_exported += points.len();
                }

                // Export protocol mappings
                let mappings = self
                    .export_channel_mappings_from_points(point_type, ch_id)
                    .await?;
                if !mappings.is_empty() {
                    let mapping_dir = channel_dir.join("mapping");
                    std::fs::create_dir_all(&mapping_dir)?;

                    // Build ordered headers: point_id first, then remaining keys sorted
                    let headers: Vec<String> = {
                        let mut extra_keys: Vec<String> = mappings[0]
                            .keys()
                            .filter(|k| k.as_str() != "point_id")
                            .cloned()
                            .collect();
                        extra_keys.sort();
                        let mut ordered = vec!["point_id".to_string()];
                        ordered.extend(extra_keys);
                        ordered
                    };
                    let header_refs: Vec<&str> = headers.iter().map(|s| s.as_str()).collect();

                    let csv_path = mapping_dir.join(format!("{}_mapping.csv", point_type));
                    self.write_csv(&csv_path, &header_refs, &mappings)?;
                    result
                        .files_exported
                        .push(format!("{}/mapping/{}_mapping.csv", ch_id, point_type));
                    result.records_exported += mappings.len();
                }
            }
        }

        // Export channel templates
        let templates = self.export_channel_templates().await?;
        if !templates.is_empty() {
            let templates_dir = output_dir.join("templates");
            std::fs::create_dir_all(&templates_dir)?;
            for (name, template_json) in &templates {
                let safe_name = name.replace(['/', '\\', ' '], "_");
                let json_path = templates_dir.join(format!("{}.json", safe_name));
                std::fs::write(&json_path, serde_json::to_string_pretty(template_json)?)?;
                result
                    .files_exported
                    .push(format!("templates/{}.json", safe_name));
                result.records_exported += 1;
            }
        }

        Ok(result)
    }

    async fn export_automation(&self, output_dir: &Path) -> Result<ExportResult> {
        let mut result = ExportResult::default();

        // Export service configuration
        let service_config = self.export_automation_config().await?;
        let yaml_path = output_dir.join("automation.yaml");
        let yaml_content = serde_yml::to_string(&service_config)?;
        std::fs::write(&yaml_path, yaml_content)?;
        result.files_exported.push("automation.yaml".to_string());

        // The kernel has no implicit product hierarchy to export.
        let products_hierarchy = self.export_products_hierarchy();
        if !products_hierarchy.is_empty() {
            let products_yaml = output_dir.join("products.yaml");
            let records_count = products_hierarchy.len();
            let mut root: BTreeMap<String, BTreeMap<String, Option<String>>> = BTreeMap::new();
            root.insert("products".to_string(), products_hierarchy);
            std::fs::write(&products_yaml, serde_yml::to_string(&root)?)?;
            result.files_exported.push("products.yaml".to_string());
            result.records_exported += records_count;
        }

        // Export instances
        let instances = self.export_instances().await?;
        if !instances.is_empty() {
            let instances_yaml = output_dir.join("instances.yaml");
            let instances_map: BTreeMap<String, serde_yml::Value> =
                BTreeMap::from_iter([("instances".to_string(), serde_yml::to_value(&instances)?)]);
            std::fs::write(&instances_yaml, serde_yml::to_string(&instances_map)?)?;
            result.files_exported.push("instances.yaml".to_string());
            result.records_exported += instances.len();
        }

        // Export instance mappings to CSV files
        for instance_name in instances.keys() {
            let mappings = self.export_instance_mappings(instance_name).await?;
            if !mappings.is_empty() {
                let instance_dir = output_dir.join(format!("instances/{}", instance_name));
                std::fs::create_dir_all(&instance_dir)?;

                let csv_path = instance_dir.join("channel_routing.csv");
                self.write_csv(&csv_path, &INSTANCE_MAPPING_CSV_HEADERS, &mappings)?;

                let relative_path = format!("instances/{}/channel_routing.csv", instance_name);
                result.files_exported.push(relative_path);
                result.records_exported += mappings.len();
            }
        }

        // Export rules as individual JSON files (matching sync input format)
        let rules_dir = output_dir.join("rules");
        let rules = self.export_rules_as_json(&rules_dir).await?;
        result.files_exported.extend(rules.files_exported);
        result.records_exported += rules.records_exported;

        Ok(result)
    }

    async fn export_rules(&self, output_dir: &Path) -> Result<ExportResult> {
        let mut result = ExportResult::default();

        // Export service configuration
        let service_config = self.export_rules_config().await?;
        let yaml_path = output_dir.join("rules.yaml");
        let yaml_content = serde_yml::to_string(&service_config)?;
        std::fs::write(&yaml_path, yaml_content)?;
        result.files_exported.push("rules.yaml".to_string());

        // Export rules list
        let rules_list = self.export_rules_list().await?;
        if !rules_list.is_empty() {
            let rules_count = rules_list.len(); // Get length before move
            let rules_yaml = output_dir.join("rules_list.yaml");
            let rules_map: BTreeMap<String, Vec<RuleConfig>> =
                BTreeMap::from_iter([("rules".to_string(), rules_list)]); // Move, not clone
            std::fs::write(&rules_yaml, serde_yml::to_string(&rules_map)?)?;
            result.files_exported.push("rules_list.yaml".to_string());
            result.records_exported += rules_count;
        }

        Ok(result)
    }

    // Helper methods for io export
    async fn export_io_config(&self) -> Result<IoConfig> {
        let mut config = IoConfig::default();

        // Query service configuration
        let rows = sqlx::query("SELECT key, value FROM service_config")
            .fetch_all(&self.pool)
            .await?;

        for row in rows {
            let key: String = row.try_get("key")?;
            let value: String = row.try_get("value")?;

            match key.as_str() {
                "service_name" => config.service.name = value,
                "api_host" => config.api.host = value,
                "service.port" | "api_port" | "port" => {
                    config.api.port = value.parse().unwrap_or(6000)
                },
                "log_level" => config.logging.level = value,
                "log_file_prefix" => config.logging.file_prefix = Some(value),
                _ => {},
            }
        }

        Ok(config)
    }

    async fn export_channels(&self) -> Result<Vec<ChannelConfig>> {
        let mut channels = Vec::new();

        let rows = sqlx::query("SELECT channel_id, name, protocol, enabled, config FROM channels")
            .fetch_all(&self.pool)
            .await?;

        for row in rows {
            let channel_id: u32 = row.try_get("channel_id")?;
            let name: String = row.try_get("name")?;
            let protocol: Option<String> = row.try_get("protocol")?;
            let enabled: bool = row.try_get("enabled")?;
            let config_str: Option<String> = row.try_get("config")?;

            let mut channel = ChannelConfig {
                core: ChannelCore {
                    id: channel_id,
                    name,
                    description: None,
                    protocol: protocol.unwrap_or_else(|| "virtual".to_string()),
                    enabled,
                },
                parameters: HashMap::new(),
                logging: Default::default(),
            };

            // Parse config JSON (consistent with sqlite_loader)
            if let Some(config_json) = config_str
                && let Ok(config_value) = serde_json::from_str::<serde_json::Value>(&config_json)
            {
                // Extract description
                channel.core.description = config_value
                    .get("description")
                    .and_then(|d| d.as_str())
                    .map(|s| s.to_string());

                // Extract parameters from nested "parameters" field
                if let Some(serde_json::Value::Object(params)) = config_value.get("parameters") {
                    channel.parameters =
                        params.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
                }

                // Extract logging config if present
                if let Some(logging_val) = config_value.get("logging")
                    && let Ok(logging) = serde_json::from_value(logging_val.clone())
                {
                    channel.logging = logging;
                }
            }

            channels.push(channel);
        }

        Ok(channels)
    }

    async fn export_points_by_channel(
        &self,
        point_type: &str,
        channel_id: u32,
    ) -> Result<Vec<HashMap<String, String>>> {
        let table = match point_type {
            "telemetry" => "telemetry_points",
            "signal" => "signal_points",
            "control" => "control_points",
            "adjustment" => "adjustment_points",
            _ => return Ok(Vec::new()),
        };

        let query = format!(
            "SELECT point_id, signal_name, scale, offset, unit, reverse, data_type, description
             FROM {} WHERE channel_id = ? ORDER BY point_id",
            table
        );

        let rows = sqlx::query(&query)
            .bind(channel_id as i64)
            .fetch_all(&self.pool)
            .await?;

        let mut points = Vec::new();
        for row in rows {
            let mut point = HashMap::new();
            point.insert(
                "point_id".to_string(),
                row.try_get::<i64, _>("point_id")?.to_string(),
            );
            point.insert("signal_name".to_string(), row.try_get("signal_name")?);

            if let Ok(scale) = row.try_get::<f64, _>("scale") {
                point.insert("scale".to_string(), scale.to_string());
            }
            if let Ok(offset) = row.try_get::<f64, _>("offset") {
                point.insert("offset".to_string(), offset.to_string());
            }
            if let Ok(Some(u)) = row.try_get::<Option<String>, _>("unit") {
                point.insert("unit".to_string(), u);
            }
            if let Ok(reverse) = row.try_get::<bool, _>("reverse") {
                point.insert("reverse".to_string(), reverse.to_string());
            }
            if let Ok(Some(dt)) = row.try_get::<Option<String>, _>("data_type") {
                point.insert("data_type".to_string(), dt);
            }
            if let Ok(Some(desc)) = row.try_get::<Option<String>, _>("description") {
                point.insert("description".to_string(), desc);
            }

            points.push(point);
        }

        Ok(points)
    }

    async fn export_channel_mappings_from_points(
        &self,
        point_type: &str,
        channel_id: u32,
    ) -> Result<Vec<HashMap<String, String>>> {
        let table = match point_type {
            "telemetry" => "telemetry_points",
            "signal" => "signal_points",
            "control" => "control_points",
            "adjustment" => "adjustment_points",
            _ => return Ok(Vec::new()),
        };

        let query = format!(
            "SELECT point_id, protocol_mappings FROM {} WHERE channel_id = ? AND protocol_mappings IS NOT NULL ORDER BY point_id",
            table
        );

        let rows = sqlx::query(&query)
            .bind(channel_id as i64)
            .fetch_all(&self.pool)
            .await?;

        let mut mappings = Vec::new();
        for row in rows {
            let point_id: i64 = row.try_get("point_id")?;
            let pm_str: String = row.try_get("protocol_mappings")?;

            if let Ok(serde_json::Value::Object(obj)) =
                serde_json::from_str::<serde_json::Value>(&pm_str)
            {
                let mut mapping = HashMap::new();
                mapping.insert("point_id".to_string(), point_id.to_string());

                for (k, v) in obj {
                    let val_str = match v {
                        serde_json::Value::Number(n) => n.to_string(),
                        serde_json::Value::String(s) => s,
                        serde_json::Value::Bool(b) => b.to_string(),
                        _ => continue,
                    };
                    mapping.insert(k, val_str);
                }

                mappings.push(mapping);
            }
        }

        Ok(mappings)
    }

    // Helper methods for automation export
    async fn export_automation_config(&self) -> Result<AutomationConfig> {
        let mut config = AutomationConfig::default();

        let rows = sqlx::query("SELECT key, value FROM service_config")
            .fetch_all(&self.pool)
            .await?;

        for row in rows {
            let key: String = row.try_get("key")?;
            let value: String = row.try_get("value")?;

            match key.as_str() {
                "service_name" => config.service.name = value,
                "api_host" => config.api.host = value,
                "service.port" | "api_port" | "port" => {
                    config.api.port = value
                        .parse()
                        .unwrap_or(aether_model::service_ports::IO_PORT)
                },
                _ => {},
            }
        }

        Ok(config)
    }

    /// Export the legacy kernel compatibility product hierarchy.
    ///
    /// The generic library is empty by default; active Pack assets are never
    /// compiled into this CLI compatibility surface.
    fn export_products_hierarchy(&self) -> BTreeMap<String, Option<String>> {
        product_lib::get_builtin_products()
            .iter()
            .map(|p| (p.name.clone(), p.parent_name.clone()))
            .collect()
    }

    async fn export_instances(
        &self,
    ) -> Result<BTreeMap<String, BTreeMap<String, serde_yml::Value>>> {
        let mut instances = BTreeMap::new();

        // Since schema v5 the `properties` column was removed from `instances`;
        // property values now live in `instance_properties(instance_id, property_id, value_json)`.
        let rows = sqlx::query(
            "SELECT instance_id, instance_name, product_name FROM instances ORDER BY instance_id",
        )
        .fetch_all(&self.pool)
        .await?;

        for row in rows {
            let instance_id: i64 = row.try_get("instance_id")?;
            let instance_name: String = row.try_get("instance_name")?;
            let product_name: String = row.try_get("product_name")?;

            let mut instance_data = BTreeMap::new();
            instance_data.insert(
                "product_name".to_string(),
                serde_yml::Value::String(product_name),
            );

            // Read property values from instance_properties table (keyed by integer property_id).
            let prop_rows = sqlx::query(
                "SELECT property_id, value_json FROM instance_properties WHERE instance_id = ?",
            )
            .bind(instance_id)
            .fetch_all(&self.pool)
            .await
            .unwrap_or_default();

            if !prop_rows.is_empty() {
                let mut props_map = serde_json::Map::new();
                for pr in prop_rows {
                    let pid: i64 = pr.try_get("property_id")?;
                    let val_json: String = pr.try_get("value_json")?;
                    if let Ok(val) = serde_json::from_str::<serde_json::Value>(&val_json) {
                        props_map.insert(pid.to_string(), val);
                    }
                }
                if let Ok(yaml_props) = serde_yml::to_value(serde_json::Value::Object(props_map)) {
                    instance_data.insert("properties".to_string(), yaml_props);
                }
            }

            instances.insert(instance_name, instance_data);
        }

        Ok(instances)
    }

    async fn export_instance_mappings(
        &self,
        instance_name: &str,
    ) -> Result<Vec<HashMap<String, String>>> {
        // Query measurement_routing table (T/S → M)
        let measurement_query = "SELECT mr.channel_id, mr.channel_type, mr.channel_point_id,
                                'M' as instance_type, mr.measurement_id as instance_point_id
                     FROM measurement_routing mr
                     JOIN instances i ON mr.instance_id = i.instance_id
                     WHERE i.instance_name = ?
                     ORDER BY mr.channel_id, mr.channel_type, mr.channel_point_id";

        let measurement_rows = sqlx::query(measurement_query)
            .bind(instance_name)
            .fetch_all(&self.pool)
            .await?;

        // Query action_routing table (A → C/A)
        let action_query = "SELECT ar.channel_id, ar.channel_type, ar.channel_point_id,
                           'A' as instance_type, ar.action_id as instance_point_id
                     FROM action_routing ar
                     JOIN instances i ON ar.instance_id = i.instance_id
                     WHERE i.instance_name = ?
                     ORDER BY ar.channel_id, ar.channel_type, ar.channel_point_id";

        let action_rows = sqlx::query(action_query)
            .bind(instance_name)
            .fetch_all(&self.pool)
            .await?;

        let mut mappings = Vec::new();

        // Process both measurement and action mappings
        let all_rows = measurement_rows
            .iter()
            .map(|r| ("M", r))
            .chain(action_rows.iter().map(|r| ("A", r)));

        for (instance_type, row) in all_rows {
            let mut mapping = HashMap::new();
            mapping.insert(
                "channel_id".to_string(),
                row.try_get::<i64, _>("channel_id")?.to_string(),
            );
            mapping.insert("channel_type".to_string(), row.try_get("channel_type")?);
            mapping.insert(
                "channel_point_id".to_string(),
                row.try_get::<i64, _>("channel_point_id")?.to_string(),
            );
            mapping.insert("instance_type".to_string(), instance_type.to_string());
            mapping.insert(
                "instance_point_id".to_string(),
                row.try_get::<i64, _>("instance_point_id")?.to_string(),
            );

            if let Ok(Some(desc)) = row.try_get::<Option<String>, _>("description") {
                mapping.insert("description".to_string(), desc);
            }

            mappings.push(mapping);
        }

        Ok(mappings)
    }

    // Helper methods for rules export
    async fn export_rules_config(&self) -> Result<RulesConfig> {
        let mut config = RulesConfig::default();

        let rows = sqlx::query("SELECT key, value FROM service_config")
            .fetch_all(&self.pool)
            .await?;

        for row in rows {
            let key: String = row.try_get("key")?;
            let value: String = row.try_get("value")?;

            match key.as_str() {
                "service_name" => config.service.name = value,
                "api_host" => config.api.host = value,
                "service.port" | "api_port" | "port" => {
                    config.api.port = value
                        .parse()
                        .unwrap_or(aether_model::service_ports::AUTOMATION_PORT)
                },
                // execution_interval and batch_size are deprecated
                "execution_interval" | "batch_size" => {},
                _ => {},
            }
        }

        Ok(config)
    }

    async fn export_rules_list(&self) -> Result<Vec<RuleConfig>> {
        let mut rules_list = Vec::new();

        let rows =
            sqlx::query("SELECT id, name, description, flow_json, enabled, priority FROM rules")
                .fetch_all(&self.pool)
                .await?;

        for row in rows {
            let id: i64 = row.try_get("id")?;
            let name: String = row.try_get("name")?;
            let description: Option<String> = row.try_get("description")?;
            let flow_json_str: String = row.try_get("flow_json")?;
            let enabled: bool = row.try_get("enabled")?;
            let priority: i64 = row.try_get("priority")?;

            // Parse flow_json string to serde_json::Value
            let flow_json = serde_json::from_str(&flow_json_str).unwrap_or(serde_json::Value::Null);

            let rule = RuleConfig {
                core: RuleCore {
                    id,
                    name,
                    description,
                    enabled,
                    priority: u32::try_from(priority).unwrap_or(0),
                },
                flow_json,
            };

            rules_list.push(rule);
        }

        Ok(rules_list)
    }

    async fn export_channel_templates(&self) -> Result<Vec<(String, serde_json::Value)>> {
        let rows = sqlx::query(
            "SELECT template_id, name, description, protocol, points_snapshot, mappings_snapshot, source_channel_id
             FROM channel_templates ORDER BY template_id",
        )
        .fetch_all(&self.pool)
        .await?;

        let mut templates = Vec::new();
        for row in rows {
            let name: String = row.try_get("name")?;
            let description: Option<String> = row.try_get("description")?;
            let protocol: String = row.try_get("protocol")?;
            let points_snapshot: String = row.try_get("points_snapshot")?;
            let mappings_snapshot: String = row.try_get("mappings_snapshot")?;
            let source_channel_id: Option<i64> = row.try_get("source_channel_id")?;

            let template = serde_json::json!({
                "name": name,
                "description": description,
                "protocol": protocol,
                "points_snapshot": serde_json::from_str::<serde_json::Value>(&points_snapshot)
                    .unwrap_or(serde_json::Value::Null),
                "mappings_snapshot": serde_json::from_str::<serde_json::Value>(&mappings_snapshot)
                    .unwrap_or(serde_json::Value::Null),
                "source_channel_id": source_channel_id,
            });

            templates.push((name, template));
        }

        Ok(templates)
    }

    async fn export_rules_as_json(&self, rules_dir: &Path) -> Result<ExportResult> {
        let mut result = ExportResult::default();

        let rows = sqlx::query(
            "SELECT id, name, description, flow_json, nodes_json, enabled, priority, cooldown_ms, trigger_config
             FROM rules ORDER BY id",
        )
        .fetch_all(&self.pool)
        .await?;

        if rows.is_empty() {
            return Ok(result);
        }

        std::fs::create_dir_all(rules_dir)?;

        for row in rows {
            let id: i64 = row.try_get("id")?;
            let name: String = row.try_get("name")?;
            let description: Option<String> = row.try_get("description")?;
            let flow_json_str: String = row.try_get("flow_json")?;
            let enabled: bool = row.try_get("enabled")?;
            let priority: i64 = row.try_get("priority")?;
            let cooldown_ms: i64 = row.try_get("cooldown_ms")?;

            let flow_json: serde_json::Value =
                serde_json::from_str(&flow_json_str).unwrap_or(serde_json::Value::Null);

            let rule_file = serde_json::json!({
                "name": name,
                "description": description.unwrap_or_default(),
                "enabled": enabled,
                "priority": priority,
                "cooldown_ms": cooldown_ms,
                "flow_json": flow_json,
                "format": "vue-flow",
                "id": id.to_string(),
            });

            let safe_name = if name.is_empty() {
                format!("rule_{}", id)
            } else {
                name.replace(['/', '\\', ' ', '.'], "_").to_lowercase()
            };
            let json_path = rules_dir.join(format!("{}.json", safe_name));
            std::fs::write(&json_path, serde_json::to_string_pretty(&rule_file)?)?;

            result
                .files_exported
                .push(format!("rules/{}.json", safe_name));
            result.records_exported += 1;
        }

        Ok(result)
    }

    /// Write records to a CSV file with the given column headers
    fn write_csv(
        &self,
        path: impl AsRef<Path>,
        headers: &[&str],
        rows: &[HashMap<String, String>],
    ) -> Result<()> {
        if rows.is_empty() {
            return Ok(());
        }
        let mut wtr = csv::Writer::from_path(path)?;
        wtr.write_record(headers)?;
        for row in rows {
            let record: Vec<&str> = headers
                .iter()
                .map(|h| row.get(*h).map(|s| s.as_str()).unwrap_or(""))
                .collect();
            wtr.write_record(&record)?;
        }
        wtr.flush()?;
        Ok(())
    }
}

/// Insert a value into a nested YAML mapping using a dot-separated key path.
fn insert_nested(root: &mut serde_yml::Mapping, parts: &[&str], value: serde_yml::Value) {
    if parts.len() == 1 {
        root.insert(serde_yml::Value::String(parts[0].to_string()), value);
        return;
    }

    let key = serde_yml::Value::String(parts[0].to_string());
    let entry = root
        .entry(key)
        .or_insert_with(|| serde_yml::Value::Mapping(serde_yml::Mapping::new()));

    if let serde_yml::Value::Mapping(nested) = entry {
        insert_nested(nested, &parts[1..], value);
    }
}
