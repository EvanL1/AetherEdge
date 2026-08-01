//! SQLite configuration loader for io
//!
//! Loads channel configurations, point tables, and mappings from SQLite database

use crate::core::channels::RuntimeChannelConfig;
#[cfg(test)]
use crate::core::config::{
    ADJUSTMENT_POINTS_TABLE, CHANNELS_TABLE, CONTROL_POINTS_TABLE, SERVICE_CONFIG_TABLE,
    SIGNAL_POINTS_TABLE, TELEMETRY_POINTS_TABLE, install_channel_revision_triggers,
};
use crate::core::config::{
    AdjustmentPoint, ApiConfig, ChannelConfig, ControlPoint, ServiceConfig, SignalPoint,
    TelemetryPoint,
};
use crate::core::config::{DEFAULT_PORT, Point};
use crate::error::{IoError, Result};
use aether_config::io::StoredChannelConfig;
use aether_ports::ChannelRevision;
use common::DEFAULT_API_HOST;
use common::sqlite::ServiceConfigLoader;
use futures::TryStreamExt;
use sqlx::sqlite::SqliteRow;
use sqlx::{Decode, QueryBuilder, Row, Sqlite, SqlitePool, Transaction, Type};
use std::collections::{BTreeMap, HashMap};
use tracing::info;

// Control point defaults.
// Only momentary controls are supported today; the schema/CSV carry no columns
// for these parameters, so every control point shares the same shape.
// If per-point override becomes a real requirement, add columns to
// control_points + CSV headers and read them in load_channel_points.
const DEFAULT_CONTROL_TYPE: &str = "momentary";
const DEFAULT_CONTROL_ON_VALUE: u16 = 1;
const DEFAULT_CONTROL_OFF_VALUE: u16 = 0;
const DEFAULT_CONTROL_PULSE_MS: u32 = 100;

struct PointTableCodec<T> {
    select_from: &'static str,
    table: &'static str,
    parse: fn(&SqliteRow) -> Result<(u32, T)>,
    insert: fn(&mut RuntimeChannelConfig, T),
}

/// Minimal post-activation authority index for one bulk reconciliation pass.
///
/// This deliberately carries no configuration or point DTOs. The full runtime
/// snapshot is loaded once before activation; these two revision maps only
/// prove whether that generation stayed authoritative while protocols were
/// being connected.
pub(crate) struct RuntimeAuthoritySnapshot {
    pub(crate) channels: BTreeMap<u32, (ChannelRevision, bool)>,
    pub(crate) tombstones: BTreeMap<u32, ChannelRevision>,
}

/// Io-specific SQLite configuration loader.
#[derive(Clone)]
pub struct IoSqliteLoader {
    base_loader: ServiceConfigLoader,
}

impl IoSqliteLoader {
    /// Create a loader from the process-wide SQLite pool.
    #[must_use]
    pub fn from_pool(pool: SqlitePool) -> Self {
        Self {
            base_loader: ServiceConfigLoader::from_pool(pool, "aether-io", DEFAULT_PORT),
        }
    }

    /// Load process-level service and API configuration.
    pub async fn load_service_config(&self) -> Result<(ServiceConfig, ApiConfig)> {
        // Load base service configuration
        let service_config =
            self.base_loader.load_config().await.map_err(|e| {
                IoError::ConfigError(format!("Failed to load service config: {}", e))
            })?;

        // Convert to io config
        let service = ServiceConfig {
            name: service_config.service_name.clone(),
            description: service_config
                .extra_config
                .get("description")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            version: service_config
                .extra_config
                .get("version")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
        };

        // Create API configuration
        let api = crate::core::config::ApiConfig {
            host: DEFAULT_API_HOST.to_string(),
            port: service_config.port,
        };

        Ok((service, api))
    }

    /// Load every channel row and its complete point topology from one read transaction.
    pub async fn load_runtime_channels(&self) -> Result<Vec<RuntimeChannelConfig>> {
        let mut transaction =
            self.base_loader.pool().begin().await.map_err(|e| {
                IoError::ConfigError(format!("Failed to begin channel snapshot: {e}"))
            })?;
        let mut channels = Vec::new();
        {
            let mut rows = sqlx::query(
                "SELECT channel_id, name, protocol, enabled, config, revision \
                 FROM channels ORDER BY channel_id",
            )
            .fetch(&mut *transaction);
            while let Some(row) = rows.try_next().await.map_err(|error| {
                IoError::ConfigError(format!("Failed to load channels snapshot: {error}"))
            })? {
                let (channel, revision) = Self::parse_channel_row(&row)?;
                channels.push(RuntimeChannelConfig::from_persisted(
                    channel,
                    ChannelRevision::new(revision),
                ));
            }
        }
        Self::load_runtime_channel_points(&mut transaction, &mut channels, None).await?;

        transaction
            .commit()
            .await
            .map_err(|e| IoError::ConfigError(format!("Failed to commit channel snapshot: {e}")))?;
        Ok(channels)
    }

