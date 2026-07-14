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
        enabled INTEGER NOT NULL DEFAULT 0,
        config TEXT,
        revision INTEGER NOT NULL DEFAULT 1
            CHECK (TYPEOF(revision) = 'integer' AND revision >= 1),
        created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
        updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
    )
"#;

/// Durable per-identity high-water mark retained after channel deletion.
///
/// A recreated channel must advance beyond this value so a CAS token issued
/// for a deleted entity can never match the new entity (ABA protection).
pub const CHANNEL_REVISION_TOMBSTONES_TABLE: &str = r#"
    CREATE TABLE IF NOT EXISTS channel_revision_tombstones (
        channel_id INTEGER NOT NULL PRIMARY KEY
            CHECK (channel_id >= 0 AND channel_id < 10000),
        last_revision INTEGER NOT NULL
            CHECK (TYPEOF(last_revision) = 'integer' AND last_revision >= 1),
        deleted_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
    )
"#;

/// Reject writes once a channel's durable compare-and-set revision is exhausted.
///
/// The companion bump trigger intentionally supports legacy writers that do
/// not yet mention `revision`. Without this guard SQLite would promote
/// `i64::MAX + 1` to REAL and silently stop providing an integer CAS token.
pub const CHANNEL_REVISION_EXHAUSTED_TRIGGER: &str = r#"
    CREATE TRIGGER IF NOT EXISTS reject_exhausted_channel_revision
    BEFORE UPDATE OF name, protocol, enabled, config ON channels
    FOR EACH ROW
    WHEN NEW.revision = OLD.revision
     AND OLD.revision >= 9223372036854775807
    BEGIN
        SELECT RAISE(ABORT, 'channel revision exhausted');
    END
"#;

/// Increment the channel revision for compatibility writes that omit it.
///
/// Governed writers set `revision = revision + 1` explicitly. In that case
/// `NEW.revision != OLD.revision`, so this trigger does not double-increment.
pub const CHANNEL_REVISION_BUMP_TRIGGER: &str = r#"
    CREATE TRIGGER IF NOT EXISTS bump_channel_revision
    AFTER UPDATE OF name, protocol, enabled, config ON channels
    FOR EACH ROW
    WHEN NEW.revision = OLD.revision
     AND OLD.revision < 9223372036854775807
    BEGIN
        UPDATE channels
        SET revision = OLD.revision + 1
        WHERE channel_id = NEW.channel_id;
    END
"#;

/// Refuse recreation only when no revision remains beyond the tombstone.
pub const CHANNEL_REVISION_INSERT_GUARD_TRIGGER: &str = r#"
    CREATE TRIGGER IF NOT EXISTS reject_exhausted_channel_revision_on_recreate
    BEFORE INSERT ON channels
    FOR EACH ROW
    WHEN EXISTS (
        SELECT 1 FROM channel_revision_tombstones
        WHERE channel_id = NEW.channel_id
          AND last_revision >= 9223372036854775807
    )
    BEGIN
        SELECT RAISE(ABORT, 'channel revision exhausted');
    END
"#;

/// Advance staged legacy INSERTs beyond the deleted entity's high-water mark.
///
/// Formal writers already supply `last_revision + 1`, so they do not match.
/// This AFTER trigger covers sync/import writers that still rely on DEFAULT 1.
pub const CHANNEL_REVISION_INSERT_ADVANCE_TRIGGER: &str = r#"
    CREATE TRIGGER IF NOT EXISTS advance_channel_revision_on_recreate
    AFTER INSERT ON channels
    FOR EACH ROW
    WHEN EXISTS (
        SELECT 1 FROM channel_revision_tombstones
        WHERE channel_id = NEW.channel_id
          AND NEW.revision <= last_revision
    )
    BEGIN
        UPDATE channels
        SET revision = (
            SELECT last_revision + 1
            FROM channel_revision_tombstones
            WHERE channel_id = NEW.channel_id
        )
        WHERE channel_id = NEW.channel_id;
    END
"#;

