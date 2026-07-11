//! Test database schema utilities
//!
//! Provides helper functions to initialize test databases with standard schemas.
//! This eliminates the need for duplicate CREATE TABLE statements across test files.
//!
//! # Usage
//!
//! ```rust,ignore
//! use common::test_utils::schema;
//! use sqlx::SqlitePool;
//!
//! #[tokio::test]
//! async fn test_something() {
//!     let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
//!     schema::init_io_schema(&pool).await.unwrap();
//!
//!     // Now use the pool with standard io tables
//! }
//! ```

use anyhow::Result;
use sqlx::SqlitePool;

// Re-export common table constants
pub use crate::{SERVICE_CONFIG_TABLE, SYNC_METADATA_TABLE};

// ============================================================================
// Io Table DDL
// ============================================================================

/// Channels table DDL (matches io::core::config::ChannelRecord)
pub const CHANNELS_TABLE: &str = r#"
    CREATE TABLE IF NOT EXISTS channels (
        channel_id INTEGER NOT NULL PRIMARY KEY,
        name TEXT NOT NULL UNIQUE,
        protocol TEXT,
        enabled INTEGER NOT NULL DEFAULT 1,
        config TEXT,
        created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
        updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
    )
"#;

/// Telemetry points table DDL (matches io::core::config::TelemetryPointRecord)
pub const TELEMETRY_POINTS_TABLE: &str = r#"
    CREATE TABLE IF NOT EXISTS telemetry_points (
        point_id INTEGER NOT NULL,
        channel_id INTEGER NOT NULL REFERENCES channels(channel_id) ON DELETE CASCADE,
        signal_name TEXT NOT NULL,
        scale REAL DEFAULT 1.0,
        offset REAL DEFAULT 0.0,
        unit TEXT,
        reverse INTEGER DEFAULT 0,
        data_type TEXT,
        description TEXT,
        protocol_mappings TEXT,
        PRIMARY KEY (channel_id, point_id)
    )
"#;

/// Signal points table DDL (matches io::core::config::SignalPointRecord)
pub const SIGNAL_POINTS_TABLE: &str = r#"
    CREATE TABLE IF NOT EXISTS signal_points (
        point_id INTEGER NOT NULL,
        channel_id INTEGER NOT NULL REFERENCES channels(channel_id) ON DELETE CASCADE,
        signal_name TEXT NOT NULL,
        scale REAL DEFAULT 1.0,
        offset REAL DEFAULT 0.0,
        unit TEXT,
        reverse INTEGER DEFAULT 0,
        normal_state INTEGER DEFAULT 0,
        data_type TEXT,
        description TEXT,
        protocol_mappings TEXT,
        PRIMARY KEY (channel_id, point_id)
    )
"#;

/// Control points table DDL (matches io::core::config::ControlPointRecord)
pub const CONTROL_POINTS_TABLE: &str = r#"
    CREATE TABLE IF NOT EXISTS control_points (
        point_id INTEGER NOT NULL,
        channel_id INTEGER NOT NULL REFERENCES channels(channel_id) ON DELETE CASCADE,
        signal_name TEXT NOT NULL,
        scale REAL DEFAULT 1.0,
        offset REAL DEFAULT 0.0,
        unit TEXT,
        reverse INTEGER DEFAULT 0,
        data_type TEXT,
        description TEXT,
        protocol_mappings TEXT,
        PRIMARY KEY (channel_id, point_id)
    )
"#;

/// Adjustment points table DDL (matches io::core::config::AdjustmentPointRecord)
pub const ADJUSTMENT_POINTS_TABLE: &str = r#"
    CREATE TABLE IF NOT EXISTS adjustment_points (
        point_id INTEGER NOT NULL,
        channel_id INTEGER NOT NULL REFERENCES channels(channel_id) ON DELETE CASCADE,
        signal_name TEXT NOT NULL,
        scale REAL DEFAULT 1.0,
        offset REAL DEFAULT 0.0,
        unit TEXT,
        reverse INTEGER DEFAULT 0,
        data_type TEXT,
        description TEXT,
        protocol_mappings TEXT,
        min_value REAL,
        max_value REAL,
        step REAL DEFAULT 1.0,
        PRIMARY KEY (channel_id, point_id)
    )