    /// Load one complete channel snapshot and require the committed revision.
    pub async fn load_runtime_channel(
        &self,
        channel_id: u32,
        expected_revision: u64,
    ) -> Result<RuntimeChannelConfig> {
        let mut transaction =
            self.base_loader.pool().begin().await.map_err(|e| {
                IoError::ConfigError(format!("Failed to begin channel snapshot: {e}"))
            })?;
        let row = sqlx::query(
            "SELECT channel_id, name, protocol, enabled, config, revision \
             FROM channels WHERE channel_id = ?",
        )
        .bind(i64::from(channel_id))
        .fetch_optional(&mut *transaction)
        .await
        .map_err(|e| IoError::ConfigError(format!("Failed to load channel {channel_id}: {e}")))?
        .ok_or_else(|| IoError::ConfigError(format!("Channel {channel_id} does not exist")))?;
        let (channel, revision) = Self::parse_channel_row(&row)?;
        if revision != expected_revision {
            return Err(IoError::ConfigError(format!(
                "Channel {channel_id} revision changed while loading runtime snapshot"
            )));
        }

        let mut runtime_config =
            RuntimeChannelConfig::from_persisted(channel, ChannelRevision::new(revision));
        Self::load_runtime_channel_points(
            &mut transaction,
            std::slice::from_mut(&mut runtime_config),
            Some(channel_id),
        )
        .await?;
        transaction
            .commit()
            .await
            .map_err(|e| IoError::ConfigError(format!("Failed to commit channel snapshot: {e}")))?;
        Ok(runtime_config)
    }

    /// Load the fixed-size post-activation authority witness.
    ///
    /// The query count is constant with channel count: one `channels` scan and
    /// one tombstone scan in the same SQLite read transaction.
    pub(crate) async fn load_runtime_authority(&self) -> Result<RuntimeAuthoritySnapshot> {
        let mut transaction = self.base_loader.pool().begin().await.map_err(|error| {
            IoError::ConfigError(format!(
                "Failed to begin runtime authority snapshot: {error}"
            ))
        })?;
        let mut channels = BTreeMap::new();
        {
            let mut rows = sqlx::query(
                "SELECT channel_id, enabled, revision FROM channels ORDER BY channel_id",
            )
            .fetch(&mut *transaction);
            while let Some(row) = rows.try_next().await.map_err(|error| {
                IoError::ConfigError(format!("Failed to load runtime channel authority: {error}"))
            })? {
                let channel_id = Self::read_channel_id(&row, "channels")?;
                let enabled = Self::read_boolean(&row, "channels", "enabled", None)?;
                let revision = Self::read_revision(&row, "channels", "revision", channel_id)?;
                if channels
                    .insert(channel_id, (ChannelRevision::new(revision), enabled))
                    .is_some()
                {
                    return Err(IoError::ConfigError(format!(
                        "Duplicate channel {channel_id} in runtime authority snapshot"
                    )));
                }
            }
        }

        let mut tombstones = BTreeMap::new();
        {
            let mut rows = sqlx::query(
                "SELECT channel_id, last_revision \
                 FROM channel_revision_tombstones ORDER BY channel_id",
            )
            .fetch(&mut *transaction);
            while let Some(row) = rows.try_next().await.map_err(|error| {
                IoError::ConfigError(format!(
                    "Failed to load runtime channel tombstones: {error}"
                ))
            })? {
                let channel_id = Self::read_channel_id(&row, "channel_revision_tombstones")?;
                let revision = Self::read_revision(
                    &row,
                    "channel_revision_tombstones",
                    "last_revision",
                    channel_id,
                )?;
                if tombstones
                    .insert(channel_id, ChannelRevision::new(revision))
                    .is_some()
                {
                    return Err(IoError::ConfigError(format!(
                        "Duplicate channel {channel_id} in runtime tombstone snapshot"
                    )));
                }
            }
        }

        transaction.commit().await.map_err(|error| {
            IoError::ConfigError(format!(
                "Failed to commit runtime authority snapshot: {error}"
            ))
        })?;
        Ok(RuntimeAuthoritySnapshot {
            channels,
            tombstones,
        })
    }