/// Refuse a legacy deletion when no monotonic tombstone revision remains.
pub const CHANNEL_REVISION_DELETE_EXHAUSTED_TRIGGER: &str = r#"
    CREATE TRIGGER IF NOT EXISTS reject_exhausted_channel_revision_on_delete
    BEFORE DELETE ON channels
    FOR EACH ROW
    WHEN OLD.revision >= 9223372036854775807
    BEGIN
        SELECT RAISE(ABORT, 'channel revision exhausted');
    END
"#;

/// Preserve ABA safety even for staged legacy writers that delete directly.
pub const CHANNEL_REVISION_DELETE_TOMBSTONE_TRIGGER: &str = r#"
    CREATE TRIGGER IF NOT EXISTS tombstone_channel_revision_on_delete
    AFTER DELETE ON channels
    FOR EACH ROW
    BEGIN
        INSERT INTO channel_revision_tombstones
            (channel_id, last_revision, deleted_at)
        VALUES
            (OLD.channel_id, OLD.revision + 1, CURRENT_TIMESTAMP)
        ON CONFLICT(channel_id) DO UPDATE SET
            last_revision = MAX(last_revision, excluded.last_revision),
            deleted_at = excluded.deleted_at;
    END
"#;

/// Install the durable channel revision compatibility triggers.
pub async fn install_channel_revision_triggers(pool: &SqlitePool) -> Result<()> {
    sqlx::query(CHANNEL_REVISION_TOMBSTONES_TABLE)
        .execute(pool)
        .await?;
    sqlx::query(CHANNEL_REVISION_EXHAUSTED_TRIGGER)
        .execute(pool)
        .await?;
    sqlx::query(CHANNEL_REVISION_BUMP_TRIGGER)
        .execute(pool)
        .await?;
    sqlx::query(CHANNEL_REVISION_INSERT_GUARD_TRIGGER)
        .execute(pool)
        .await?;
    sqlx::query(CHANNEL_REVISION_INSERT_ADVANCE_TRIGGER)
        .execute(pool)
        .await?;
    sqlx::query(CHANNEL_REVISION_DELETE_EXHAUSTED_TRIGGER)
        .execute(pool)
        .await?;
    sqlx::query(CHANNEL_REVISION_DELETE_TOMBSTONE_TRIGGER)
        .execute(pool)
        .await?;
    Ok(())
}

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

/// Shared compare-and-set heads for authoritative configuration aggregates.
pub const CONFIGURATION_REVISIONS_TABLE: &str = r#"
    CREATE TABLE IF NOT EXISTS configuration_revisions (
        scope TEXT NOT NULL PRIMARY KEY CHECK (length(trim(scope)) > 0),
        revision INTEGER NOT NULL
            CHECK (TYPEOF(revision) = 'integer' AND revision >= 1),
        updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
    )
"#;

/// Ensures canonical CAS heads exist for online configuration aggregates.
pub async fn initialize_configuration_revisions(pool: &SqlitePool) -> Result<()> {
    sqlx::query(CONFIGURATION_REVISIONS_TABLE)
        .execute(pool)
        .await?;
    sqlx::query(
        "INSERT INTO configuration_revisions (scope, revision) \
         VALUES ('logical_routing', 1) ON CONFLICT(scope) DO NOTHING",
    )
    .execute(pool)
    .await?;
    sqlx::query(
        "INSERT INTO configuration_revisions (scope, revision) \
         VALUES ('automation_rules', 1) ON CONFLICT(scope) DO NOTHING",
    )
    .execute(pool)
    .await?;
    Ok(())
}

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

// ============================================================================
// Logical Routing Integrity Triggers
// ============================================================================