"#;

// ============================================================================
// Channel Templates DDL
// ============================================================================

/// Channel templates table DDL — stores point configuration snapshots as JSON
///
/// Templates capture a channel's complete point definitions and protocol mappings,
/// enabling "save once → apply many" workflows for devices with identical configurations.
pub const CHANNEL_TEMPLATES_TABLE: &str = r#"
    CREATE TABLE IF NOT EXISTS channel_templates (
        template_id       INTEGER PRIMARY KEY AUTOINCREMENT,
        name              TEXT NOT NULL UNIQUE,
        description       TEXT,
        protocol          TEXT NOT NULL,
        points_snapshot   TEXT NOT NULL,
        mappings_snapshot TEXT NOT NULL,
        source_channel_id INTEGER REFERENCES channels(channel_id) ON DELETE SET NULL,
        created_at        TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
        updated_at        TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
    )
"#;

/// Index on channel_templates.source_channel_id — accelerates lookups by source
/// channel and lets `ON DELETE SET NULL` cascade cheaply.
pub const CHANNEL_TEMPLATES_SOURCE_INDEX: &str = "CREATE INDEX IF NOT EXISTS idx_channel_templates_source ON channel_templates(source_channel_id)";

// ============================================================================
// Automation Table DDL (matches automation::config schemas)
// ============================================================================

/// Instances table DDL (matches automation::config::InstanceRecord)
/// Note: No foreign key to products table - products are compile-time constants
pub const INSTANCES_TABLE: &str = r#"
    CREATE TABLE IF NOT EXISTS instances (
        instance_id INTEGER NOT NULL PRIMARY KEY,
        instance_name TEXT NOT NULL UNIQUE,
        product_name TEXT NOT NULL,
        parent_id INTEGER,
        created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
        updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
        FOREIGN KEY (parent_id) REFERENCES instances(instance_id) ON DELETE SET NULL
    )
"#;

/// Measurement routing table DDL (matches automation::config::MeasurementRoutingRecord)
pub const MEASUREMENT_ROUTING_TABLE: &str = r#"
    CREATE TABLE IF NOT EXISTS measurement_routing (
        routing_id INTEGER PRIMARY KEY AUTOINCREMENT,
        instance_id INTEGER NOT NULL REFERENCES instances(instance_id) ON DELETE CASCADE,
        instance_name TEXT NOT NULL,
        channel_id INTEGER REFERENCES channels(channel_id) ON DELETE SET NULL,
        channel_type TEXT,
        channel_point_id INTEGER,
        measurement_id INTEGER NOT NULL,
        description TEXT,
        enabled INTEGER NOT NULL DEFAULT 1,
        created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
        updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
        UNIQUE(instance_id, measurement_id),
        CHECK(channel_type IN ('T','S'))
    )
"#;

/// Action routing table DDL (matches automation::config::ActionRoutingRecord)
pub const ACTION_ROUTING_TABLE: &str = r#"
    CREATE TABLE IF NOT EXISTS action_routing (
        routing_id INTEGER PRIMARY KEY AUTOINCREMENT,
        instance_id INTEGER NOT NULL REFERENCES instances(instance_id) ON DELETE CASCADE,
        instance_name TEXT NOT NULL,
        action_id INTEGER NOT NULL,
        channel_id INTEGER REFERENCES channels(channel_id) ON DELETE SET NULL,
        channel_type TEXT,
        channel_point_id INTEGER,
        description TEXT,
        enabled INTEGER NOT NULL DEFAULT 1,
        created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
        updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
        UNIQUE(instance_id, action_id),
        CHECK(channel_type IN ('C','A'))
    )
"#;