    fn read_column<'row, T>(
        row: &'row SqliteRow,
        table: &'static str,
        column: &'static str,
    ) -> Result<T>
    where
        T: Decode<'row, Sqlite> + Type<Sqlite>,
    {
        row.try_get(column).map_err(|error| {
            IoError::ConfigError(format!(
                "Invalid persisted column {table}.{column}: {error}"
            ))
        })
    }

    fn read_boolean(
        row: &SqliteRow,
        table: &'static str,
        column: &'static str,
        null_default: Option<bool>,
    ) -> Result<bool> {
        let value: Option<i64> = Self::read_column(row, table, column)?;
        match value {
            Some(0) => Ok(false),
            Some(1) => Ok(true),
            None => null_default.ok_or_else(|| {
                IoError::ConfigError(format!(
                    "Invalid persisted column {table}.{column}: NULL is not allowed"
                ))
            }),
            Some(value) => Err(IoError::ConfigError(format!(
                "Invalid persisted column {table}.{column}: expected 0 or 1, got {value}"
            ))),
        }
    }

    fn read_optional_finite_f64(
        row: &SqliteRow,
        table: &'static str,
        column: &'static str,
    ) -> Result<Option<f64>> {
        let value: Option<f64> = Self::read_column(row, table, column)?;
        if value.is_some_and(|value| !value.is_finite()) {
            return Err(IoError::ConfigError(format!(
                "Invalid persisted column {table}.{column}: value must be finite"
            )));
        }
        Ok(value)
    }

    fn read_channel_id(row: &SqliteRow, table: &'static str) -> Result<u32> {
        let channel_id: i64 = Self::read_column(row, table, "channel_id")?;
        u32::try_from(channel_id).map_err(|_| {
            IoError::ConfigError(format!(
                "Invalid persisted column {table}.channel_id: {channel_id} is outside u32"
            ))
        })
    }

    fn read_revision(
        row: &SqliteRow,
        table: &'static str,
        column: &'static str,
        channel_id: u32,
    ) -> Result<u64> {
        let revision: i64 = Self::read_column(row, table, column)?;
        let revision = u64::try_from(revision).map_err(|_| {
            IoError::ConfigError(format!(
                "Invalid persisted column {table}.{column} for channel {channel_id}"
            ))
        })?;
        if revision == 0 {
            return Err(IoError::ConfigError(format!(
                "Invalid persisted column {table}.{column} for channel {channel_id}: \
                 revision must be positive"
            )));
        }
        Ok(revision)
    }

    fn parse_channel_row(row: &SqliteRow) -> Result<(ChannelConfig, u64)> {
        let channel_id = Self::read_channel_id(row, "channels")?;
        let name: String = Self::read_column(row, "channels", "name")?;
        let protocol: String = Self::read_column(row, "channels", "protocol")?;
        let enabled = Self::read_boolean(row, "channels", "enabled", None)?;
        let revision = Self::read_revision(row, "channels", "revision", channel_id)?;
        let config_json: Option<String> = Self::read_column(row, "channels", "config")?;
        let stored = StoredChannelConfig::decode(config_json.as_deref()).map_err(|error| {
            IoError::ConfigError(format!(
                "Invalid stored channel configuration for channel {channel_id}: {error}"
            ))
        })?;

        Ok((
            ChannelConfig {
                core: crate::core::config::ChannelCore {
                    id: channel_id,
                    name,
                    description: stored.description,
                    protocol,
                    enabled,
                },
                parameters: stored.parameters,
                logging: stored.logging,
            },
            revision,
        ))
    }

    fn parse_base_point(row: &SqliteRow, table: &'static str) -> Result<(u32, Point)> {
        let channel_id = Self::read_channel_id(row, table)?;
        let point_id: i64 = Self::read_column(row, table, "point_id")?;
        let point_id_u32 = u32::try_from(point_id).map_err(|_| {
            IoError::ConfigError(format!(
                "Invalid persisted column {table}.point_id: {point_id} is outside u32"
            ))
        })?;
        let signal_name: String = Self::read_column(row, table, "signal_name")?;
        let unit: Option<String> = Self::read_column(row, table, "unit")?;
        let description: Option<String> = Self::read_column(row, table, "description")?;
        let mapping: Option<String> = Self::read_column(row, table, "protocol_mappings")?;
        let protocol_mappings =
            Self::parse_protocol_mapping(mapping, table, channel_id, point_id_u32)?;

        Ok((
            channel_id,
            Point {
                point_id: point_id_u32,
                signal_name,
                description,
                unit: unit.filter(|unit| !unit.is_empty()),
                protocol_mappings,
            },
        ))
    }

    fn parse_protocol_mapping(
        mapping: Option<String>,
        table: &'static str,
        channel_id: u32,
        point_id: u32,
    ) -> Result<Option<String>> {
        let Some(mapping) = mapping else {
            return Ok(None);
        };
        let trimmed = mapping.trim();
        if trimmed.is_empty() {
            return Ok(None);
        }
        let value: serde_json::Value = serde_json::from_str(trimmed).map_err(|error| {
            IoError::ConfigError(format!(
                "Invalid persisted column {table}.protocol_mappings for channel {channel_id}, \
                 point {point_id}: {error}"
            ))
        })?;
        if value.is_null() || value.as_object().is_some_and(serde_json::Map::is_empty) {
            return Ok(None);
        }
        Ok(Some(mapping))
    }

    fn parse_telemetry_point(row: &SqliteRow) -> Result<(u32, TelemetryPoint)> {
        const TABLE: &str = "telemetry_points";
        let (channel_id, base) = Self::parse_base_point(row, TABLE)?;
        // Decode/type failures propagate above; only a legacy SQL NULL takes the schema default.
        let scale = Self::read_optional_finite_f64(row, TABLE, "scale")?.unwrap_or(1.0);
        let offset = Self::read_optional_finite_f64(row, TABLE, "offset")?.unwrap_or(0.0);
        let reverse = Self::read_boolean(row, TABLE, "reverse", Some(false))?;
        let data_type: Option<String> = Self::read_column(row, TABLE, "data_type")?;
        Ok((
            channel_id,
            TelemetryPoint {
                base,
                scale,
                offset,
                data_type: match data_type {
                    Some(value) => value,
                    None => "float32".to_string(),
                },
                reverse,
            },
        ))
    }

    fn parse_signal_point(row: &SqliteRow) -> Result<(u32, SignalPoint)> {
        const TABLE: &str = "signal_points";
        let (channel_id, base) = Self::parse_base_point(row, TABLE)?;
        let reverse = Self::read_boolean(row, TABLE, "reverse", Some(false))?;
        Ok((channel_id, SignalPoint { base, reverse }))
    }

    fn parse_control_point(row: &SqliteRow) -> Result<(u32, ControlPoint)> {
        const TABLE: &str = "control_points";
        let (channel_id, base) = Self::parse_base_point(row, TABLE)?;
        let reverse = Self::read_boolean(row, TABLE, "reverse", Some(false))?;
        Ok((
            channel_id,
            ControlPoint {
                base,
                reverse,
                control_type: DEFAULT_CONTROL_TYPE.to_string(),
                on_value: DEFAULT_CONTROL_ON_VALUE,
                off_value: DEFAULT_CONTROL_OFF_VALUE,
                pulse_duration_ms: Some(DEFAULT_CONTROL_PULSE_MS),
            },
        ))
    }

    fn parse_adjustment_point(row: &SqliteRow) -> Result<(u32, AdjustmentPoint)> {
        const TABLE: &str = "adjustment_points";
        let (channel_id, base) = Self::parse_base_point(row, TABLE)?;
        let scale = Self::read_optional_finite_f64(row, TABLE, "scale")?.unwrap_or(1.0);
        let offset = Self::read_optional_finite_f64(row, TABLE, "offset")?.unwrap_or(0.0);
        let data_type: Option<String> = Self::read_column(row, TABLE, "data_type")?;
        let min_value = Self::read_optional_finite_f64(row, TABLE, "min_value")?;
        let max_value = Self::read_optional_finite_f64(row, TABLE, "max_value")?;
        let step = Self::read_optional_finite_f64(row, TABLE, "step")?.unwrap_or(1.0);
        Ok((
            channel_id,
            AdjustmentPoint {
                base,
                min_value,
                max_value,
                step,
                data_type: match data_type {
                    Some(value) => value,
                    None => "float32".to_string(),
                },
                scale,
                offset,
            },
        ))
    }

    async fn load_point_table<T>(
        transaction: &mut Transaction<'_, Sqlite>,
        channels: &mut [RuntimeChannelConfig],
        channel_indices: &HashMap<u32, usize>,
        channel_filter: Option<u32>,
        codec: PointTableCodec<T>,
    ) -> Result<()> {
        let mut query = QueryBuilder::<Sqlite>::new(codec.select_from);
        if let Some(channel_id) = channel_filter {
            query
                .push(" WHERE channel_id = ")
                .push_bind(i64::from(channel_id));
        }
        query.push(" ORDER BY channel_id, point_id");
        let mut rows = query.build().fetch(&mut **transaction);
        while let Some(row) = rows.try_next().await.map_err(|error| {
            IoError::ConfigError(format!("Failed to load {} snapshot: {error}", codec.table))
        })? {
            let (channel_id, point) = (codec.parse)(&row)?;
            let channel =
                Self::runtime_channel_mut(channels, channel_indices, channel_id, codec.table)?;
            (codec.insert)(channel, point);
        }
        Ok(())
    }

    fn runtime_channel_mut<'channels>(
        channels: &'channels mut [RuntimeChannelConfig],
        channel_indices: &HashMap<u32, usize>,
        channel_id: u32,
        table: &'static str,
    ) -> Result<&'channels mut RuntimeChannelConfig> {
        let index = channel_indices.get(&channel_id).copied().ok_or_else(|| {
            IoError::ConfigError(format!(
                "Persisted {table} row references missing channel {channel_id}"
            ))
        })?;
        Ok(&mut channels[index])
    }

    /// Load complete point topology with four table scans on the current snapshot connection.
    async fn load_runtime_channel_points(
        transaction: &mut Transaction<'_, Sqlite>,
        channels: &mut [RuntimeChannelConfig],
        channel_filter: Option<u32>,
    ) -> Result<()> {
        let mut channel_indices = HashMap::with_capacity(channels.len());
        for (index, channel) in channels.iter_mut().enumerate() {
            channel.telemetry_points.clear();
            channel.signal_points.clear();
            channel.control_points.clear();
            channel.adjustment_points.clear();
            if channel_indices.insert(channel.id(), index).is_some() {
                return Err(IoError::ConfigError(format!(
                    "Duplicate channel {} in runtime snapshot",
                    channel.id()
                )));
            }
        }

        Self::load_point_table(
            transaction,
            channels,
            &channel_indices,
            channel_filter,
            PointTableCodec {
                select_from: "SELECT channel_id, point_id, signal_name, scale, offset, unit, \
                              reverse, data_type, description, protocol_mappings \
                              FROM telemetry_points",
                table: "telemetry_points",
                parse: Self::parse_telemetry_point,
                insert: |channel, point| channel.telemetry_points.push(point),
            },
        )
        .await?;

        Self::load_point_table(
            transaction,
            channels,
            &channel_indices,
            channel_filter,
            PointTableCodec {
                select_from: "SELECT channel_id, point_id, signal_name, unit, reverse, \
                              description, protocol_mappings FROM signal_points",
                table: "signal_points",
                parse: Self::parse_signal_point,
                insert: |channel, point| channel.signal_points.push(point),
            },
        )
        .await?;

        Self::load_point_table(
            transaction,
            channels,
            &channel_indices,
            channel_filter,
            PointTableCodec {
                select_from: "SELECT channel_id, point_id, signal_name, unit, reverse, \
                              description, protocol_mappings FROM control_points",
                table: "control_points",
                parse: Self::parse_control_point,
                insert: |channel, point| channel.control_points.push(point),
            },
        )
        .await?;

        Self::load_point_table(
            transaction,
            channels,
            &channel_indices,
            channel_filter,
            PointTableCodec {
                select_from: "SELECT channel_id, point_id, signal_name, scale, offset, unit, \
                              data_type, description, protocol_mappings, min_value, max_value, step \
                              FROM adjustment_points",
                table: "adjustment_points",
                parse: Self::parse_adjustment_point,
                insert: |channel, point| channel.adjustment_points.push(point),
            },
        )
        .await?;

        for channel in channels {
            info!(
                "Loaded {} points for channel {}: {} telemetry, {} signal, {} control, {} adjustment",
                channel.point_count(),
                channel.id(),
                channel.telemetry_points.len(),
                channel.signal_points.len(),
                channel.control_points.len(),
                channel.adjustment_points.len()
            );
        }

        Ok(())
    }
}