/// Names of the routing-integrity triggers installed in the unified database.
///
/// Production initialization drops these names before reinstalling them so an
/// already initialized database cannot retain an older trigger definition.
pub const LOGICAL_ROUTING_INTEGRITY_TRIGGER_NAMES: &[&str] = &[
    "validate_measurement_routing_target_on_insert",
    "validate_measurement_routing_target_on_update",
    "validate_action_routing_target_on_insert",
    "validate_action_routing_target_on_update",
    "protect_measurement_routing_on_telemetry_delete",
    "protect_measurement_routing_on_telemetry_identity_update",
    "protect_measurement_routing_on_signal_delete",
    "protect_measurement_routing_on_signal_identity_update",
    "protect_action_routing_on_control_delete",
    "protect_action_routing_on_control_identity_update",
    "protect_action_routing_on_adjustment_delete",
    "protect_action_routing_on_adjustment_identity_update",
    "protect_measurement_routing_on_channel_delete",
    "protect_measurement_routing_on_instance_delete",
    "protect_action_routing_on_channel_delete",
    "protect_action_routing_on_instance_delete",
];

/// Canonical DDL for the logical-routing integrity triggers.
///
/// Exposed for the unified-schema migration that must temporarily remove and
/// restore these triggers while rebuilding physical point tables.
pub const LOGICAL_ROUTING_INTEGRITY_TRIGGERS: &[&str] = &[
    r#"
    CREATE TRIGGER IF NOT EXISTS validate_measurement_routing_target_on_insert
    BEFORE INSERT ON measurement_routing
    FOR EACH ROW
    WHEN NOT EXISTS (
             SELECT 1 FROM instances
             WHERE instance_id = NEW.instance_id
               AND instance_name = NEW.instance_name
         )
      OR NEW.channel_id IS NULL
      OR NEW.channel_type IS NULL
      OR NEW.channel_point_id IS NULL
      OR (NEW.channel_type = 'T' AND NOT EXISTS (
             SELECT 1 FROM telemetry_points
             WHERE channel_id = NEW.channel_id AND point_id = NEW.channel_point_id
         ))
      OR (NEW.channel_type = 'S' AND NOT EXISTS (
             SELECT 1 FROM signal_points
             WHERE channel_id = NEW.channel_id AND point_id = NEW.channel_point_id
         ))
      OR NEW.channel_type NOT IN ('T', 'S')
    BEGIN
        SELECT RAISE(ABORT, 'measurement route requires a matching instance and T/S physical target');
    END
    "#,
    r#"
    CREATE TRIGGER IF NOT EXISTS validate_measurement_routing_target_on_update
    BEFORE UPDATE OF instance_id, instance_name, measurement_id, channel_id, channel_type, channel_point_id
    ON measurement_routing
    FOR EACH ROW
    WHEN NOT EXISTS (
             SELECT 1 FROM instances
             WHERE instance_id = NEW.instance_id
               AND instance_name = NEW.instance_name
         )
      OR NEW.channel_id IS NULL
      OR NEW.channel_type IS NULL
      OR NEW.channel_point_id IS NULL
      OR (NEW.channel_type = 'T' AND NOT EXISTS (
             SELECT 1 FROM telemetry_points
             WHERE channel_id = NEW.channel_id AND point_id = NEW.channel_point_id
         ))
      OR (NEW.channel_type = 'S' AND NOT EXISTS (
             SELECT 1 FROM signal_points
             WHERE channel_id = NEW.channel_id AND point_id = NEW.channel_point_id
         ))
      OR NEW.channel_type NOT IN ('T', 'S')
    BEGIN
        SELECT RAISE(ABORT, 'measurement route requires a matching instance and T/S physical target');
    END
    "#,
    r#"
    CREATE TRIGGER IF NOT EXISTS validate_action_routing_target_on_insert
    BEFORE INSERT ON action_routing
    FOR EACH ROW
    WHEN NOT EXISTS (
             SELECT 1 FROM instances
             WHERE instance_id = NEW.instance_id
               AND instance_name = NEW.instance_name
         )
      OR NEW.channel_id IS NULL
      OR NEW.channel_type IS NULL
      OR NEW.channel_point_id IS NULL
      OR (NEW.channel_type = 'C' AND NOT EXISTS (
             SELECT 1 FROM control_points
             WHERE channel_id = NEW.channel_id AND point_id = NEW.channel_point_id
         ))
      OR (NEW.channel_type = 'A' AND NOT EXISTS (
             SELECT 1 FROM adjustment_points
             WHERE channel_id = NEW.channel_id AND point_id = NEW.channel_point_id
         ))
      OR NEW.channel_type NOT IN ('C', 'A')
    BEGIN
        SELECT RAISE(ABORT, 'action route requires a matching instance and C/A physical target');
    END
    "#,
    r#"
    CREATE TRIGGER IF NOT EXISTS validate_action_routing_target_on_update
    BEFORE UPDATE OF instance_id, instance_name, action_id, channel_id, channel_type, channel_point_id
    ON action_routing
    FOR EACH ROW
    WHEN NOT EXISTS (
             SELECT 1 FROM instances
             WHERE instance_id = NEW.instance_id
               AND instance_name = NEW.instance_name
         )
      OR NEW.channel_id IS NULL
      OR NEW.channel_type IS NULL
      OR NEW.channel_point_id IS NULL
      OR (NEW.channel_type = 'C' AND NOT EXISTS (
             SELECT 1 FROM control_points
             WHERE channel_id = NEW.channel_id AND point_id = NEW.channel_point_id
         ))
      OR (NEW.channel_type = 'A' AND NOT EXISTS (
             SELECT 1 FROM adjustment_points
             WHERE channel_id = NEW.channel_id AND point_id = NEW.channel_point_id
         ))
      OR NEW.channel_type NOT IN ('C', 'A')
    BEGIN
        SELECT RAISE(ABORT, 'action route requires a matching instance and C/A physical target');
    END
    "#,
    r#"
    CREATE TRIGGER IF NOT EXISTS protect_measurement_routing_on_telemetry_delete
    BEFORE DELETE ON telemetry_points
    FOR EACH ROW
    WHEN EXISTS (
        SELECT 1 FROM measurement_routing
        WHERE channel_id = OLD.channel_id
          AND channel_type = 'T'
          AND channel_point_id = OLD.point_id
    )
    BEGIN
        SELECT RAISE(ABORT, 'use the governed measurement-routing command before deleting a measurement target');
    END
    "#,
    r#"
    CREATE TRIGGER IF NOT EXISTS protect_measurement_routing_on_telemetry_identity_update
    BEFORE UPDATE OF channel_id, point_id ON telemetry_points
    FOR EACH ROW
    WHEN (NEW.channel_id IS NOT OLD.channel_id OR NEW.point_id IS NOT OLD.point_id)
     AND EXISTS (
        SELECT 1 FROM measurement_routing
        WHERE channel_id = OLD.channel_id
          AND channel_type = 'T'
          AND channel_point_id = OLD.point_id
    )
    BEGIN
        SELECT RAISE(ABORT, 'use the governed measurement-routing command before changing a measurement target identity');
    END
    "#,
    r#"
    CREATE TRIGGER IF NOT EXISTS protect_measurement_routing_on_signal_delete
    BEFORE DELETE ON signal_points
    FOR EACH ROW
    WHEN EXISTS (
        SELECT 1 FROM measurement_routing
        WHERE channel_id = OLD.channel_id
          AND channel_type = 'S'
          AND channel_point_id = OLD.point_id
    )
    BEGIN
        SELECT RAISE(ABORT, 'use the governed measurement-routing command before deleting a measurement target');
    END
    "#,
    r#"
    CREATE TRIGGER IF NOT EXISTS protect_measurement_routing_on_signal_identity_update
    BEFORE UPDATE OF channel_id, point_id ON signal_points
    FOR EACH ROW
    WHEN (NEW.channel_id IS NOT OLD.channel_id OR NEW.point_id IS NOT OLD.point_id)
     AND EXISTS (
        SELECT 1 FROM measurement_routing
        WHERE channel_id = OLD.channel_id
          AND channel_type = 'S'
          AND channel_point_id = OLD.point_id
    )
    BEGIN
        SELECT RAISE(ABORT, 'use the governed measurement-routing command before changing a measurement target identity');
    END
    "#,
    r#"
    CREATE TRIGGER IF NOT EXISTS protect_action_routing_on_control_delete
    BEFORE DELETE ON control_points
    FOR EACH ROW
    WHEN EXISTS (
        SELECT 1 FROM action_routing
        WHERE channel_id = OLD.channel_id
          AND channel_type = 'C'
          AND channel_point_id = OLD.point_id
    )
    BEGIN
        SELECT RAISE(ABORT, 'use the governed action-routing command before deleting an action target');
    END
    "#,
    r#"
    CREATE TRIGGER IF NOT EXISTS protect_action_routing_on_control_identity_update
    BEFORE UPDATE OF channel_id, point_id ON control_points
    FOR EACH ROW
    WHEN (NEW.channel_id IS NOT OLD.channel_id OR NEW.point_id IS NOT OLD.point_id)
     AND EXISTS (
        SELECT 1 FROM action_routing
        WHERE channel_id = OLD.channel_id
          AND channel_type = 'C'
          AND channel_point_id = OLD.point_id
    )
    BEGIN
        SELECT RAISE(ABORT, 'use the governed action-routing command before changing an action target identity');
    END
    "#,
    r#"
    CREATE TRIGGER IF NOT EXISTS protect_action_routing_on_adjustment_delete
    BEFORE DELETE ON adjustment_points
    FOR EACH ROW
    WHEN EXISTS (
        SELECT 1 FROM action_routing
        WHERE channel_id = OLD.channel_id
          AND channel_type = 'A'
          AND channel_point_id = OLD.point_id
    )
    BEGIN
        SELECT RAISE(ABORT, 'use the governed action-routing command before deleting an action target');
    END
    "#,
    r#"
    CREATE TRIGGER IF NOT EXISTS protect_action_routing_on_adjustment_identity_update
    BEFORE UPDATE OF channel_id, point_id ON adjustment_points
    FOR EACH ROW
    WHEN (NEW.channel_id IS NOT OLD.channel_id OR NEW.point_id IS NOT OLD.point_id)
     AND EXISTS (
        SELECT 1 FROM action_routing
        WHERE channel_id = OLD.channel_id
          AND channel_type = 'A'
          AND channel_point_id = OLD.point_id
    )
    BEGIN
        SELECT RAISE(ABORT, 'use the governed action-routing command before changing an action target identity');
    END
    "#,
    r#"
    CREATE TRIGGER IF NOT EXISTS protect_measurement_routing_on_channel_delete
    BEFORE DELETE ON channels
    FOR EACH ROW
    WHEN EXISTS (SELECT 1 FROM measurement_routing WHERE channel_id = OLD.channel_id)
    BEGIN
        SELECT RAISE(ABORT, 'use the governed measurement-routing command before deleting a measurement channel');
    END
    "#,
    r#"
    CREATE TRIGGER IF NOT EXISTS protect_measurement_routing_on_instance_delete
    BEFORE DELETE ON instances
    FOR EACH ROW
    WHEN EXISTS (SELECT 1 FROM measurement_routing WHERE instance_id = OLD.instance_id)
    BEGIN
        SELECT RAISE(ABORT, 'use the governed measurement-routing command before deleting a measurement instance');
    END
    "#,
    r#"
    CREATE TRIGGER IF NOT EXISTS protect_action_routing_on_channel_delete
    BEFORE DELETE ON channels
    FOR EACH ROW
    WHEN EXISTS (SELECT 1 FROM action_routing WHERE channel_id = OLD.channel_id)
    BEGIN
        SELECT RAISE(ABORT, 'use the governed action-routing command before deleting an action channel');
    END
    "#,
    r#"
    CREATE TRIGGER IF NOT EXISTS protect_action_routing_on_instance_delete
    BEFORE DELETE ON instances
    FOR EACH ROW
    WHEN EXISTS (SELECT 1 FROM action_routing WHERE instance_id = OLD.instance_id)
    BEGIN
        SELECT RAISE(ABORT, 'use the governed action-routing command before deleting an action instance');
    END
    "#,
];