/// Instance property values table DDL
///
/// One row per (instance_id, property_id). `value_json` holds the property's
/// current value as a JSON-encoded string (any JSON type is accepted —
/// number, string, bool, null, object, array). `property_id` references the
/// PropertyTemplate declared by the instance's product (a compile-time
/// constant in the `aether-model` crate, so no foreign key is possible —
/// handlers validate the id against the template).
pub const INSTANCE_PROPERTIES_TABLE: &str = r#"
    CREATE TABLE IF NOT EXISTS instance_properties (
        instance_id INTEGER NOT NULL REFERENCES instances(instance_id) ON DELETE CASCADE,
        property_id INTEGER NOT NULL,
        value_json  TEXT    NOT NULL,
        updated_at  TEXT    NOT NULL DEFAULT CURRENT_TIMESTAMP,
        PRIMARY KEY (instance_id, property_id)
    )
"#;

// ============================================================================
// Rules Table DDL
// ============================================================================

/// Rule chains table DDL (Vue Flow format).
///
/// `id` uses AUTOINCREMENT to prevent SQLite from reusing rowids of deleted
/// rules — otherwise rule_history rows referencing a deleted rule could be
/// silently re-bound to a new rule with the same id.
pub const RULE_CHAINS_TABLE: &str = r#"
    CREATE TABLE IF NOT EXISTS rules (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        name TEXT NOT NULL,
        description TEXT,
        enabled INTEGER DEFAULT 1,
        priority INTEGER DEFAULT 0,
        cooldown_ms INTEGER DEFAULT 0,
        trigger_config TEXT,
        nodes_json TEXT NOT NULL,
        flow_json TEXT,
        format TEXT DEFAULT 'vue-flow',
        created_at TEXT DEFAULT CURRENT_TIMESTAMP,
        updated_at TEXT DEFAULT CURRENT_TIMESTAMP
    )
"#;

/// Rule history table DDL — `rule_id` cascades so deleting a rule purges its
/// historical execution records (no orphaned history rows).
pub const RULE_HISTORY_TABLE: &str = r#"
    CREATE TABLE IF NOT EXISTS rule_history (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        rule_id INTEGER NOT NULL REFERENCES rules(id) ON DELETE CASCADE,
        triggered_at TEXT NOT NULL,
        execution_result TEXT,
        error TEXT
    )
"#;

// ============================================================================
// Schema Initialization Functions
// ============================================================================

/// Initialize io standard schema for testing
///
/// Creates all io-related tables.
/// This includes:
/// - service_config
/// - sync_metadata
/// - channels
/// - telemetry_points, signal_points, control_points, adjustment_points
pub async fn init_io_schema(pool: &SqlitePool) -> Result<()> {
    // Service metadata tables
    sqlx::query(SERVICE_CONFIG_TABLE).execute(pool).await?;
    sqlx::query(SYNC_METADATA_TABLE).execute(pool).await?;

    // Core channel table
    sqlx::query(CHANNELS_TABLE).execute(pool).await?;

    // Point tables
    sqlx::query(TELEMETRY_POINTS_TABLE).execute(pool).await?;
    sqlx::query(SIGNAL_POINTS_TABLE).execute(pool).await?;
    sqlx::query(CONTROL_POINTS_TABLE).execute(pool).await?;
    sqlx::query(ADJUSTMENT_POINTS_TABLE).execute(pool).await?;

    // Channel templates table
    sqlx::query(CHANNEL_TEMPLATES_TABLE).execute(pool).await?;
    sqlx::query(CHANNEL_TEMPLATES_SOURCE_INDEX)
        .execute(pool)
        .await?;

    Ok(())
}