#[cfg(test)]
#[allow(clippy::disallowed_methods)] // Test code - unwrap is acceptable
mod tests {
    use super::*;
    use sqlx::SqlitePool;
    use tempfile::TempDir;

    /// Create a test database with basic schema and sample data
    async fn create_test_database() -> (TempDir, String) {
        let temp_dir = TempDir::new().unwrap();
        let db_path = temp_dir.path().join("test_aether.db");
        let db_url = format!("sqlite://{}?mode=rwc", db_path.display());

        let pool = SqlitePool::connect(&db_url).await.unwrap();

        // Create service_config table
        sqlx::query(SERVICE_CONFIG_TABLE)
            .execute(&pool)
            .await
            .unwrap();

        // Insert basic service config (with service_name column)
        sqlx::query(
            "INSERT INTO service_config (service_name, key, value) VALUES
                ('aether-io', 'service_name', 'aether-io'),
                ('aether-io', 'port', '6001'),
                ('aether-io', 'description', 'Test Service'),
                ('aether-io', 'version', '1.0.0')",
        )
        .execute(&pool)
        .await
        .unwrap();

        // Create channels table
        sqlx::query(CHANNELS_TABLE).execute(&pool).await.unwrap();
        install_channel_revision_triggers(&pool).await.unwrap();

        // Insert test channels
        sqlx::query(
            "INSERT INTO channels (channel_id, name, protocol, enabled, config) VALUES
                (1001, 'Test Modbus Channel', 'modbus_tcp', 1, '{\"parameters\":{\"host\":\"192.168.1.100\",\"port\":502}}'),
                (1002, 'Test Modbus Channel 2', 'modbus_tcp', 1, '{\"parameters\":{\"host\":\"127.0.0.1\",\"port\":502}}'),
                (1003, 'Test Disabled Channel', 'modbus_tcp', 0, '{}')",
        )
        .execute(&pool)
        .await
        .unwrap();

        // Create telemetry_points table
        sqlx::query(TELEMETRY_POINTS_TABLE)
            .execute(&pool)
            .await
            .unwrap();

        // Create signal_points table
        sqlx::query(SIGNAL_POINTS_TABLE)
            .execute(&pool)
            .await
            .unwrap();

        // Create control_points table
        sqlx::query(CONTROL_POINTS_TABLE)
            .execute(&pool)
            .await
            .unwrap();

        // Create adjustment_points table
        sqlx::query(ADJUSTMENT_POINTS_TABLE)
            .execute(&pool)
            .await
            .unwrap();

        // Insert test telemetry points (with protocol_mappings JSON)
        sqlx::query(
            "INSERT INTO telemetry_points (channel_id, point_id, signal_name, scale, offset, unit, reverse, data_type, description, protocol_mappings) VALUES
                (1001, 1, 'Temperature', 0.1, 0.0, '°C', 0, 'float32', 'Test temperature', '{\"slave_id\":1,\"function_code\":3,\"register_address\":100,\"data_type\":\"float32\",\"byte_order\":\"ABCD\"}'),
                (1002, 1, 'Modbus Point 1', 1.0, 0.0, '', 0, 'float32', 'Test Modbus point', '{\"slave_id\":1,\"function_code\":3,\"register_address\":1}')",
        )
        .execute(&pool)
        .await
        .unwrap();

        // Insert test signal points (with protocol_mappings JSON)
        sqlx::query(
            "INSERT INTO signal_points (channel_id, point_id, signal_name, unit, reverse, data_type, description, protocol_mappings) VALUES
                (1001, 2, 'Status', '', 0, 'uint16', 'Device status', '{\"slave_id\":1,\"function_code\":3,\"register_address\":102,\"data_type\":\"uint16\",\"byte_order\":\"ABCD\"}')",
        )
        .execute(&pool)
        .await
        .unwrap();

        // Insert test control points
        sqlx::query(
            "INSERT INTO control_points (channel_id, point_id, signal_name, unit, data_type, description) VALUES
                (1001, 3, 'Start', '', 'bool', 'Start control')",
        )
        .execute(&pool)
        .await
        .unwrap();

        // Insert test adjustment points
        sqlx::query(
            "INSERT INTO adjustment_points (channel_id, point_id, signal_name, scale, offset, unit, reverse, data_type, description, min_value, max_value, step) VALUES
                (1001, 4, 'Setpoint', 1.0, 0.0, '°C', 0, 'float32', 'Temperature setpoint', 10.0, 30.0, 0.5)",
        )
        .execute(&pool)
        .await
        .unwrap();

        // Update telemetry_points with protocol_mappings for Modbus channel
        sqlx::query(
            "UPDATE telemetry_points SET protocol_mappings = '{\"slave_id\":1,\"function_code\":3,\"register_address\":100,\"data_type\":\"float32\",\"byte_order\":\"ABCD\"}'
             WHERE channel_id = 1001 AND point_id = 1",
        )
        .execute(&pool)
        .await
        .unwrap();

        // Update signal_points with protocol_mappings for Modbus channel
        sqlx::query(
            "UPDATE signal_points SET protocol_mappings = '{\"slave_id\":1,\"function_code\":3,\"register_address\":102,\"data_type\":\"uint16\",\"byte_order\":\"ABCD\"}'
             WHERE channel_id = 1001 AND point_id = 2",
        )
        .execute(&pool)
        .await
        .unwrap();

        // Update telemetry_points with protocol_mappings for the second channel.
        sqlx::query(
            "UPDATE telemetry_points SET protocol_mappings = '{\"update_interval\":1000,\"initial_value\":25.0,\"noise_range\":2.0}'
             WHERE channel_id = 1002 AND point_id = 1",
        )
        .execute(&pool)
        .await
        .unwrap();

        pool.close().await;

        (temp_dir, db_path.to_string_lossy().to_string())
    }