/// Installs fail-closed referential integrity for the logical-routing tables.
///
/// Call this only after `channels`, all four physical point tables,
/// `instances`, and both routing tables have been created. The split IO and
/// automation test-schema helpers cannot install it independently because
/// neither owns that complete unified schema.
pub async fn install_logical_routing_integrity_triggers(pool: &SqlitePool) -> Result<()> {
    for trigger in LOGICAL_ROUTING_INTEGRITY_TRIGGERS {
        sqlx::query(trigger).execute(pool).await?;
    }
    Ok(())
}

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
    install_channel_revision_triggers(pool).await?;

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
/// No products table is created. Product definitions come from validated active
/// Packs and an optional site directory at runtime.
pub async fn init_automation_schema(pool: &SqlitePool) -> Result<()> {
    // Service metadata tables
    sqlx::query(SERVICE_CONFIG_TABLE).execute(pool).await?;
    sqlx::query(SYNC_METADATA_TABLE).execute(pool).await?;

    // Channels table (required by routing table foreign keys in unified database architecture)
    sqlx::query(CHANNELS_TABLE).execute(pool).await?;
    install_channel_revision_triggers(pool).await?;

    // Instance table (no longer references products table)
    sqlx::query(INSTANCES_TABLE).execute(pool).await?;

    // Routing tables
    sqlx::query(MEASUREMENT_ROUTING_TABLE).execute(pool).await?;
    sqlx::query(ACTION_ROUTING_TABLE).execute(pool).await?;
    initialize_configuration_revisions(pool).await?;

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
    initialize_configuration_revisions(pool).await?;

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

        let revision: i64 = sqlx::query_scalar(
            "INSERT INTO channels (channel_id, name, protocol, enabled, config) \
             VALUES (7, 'revision-contract', 'virtual', 0, '{}') \
             RETURNING revision",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(revision, 1);

        sqlx::query("UPDATE channels SET name = 'legacy-update' WHERE channel_id = 7")
            .execute(&pool)
            .await
            .unwrap();
        let revision: i64 =
            sqlx::query_scalar("SELECT revision FROM channels WHERE channel_id = 7")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(revision, 2, "legacy writes must receive a revision");

        sqlx::query(
            "UPDATE channels SET name = 'governed-update', revision = revision + 1 \
             WHERE channel_id = 7",
        )
        .execute(&pool)
        .await
        .unwrap();
        let revision: i64 =
            sqlx::query_scalar("SELECT revision FROM channels WHERE channel_id = 7")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(revision, 3, "governed writes must not be bumped twice");
    }

    #[tokio::test]
    async fn channel_revision_trigger_covers_legacy_and_explicit_writers() {
        let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
        init_io_schema(&pool).await.unwrap();
        sqlx::query(
            "INSERT INTO channels (channel_id, name, protocol, enabled, config) \
             VALUES (7, 'legacy-writer', 'virtual', 0, '{}')",
        )
        .execute(&pool)
        .await
        .unwrap();

        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT revision FROM channels WHERE channel_id = 7")
                .fetch_one(&pool)
                .await
                .unwrap(),
            1
        );

        sqlx::query("UPDATE channels SET name = 'legacy-updated' WHERE channel_id = 7")
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT revision FROM channels WHERE channel_id = 7")
                .fetch_one(&pool)
                .await
                .unwrap(),
            2
        );

        sqlx::query(
            "UPDATE channels SET enabled = 1, revision = 3 \
             WHERE channel_id = 7 AND revision = 2",
        )
        .execute(&pool)
        .await
        .unwrap();
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT revision FROM channels WHERE channel_id = 7")
                .fetch_one(&pool)
                .await
                .unwrap(),
            3,
            "explicit CAS revision must not be incremented twice"
        );

        sqlx::query("UPDATE channels SET updated_at = '2099-01-01 00:00:00' WHERE channel_id = 7")
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT revision FROM channels WHERE channel_id = 7")
                .fetch_one(&pool)
                .await
                .unwrap(),
            3,
            "non-desired metadata updates must not bump revision"
        );

        sqlx::query("UPDATE channels SET revision = 9223372036854775807 WHERE channel_id = 7")
            .execute(&pool)
            .await
            .unwrap();
        let exhausted =
            sqlx::query("UPDATE channels SET name = 'must-not-overflow' WHERE channel_id = 7")
                .execute(&pool)
                .await
                .expect_err("legacy desired update must not overflow revision");
        assert!(exhausted.to_string().contains("channel revision exhausted"));
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT revision FROM channels WHERE channel_id = 7")
                .fetch_one(&pool)
                .await
                .unwrap(),
            i64::MAX
        );
    }

    #[tokio::test]
    async fn channel_revision_tombstone_prevents_delete_recreate_aba() {
        let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
        init_io_schema(&pool).await.unwrap();
        sqlx::query(
            "INSERT INTO channels (channel_id, name, protocol, enabled, config) \
             VALUES (8, 'first-entity', 'virtual', 0, '{}')",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query("DELETE FROM channels WHERE channel_id = 8")
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT last_revision FROM channel_revision_tombstones WHERE channel_id = 8",
            )
            .fetch_one(&pool)
            .await
            .unwrap(),
            2
        );

        sqlx::query(
            "INSERT INTO channels (channel_id, name, protocol, enabled, config) \
             VALUES (8, 'legacy-recreate', 'virtual', 0, '{}')",
        )
        .execute(&pool)
        .await
        .unwrap();
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT revision FROM channels WHERE channel_id = 8")
                .fetch_one(&pool)
                .await
                .unwrap(),
            3,
            "legacy recreation must advance beyond the delete tombstone"
        );
        assert_eq!(
            sqlx::query(
                "UPDATE channels SET name = 'stale-cas' WHERE channel_id = 8 AND revision = 1"
            )
            .execute(&pool)
            .await
            .unwrap()
            .rows_affected(),
            0,
            "a stale CAS token must not match the recreated entity"
        );
    }

    #[tokio::test]
    async fn exhausted_channel_tombstone_rejects_recreation_before_insert() {
        let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
        init_io_schema(&pool).await.unwrap();
        sqlx::query(
            "INSERT INTO channel_revision_tombstones (channel_id, last_revision) \
             VALUES (9, 9223372036854775807)",
        )
        .execute(&pool)
        .await
        .unwrap();

        let error = sqlx::query(
            "INSERT INTO channels (channel_id, name, protocol, enabled, config) \
             VALUES (9, 'exhausted-recreate', 'virtual', 0, '{}')",
        )
        .execute(&pool)
        .await
        .expect_err("an exhausted identity must not be recreated");
        assert!(error.to_string().contains("channel revision exhausted"));
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM channels WHERE channel_id = 9")
                .fetch_one(&pool)
                .await
                .unwrap(),
            0,
            "the BEFORE guard must reject the row itself"
        );
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
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT revision FROM configuration_revisions \
                 WHERE scope = 'logical_routing'",
            )
            .fetch_one(&pool)
            .await
            .unwrap(),
            1
        );
    }

    #[tokio::test]
    async fn logical_routing_trigger_helper_rejects_a_missing_physical_target() {
        let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
        init_automation_schema(&pool).await.unwrap();
        init_io_schema(&pool).await.unwrap();
        install_logical_routing_integrity_triggers(&pool)
            .await
            .unwrap();

        sqlx::query(
            "INSERT INTO instances (instance_id, instance_name, product_name) \
             VALUES (7, 'fixture', 'ExampleDevice')",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO channels (channel_id, name, protocol, enabled) \
             VALUES (3, 'fixture-channel', 'virtual', 0)",
        )
        .execute(&pool)
        .await
        .unwrap();

        let error = sqlx::query(
            "INSERT INTO measurement_routing \
             (instance_id, instance_name, measurement_id, channel_id, channel_type, channel_point_id) \
             VALUES (7, 'fixture', 1, 3, 'T', 99)",
        )
        .execute(&pool)
        .await
        .expect_err("routing integrity must reject a missing physical point");
        assert!(
            error
                .to_string()
                .contains("matching instance and T/S physical target"),
            "unexpected error: {error}"
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
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT revision FROM configuration_revisions \
                 WHERE scope = 'automation_rules'",
            )
            .fetch_one(&pool)
            .await
            .unwrap(),
            1
        );
    }
}