/// Initialize automation standard schema for testing
///
/// Creates all automation-related tables.
/// This includes:
/// - service_config
/// - sync_metadata
/// - channels (required by routing table foreign keys)
/// - instances
/// - measurement_routing, action_routing
///
/// Note: Products are now compile-time built-in constants from aether-model crate.
/// No products table is created. Use built-in product names like "Battery", "PCS", etc.
pub async fn init_automation_schema(pool: &SqlitePool) -> Result<()> {
    // Service metadata tables
    sqlx::query(SERVICE_CONFIG_TABLE).execute(pool).await?;
    sqlx::query(SYNC_METADATA_TABLE).execute(pool).await?;

    // Channels table (required by routing table foreign keys in unified database architecture)
    sqlx::query(CHANNELS_TABLE).execute(pool).await?;

    // Instance table (no longer references products table)
    sqlx::query(INSTANCES_TABLE).execute(pool).await?;

    // Routing tables
    sqlx::query(MEASUREMENT_ROUTING_TABLE).execute(pool).await?;
    sqlx::query(ACTION_ROUTING_TABLE).execute(pool).await?;

    // Instance property values (one row per property)
    sqlx::query(INSTANCE_PROPERTIES_TABLE).execute(pool).await?;

    Ok(())
}

/// Initialize rules standard schema for testing
///
/// Creates all rules-related tables.
/// This includes:
/// - service_config
/// - sync_metadata
/// - rules (Vue Flow rule chains)
/// - rule_history
pub async fn init_rules_schema(pool: &SqlitePool) -> Result<()> {
    // Service metadata tables
    sqlx::query(SERVICE_CONFIG_TABLE).execute(pool).await?;
    sqlx::query(SYNC_METADATA_TABLE).execute(pool).await?;

    // Rule chains table (Vue Flow format)
    sqlx::query(RULE_CHAINS_TABLE).execute(pool).await?;
    sqlx::query(RULE_HISTORY_TABLE).execute(pool).await?;

    Ok(())
}

#[cfg(test)]
#[allow(clippy::disallowed_methods)] // Test code - unwrap is acceptable
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_init_io_schema() {
        let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
        init_io_schema(&pool).await.unwrap();

        // Verify tables exist by querying them
        let result: (i64,) =
            sqlx::query_as("SELECT COUNT(*) FROM sqlite_master WHERE type='table'")
                .fetch_one(&pool)
                .await
                .unwrap();

        // Should have 8 tables: service_config, sync_metadata, channels, 4 point tables, channel_templates
        assert!(
            result.0 >= 8,
            "Expected at least 8 tables, found {}",
            result.0
        );

        for table in [
            "telemetry_points",
            "signal_points",
            "control_points",
            "adjustment_points",
        ] {
            let on_delete: String = sqlx::query_scalar(&format!(
                "SELECT on_delete FROM pragma_foreign_key_list('{table}') \
                 WHERE \"table\" = 'channels' \
                   AND \"from\" = 'channel_id' \
                   AND \"to\" = 'channel_id'"
            ))
            .fetch_one(&pool)
            .await
            .unwrap();
            assert_eq!(on_delete, "CASCADE", "wrong delete action for {table}");
        }
    }

    #[tokio::test]
    async fn test_init_automation_schema() {
        let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
        init_automation_schema(&pool).await.unwrap();

        // Verify tables exist
        let result: (i64,) =
            sqlx::query_as("SELECT COUNT(*) FROM sqlite_master WHERE type='table'")
                .fetch_one(&pool)
                .await
                .unwrap();

        // Should have 6 tables: service_config, sync_metadata, channels, instances,
        // measurement_routing, action_routing
        assert!(
            result.0 >= 6,
            "Expected at least 6 tables, found {}",
            result.0
        );
    }

    #[tokio::test]
    async fn test_init_rules_schema() {
        let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
        init_rules_schema(&pool).await.unwrap();

        // Verify tables exist
        let result: (i64,) =
            sqlx::query_as("SELECT COUNT(*) FROM sqlite_master WHERE type='table'")
                .fetch_one(&pool)
                .await
                .unwrap();

        // Should have 4 tables: service_config, sync_metadata, rules, rule_history
        assert!(
            result.0 >= 4,
            "Expected at least 4 tables, found {}",
            result.0
        );
    }
}