    async fn test_loader(db_path: impl AsRef<std::path::Path>) -> IoSqliteLoader {
        let pool = SqlitePool::connect(&format!("sqlite://{}", db_path.as_ref().display()))
            .await
            .unwrap();
        IoSqliteLoader::from_pool(pool)
    }

    #[tokio::test]
    async fn test_load_complete_config() {
        let (_temp_dir, db_path) = create_test_database().await;
        let loader = test_loader(&db_path).await;

        let (service, api) = loader.load_service_config().await.unwrap();
        let channels = loader.load_runtime_channels().await.unwrap();
        assert_eq!(service.name, "aether-io");
        assert_eq!(api.port, 6001); // Default port (test uses wrong key 'port' instead of 'service.port')
        assert_eq!(channels.len(), 3, "Should load all 3 channels");
    }

    #[tokio::test]
    async fn test_load_channels() {
        let (_temp_dir, db_path) = create_test_database().await;
        let loader = test_loader(&db_path).await;

        let channels = loader.load_runtime_channels().await.unwrap();

        // Verify first channel (Modbus)
        let channel1 = &channels[0];
        assert_eq!(channel1.id(), 1001);
        assert_eq!(channel1.name(), "Test Modbus Channel");
        assert_eq!(channel1.protocol(), "modbus_tcp");
        assert!(channel1.channel_config().core.enabled);
        assert!(channel1.base.parameters.contains_key("host"));

        // Verify the second Modbus channel.
        let channel2 = &channels[1];
        assert_eq!(channel2.id(), 1002);
        assert_eq!(channel2.protocol(), "modbus_tcp");
        assert!(channel2.channel_config().core.enabled);

        // Verify third channel (Disabled)
        let channel3 = &channels[2];
        assert_eq!(channel3.id(), 1003);
        assert!(!channel3.channel_config().core.enabled);
    }

    #[tokio::test]
    async fn runtime_authority_batches_channel_revisions_and_tombstones() {
        let (_temp_dir, db_path) = create_test_database().await;
        let pool = SqlitePool::connect(&format!("sqlite://{db_path}"))
            .await
            .unwrap();
        sqlx::query("UPDATE channels SET enabled = 0 WHERE channel_id = 1002")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("DELETE FROM channels WHERE channel_id = 1003")
            .execute(&pool)
            .await
            .unwrap();

        let authority = IoSqliteLoader::from_pool(pool)
            .load_runtime_authority()
            .await
            .unwrap();

        assert_eq!(
            authority.channels,
            BTreeMap::from([
                (1001, (ChannelRevision::new(1), true)),
                (1002, (ChannelRevision::new(2), false)),
            ])
        );
        assert_eq!(
            authority.tombstones,
            BTreeMap::from([(1003, ChannelRevision::new(2))])
        );
    }

    #[tokio::test]
    async fn test_load_runtime_channel_points_modbus() {
        let (_temp_dir, db_path) = create_test_database().await;
        let loader = test_loader(&db_path).await;
        let runtime_config = loader
            .load_runtime_channels()
            .await
            .unwrap()
            .into_iter()
            .next()
            .unwrap();

        // Verify loaded points
        assert_eq!(runtime_config.telemetry_points.len(), 1);
        assert_eq!(runtime_config.signal_points.len(), 1);
        assert_eq!(runtime_config.control_points.len(), 1);
        assert_eq!(runtime_config.adjustment_points.len(), 1);

        // Verify protocol_mappings embedded in telemetry point
        let telem_point = &runtime_config.telemetry_points[0];
        assert!(telem_point.base.protocol_mappings.is_some());
        let mappings_json = telem_point.base.protocol_mappings.as_ref().unwrap();
        assert!(mappings_json.contains("register_address"));
        assert!(mappings_json.contains("100"));
    }

    #[tokio::test]
    async fn test_load_runtime_channel_requires_committed_revision() {
        let (_temp_dir, db_path) = create_test_database().await;
        let loader = test_loader(&db_path).await;

        let snapshot = loader.load_runtime_channel(1001, 1).await.unwrap();
        assert_eq!(snapshot.id(), 1001);
        assert_eq!(snapshot.telemetry_points.len(), 1);
        assert_eq!(snapshot.signal_points.len(), 1);
        assert_eq!(snapshot.control_points.len(), 1);
        assert_eq!(snapshot.adjustment_points.len(), 1);

        let error = loader
            .load_runtime_channel(1001, 2)
            .await
            .expect_err("stale revision must not produce a runtime snapshot");
        assert!(error.to_string().contains("revision changed"));
    }

    #[tokio::test]
    async fn batch_snapshot_groups_every_point_type_by_channel() {
        let (_temp_dir, db_path) = create_test_database().await;
        let pool = SqlitePool::connect(&format!("sqlite://{db_path}"))
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO signal_points \
             (channel_id, point_id, signal_name, reverse, data_type) \
             VALUES (1002, 12, 'Second status', 1, 'bool')",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO control_points \
             (channel_id, point_id, signal_name, reverse, data_type) \
             VALUES (1002, 13, 'Second control', 1, 'bool')",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO adjustment_points \
             (channel_id, point_id, signal_name, scale, offset, data_type, min_value, max_value, step) \
             VALUES (1002, 14, 'Second setpoint', 2.0, 3.0, 'float64', 1.0, 9.0, 0.25)",
        )
        .execute(&pool)
        .await
        .unwrap();
        pool.close().await;

        let snapshots = test_loader(&db_path)
            .await
            .load_runtime_channels()
            .await
            .unwrap();
        let first = snapshots
            .iter()
            .find(|snapshot| snapshot.id() == 1001)
            .unwrap();
        let second = snapshots
            .iter()
            .find(|snapshot| snapshot.id() == 1002)
            .unwrap();
        let empty = snapshots
            .iter()
            .find(|snapshot| snapshot.id() == 1003)
            .unwrap();

        assert_eq!(first.telemetry_points[0].base.signal_name, "Temperature");
        assert_eq!(first.signal_points[0].base.signal_name, "Status");
        assert_eq!(first.control_points[0].base.signal_name, "Start");
        assert_eq!(first.adjustment_points[0].base.signal_name, "Setpoint");

        assert_eq!(
            second.telemetry_points[0].base.signal_name,
            "Modbus Point 1"
        );
        assert_eq!(second.signal_points[0].base.point_id, 12);
        assert_eq!(second.control_points[0].base.point_id, 13);
        assert_eq!(second.adjustment_points[0].base.point_id, 14);
        assert_eq!(second.adjustment_points[0].step, 0.25);

        assert_eq!(empty.point_count(), 0);
    }

    #[tokio::test]
    async fn single_and_batch_runtime_snapshots_are_equivalent() {
        let (_temp_dir, db_path) = create_test_database().await;
        let loader = test_loader(&db_path).await;

        let batch = loader
            .load_runtime_channels()
            .await
            .unwrap()
            .into_iter()
            .find(|snapshot| snapshot.id() == 1001)
            .unwrap();
        let single = loader.load_runtime_channel(1001, 1).await.unwrap();

        let snapshot_json = |snapshot: &RuntimeChannelConfig| {
            serde_json::json!({
                "base": &snapshot.base,
                "telemetry": &snapshot.telemetry_points,
                "signal": &snapshot.signal_points,
                "control": &snapshot.control_points,
                "adjustment": &snapshot.adjustment_points,
            })
        };
        assert_eq!(snapshot_json(&single), snapshot_json(&batch));
    }

    #[tokio::test]
    async fn malformed_point_columns_and_mapping_json_fail_closed() {
        let (_temp_dir, db_path) = create_test_database().await;
        let pool = SqlitePool::connect(&format!("sqlite://{db_path}"))
            .await
            .unwrap();
        sqlx::query(
            "UPDATE telemetry_points SET scale = 'not-a-number' \
             WHERE channel_id = 1001 AND point_id = 1",
        )
        .execute(&pool)
        .await
        .unwrap();
        pool.close().await;

        let error = test_loader(&db_path)
            .await
            .load_runtime_channels()
            .await
            .expect_err("malformed persisted numeric columns must not use defaults");
        assert!(error.to_string().contains("telemetry_points.scale"));

        let pool = SqlitePool::connect(&format!("sqlite://{db_path}"))
            .await
            .unwrap();
        sqlx::query(
            "UPDATE telemetry_points SET scale = 1.0, protocol_mappings = '{bad json' \
             WHERE channel_id = 1001 AND point_id = 1",
        )
        .execute(&pool)
        .await
        .unwrap();
        pool.close().await;

        let error = test_loader(&db_path)
            .await
            .load_runtime_channels()
            .await
            .expect_err("malformed persisted mapping JSON must fail closed");
        assert!(
            error
                .to_string()
                .contains("telemetry_points.protocol_mappings")
        );
    }

    #[tokio::test]
    async fn nullable_legacy_rows_keep_their_documented_defaults() {
        let (_temp_dir, db_path) = create_test_database().await;
        let pool = SqlitePool::connect(&format!("sqlite://{db_path}"))
            .await
            .unwrap();
        let telemetry_row = sqlx::query(
            "SELECT 1001 AS channel_id, 1 AS point_id, 'Legacy telemetry' AS signal_name, \
             NULL AS scale, NULL AS offset, NULL AS unit, NULL AS reverse, NULL AS data_type, \
             NULL AS description, NULL AS protocol_mappings",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        let (_, telemetry) = IoSqliteLoader::parse_telemetry_point(&telemetry_row).unwrap();
        assert_eq!(telemetry.scale, 1.0);
        assert_eq!(telemetry.offset, 0.0);
        assert!(!telemetry.reverse);
        assert_eq!(telemetry.data_type, "float32");

        let signal_row = sqlx::query(
            "SELECT 1001 AS channel_id, 2 AS point_id, 'Legacy signal' AS signal_name, \
             NULL AS unit, NULL AS reverse, NULL AS description, NULL AS protocol_mappings",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        let (_, signal) = IoSqliteLoader::parse_signal_point(&signal_row).unwrap();
        assert!(!signal.reverse);

        let control_row = sqlx::query(
            "SELECT 1001 AS channel_id, 3 AS point_id, 'Legacy control' AS signal_name, \
             NULL AS unit, NULL AS reverse, NULL AS description, NULL AS protocol_mappings",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        let (_, control) = IoSqliteLoader::parse_control_point(&control_row).unwrap();
        assert!(!control.reverse);

        let adjustment_row = sqlx::query(
            "SELECT 1001 AS channel_id, 4 AS point_id, 'Legacy adjustment' AS signal_name, \
             NULL AS scale, NULL AS offset, NULL AS unit, NULL AS data_type, NULL AS description, \
             NULL AS protocol_mappings, NULL AS min_value, NULL AS max_value, NULL AS step",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        let (_, adjustment) = IoSqliteLoader::parse_adjustment_point(&adjustment_row).unwrap();
        assert_eq!(adjustment.scale, 1.0);
        assert_eq!(adjustment.offset, 0.0);
        assert_eq!(adjustment.step, 1.0);
        assert_eq!(adjustment.data_type, "float32");

        let snapshot = test_loader(&db_path)
            .await
            .load_runtime_channels()
            .await
            .unwrap()
            .into_iter()
            .find(|snapshot| snapshot.id() == 1001)
            .unwrap();
        assert_eq!(snapshot.point_count(), 4);
    }

    #[tokio::test]
    async fn stored_channel_payload_is_strict_and_accepts_null_description() {
        let (_temp_dir, db_path) = create_test_database().await;
        let db_url = format!("sqlite://{db_path}");
        let pool = SqlitePool::connect(&db_url).await.unwrap();
        sqlx::query(
            "UPDATE channels SET config = ? WHERE channel_id = 1001",
        )
        .bind(
            r#"{"description":null,"parameters":{"host":"192.168.1.100","port":502},"future":true}"#,
        )
        .execute(&pool)
        .await
        .unwrap();

        let loader = test_loader(&db_path).await;
        let snapshot = loader.load_runtime_channel(1001, 2).await.unwrap();
        assert!(snapshot.base.core.description.is_none());

        sqlx::query("UPDATE channels SET config = ? WHERE channel_id = 1001")
            .bind(r#"{"logging":"debug"}"#)
            .execute(&pool)
            .await
            .unwrap();
        let error = loader
            .load_runtime_channel(1001, 3)
            .await
            .expect_err("invalid logging must not fall back to defaults");
        assert!(error.to_string().contains("logging policy is invalid"));
    }

    #[tokio::test]
    async fn test_load_runtime_channel_points_second_modbus_channel() {
        let (_temp_dir, db_path) = create_test_database().await;
        let loader = test_loader(&db_path).await;
        let runtime_config = loader
            .load_runtime_channels()
            .await
            .unwrap()
            .into_iter()
            .nth(1)
            .unwrap();

        // Verify loaded points
        assert_eq!(runtime_config.telemetry_points.len(), 1);

        // Verify protocol_mappings embedded in telemetry point
        let telem_point = &runtime_config.telemetry_points[0];
        assert!(telem_point.base.protocol_mappings.is_some());
        let mappings_json = telem_point.base.protocol_mappings.as_ref().unwrap();
        assert!(mappings_json.contains("update_interval"));
        assert!(mappings_json.contains("1000"));
    }

    #[tokio::test]
    async fn test_parameter_preservation() {
        let (_temp_dir, db_path) = create_test_database().await;
        let loader = test_loader(&db_path).await;
        let channels = loader.load_runtime_channels().await.unwrap();

        // Check that custom parameters from database are preserved
        let modbus_channel = channels
            .iter()
            .find(|c| c.protocol() == "modbus_tcp")
            .unwrap();
        assert_eq!(
            modbus_channel.base.parameters.get("host").unwrap().as_str(),
            Some("192.168.1.100")
        );
        assert_eq!(
            modbus_channel.base.parameters.get("port").unwrap().as_i64(),
            Some(502)
        );
    }

    #[tokio::test]
    async fn test_point_data_types() {
        let (_temp_dir, db_path) = create_test_database().await;
        let loader = test_loader(&db_path).await;
        let runtime_config = loader
            .load_runtime_channels()
            .await
            .unwrap()
            .into_iter()
            .next()
            .unwrap();

        // Check telemetry point
        let telem = &runtime_config.telemetry_points[0];
        assert_eq!(telem.base.point_id, 1);
        assert_eq!(telem.base.signal_name, "Temperature");
        assert_eq!(telem.scale, 0.1);
        assert_eq!(telem.offset, 0.0);
        assert_eq!(telem.data_type, "float32");
        assert!(!telem.reverse);

        // Check signal point
        let signal = &runtime_config.signal_points[0];
        assert_eq!(signal.base.point_id, 2);
        assert_eq!(signal.base.signal_name, "Status");

        // Check control point
        let control = &runtime_config.control_points[0];
        assert_eq!(control.base.point_id, 3);
        assert_eq!(control.base.signal_name, "Start");

        // Check adjustment point
        let adj = &runtime_config.adjustment_points[0];
        assert_eq!(adj.base.point_id, 4);
        assert_eq!(adj.base.signal_name, "Setpoint");
        assert_eq!(adj.min_value, Some(10.0));
        assert_eq!(adj.max_value, Some(30.0));
        assert_eq!(adj.step, 0.5);
    }

    #[tokio::test]
    async fn test_empty_database() {
        let temp_dir = TempDir::new().unwrap();
        let db_path = temp_dir.path().join("empty.db");
        let db_url = format!("sqlite://{}?mode=rwc", db_path.display());

        // Create an empty, complete IO schema.
        let pool = SqlitePool::connect(&db_url).await.unwrap();
        common::schema::init_io_schema(&pool).await.unwrap();

        sqlx::query(
            "INSERT INTO service_config (service_name, key, value) VALUES
                ('aether-io', 'service_name', 'aether-io'),
                ('aether-io', 'port', '6001')",
        )
        .execute(&pool)
        .await
        .unwrap();

        pool.close().await;

        // Should load successfully with no channels
        let loader = test_loader(&db_path).await;
        let channels = loader.load_runtime_channels().await.unwrap();
        assert!(channels.is_empty());
    }
}
