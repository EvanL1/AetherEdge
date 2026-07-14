//! Database schema initialization
//!
//! Provides unified database initialization for all Aether tables.
//! All tables are created in a single `aether.db` file.

use anyhow::{Context, Result, anyhow, bail, ensure};
use sqlx::{Connection, Row, Sqlite, SqliteConnection, SqlitePool};
use std::path::Path;
use tracing::{info, warn};

// Import DDL constants from common (shared schema definitions)
use common::test_utils::schema::{
    ACTION_ROUTING_TABLE, ADJUSTMENT_POINTS_TABLE, CHANNEL_REVISION_BUMP_TRIGGER,
    CHANNEL_REVISION_DELETE_EXHAUSTED_TRIGGER, CHANNEL_REVISION_DELETE_TOMBSTONE_TRIGGER,
    CHANNEL_REVISION_EXHAUSTED_TRIGGER, CHANNEL_REVISION_INSERT_ADVANCE_TRIGGER,
    CHANNEL_REVISION_INSERT_GUARD_TRIGGER, CHANNEL_REVISION_TOMBSTONES_TABLE,
    CHANNEL_TEMPLATES_TABLE, CHANNELS_TABLE, CONFIGURATION_REVISIONS_TABLE, CONTROL_POINTS_TABLE,
    INSTANCE_PROPERTIES_TABLE, INSTANCES_TABLE, LOGICAL_ROUTING_INTEGRITY_TRIGGER_NAMES,
    LOGICAL_ROUTING_INTEGRITY_TRIGGERS, MEASUREMENT_ROUTING_TABLE, SERVICE_CONFIG_TABLE,
    SIGNAL_POINTS_TABLE, SYNC_METADATA_TABLE, TELEMETRY_POINTS_TABLE,
};

use super::file_utils;

// ============================================================================
// Rules DDL (defined locally since rules are managed by aether)
// ============================================================================

/// Rules table SQL — mirrors `libs/common::test_utils::schema::RULE_CHAINS_TABLE`.
///
/// `id` uses AUTOINCREMENT so deleted rowids are never reused, which prevents
/// `rule_history` rows from silently being re-bound to a new rule with the
/// same id. All booleans are stored as INTEGER 1/0 for cross-version SQLite
/// compatibility; timestamps as TEXT (CURRENT_TIMESTAMP) for consistency
/// with the rest of the schema.
const RULE_CHAINS_TABLE: &str = r#"
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

/// Rule history table SQL — `rule_id` cascades on rule delete to prevent
/// orphaned history rows (which would silently rebind under AUTOINCREMENT
/// ID reuse — see v6 migration notes).
const RULE_HISTORY_TABLE: &str = r#"
    CREATE TABLE IF NOT EXISTS rule_history (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        rule_id INTEGER NOT NULL REFERENCES rules(id) ON DELETE CASCADE,
        triggered_at TEXT NOT NULL,
        execution_result TEXT,
        error TEXT
    )
"#;

// ============================================================================
// Schema Version Migration
// ============================================================================
//
// Uses SQLite's built-in PRAGMA user_version to track schema structure version.
// Each breaking schema change gets a new version with a migration function.
//
// To add a new migration:
//   1. Increment SCHEMA_VERSION
//   2. Add `migrate_vN()` function
//   3. Add `if current < N { migrate_vN(&mut conn).await?; }` in run_migrations()

/// Current schema structure version — increment when adding migrations
pub(crate) const SCHEMA_VERSION: i32 = 12;

/// Run pending schema migrations based on `PRAGMA user_version`
///
/// Reads the database's current version, executes any outstanding migrations
/// sequentially, then stamps the new version. All migration queries run on
/// a single connection to keep `PRAGMA foreign_keys` state consistent.
async fn run_migrations(pool: &SqlitePool) -> Result<()> {
    let current: i32 = sqlx::query_scalar("PRAGMA user_version")
        .fetch_one(pool)
        .await?;

    if current >= SCHEMA_VERSION {
        return Ok(());
    }

    info!("Schema migration: v{current} -> v{SCHEMA_VERSION}",);

    // Acquire a single connection — PRAGMA foreign_keys is per-connection
    let mut conn = pool.acquire().await?;

    if current < 1 {
        migrate_v0(&mut conn).await.context("Migration v0 failed")?;
        migrate_v1(&mut conn).await.context("Migration v1 failed")?;
    }

    if current < 2 {
        migrate_v2(&mut conn).await.context("Migration v2 failed")?;
    }

    if current < 3 {
        migrate_v3(&mut conn).await.context("Migration v3 failed")?;
    }

    if current < 4 {
        migrate_v4(&mut conn).await.context("Migration v4 failed")?;
    }

    if current < 5 {
        migrate_v5(&mut conn).await.context("Migration v5 failed")?;
    }

    if current < 6 {
        migrate_v6(&mut conn).await.context("Migration v6 failed")?;
    }

    if current < 7 {
        migrate_v7(&mut conn).await.context("Migration v7 failed")?;
    }

    if current < 8 {
        migrate_v8(&mut conn).await.context("Migration v8 failed")?;
    }

    if current < 9 {
        migrate_v9(&mut conn).await.context("Migration v9 failed")?;
    }

    if current < 10 {
        migrate_v10(&mut conn)
            .await
            .context("Migration v10 failed")?;
    }

    if current < 11 {
        migrate_v11(&mut conn)
            .await
            .context("Migration v11 failed")?;
    }

    if current < 12 {
        migrate_v12(&mut conn)
            .await
            .context("Migration v12 failed")?;
    }

    sqlx::query(&format!("PRAGMA user_version = {SCHEMA_VERSION}"))
        .execute(&mut *conn)
        .await?;

    info!("Schema migration complete: v{SCHEMA_VERSION}");
    Ok(())
}

/// v0: Legacy ad-hoc rules-table rebuild (originally `migrate_rules_table_if_needed`).
///
/// Old prototype shape used `id TEXT` on the `rules` table. If we encounter
/// such a table on a freshly-imported DB, drop both `rules` and `rule_history`
/// so the post-migration `CREATE TABLE IF NOT EXISTS` recreates them with
/// the correct schema. Gated by `current < 1` in run_migrations so it only
/// runs on databases that pre-date the user_version system.
async fn migrate_v0(conn: &mut sqlx::pool::PoolConnection<Sqlite>) -> Result<()> {
    let row = sqlx::query("SELECT type FROM pragma_table_info('rules') WHERE name = 'id'")
        .fetch_optional(&mut **conn)
        .await?;

    let Some(row) = row else {
        return Ok(()); // no rules table yet, nothing to do
    };

    let col_type: String = row.try_get("type")?;
    if !col_type.eq_ignore_ascii_case("TEXT") {
        return Ok(()); // already INTEGER-keyed, skip
    }

    warn!("Migration v0: legacy rules table (id TEXT) detected — dropping for rebuild");
    sqlx::query("DROP TABLE IF EXISTS rule_history")
        .execute(&mut **conn)
        .await?;
    sqlx::query("DROP TABLE IF EXISTS rules")
        .execute(&mut **conn)
        .await?;
    Ok(())
}

/// v1: Remove `products` foreign key from `instances` table, drop obsolete tables
///
/// Old schema had `REFERENCES products(product_name)` on instances.product_name.
/// Products are now compile-time constants — the FK and table are no longer needed.
async fn migrate_v1(conn: &mut sqlx::pool::PoolConnection<Sqlite>) -> Result<()> {
    let has_products: bool =
        sqlx::query_scalar("SELECT 1 FROM sqlite_master WHERE type='table' AND name='products'")
            .fetch_optional(&mut **conn)
            .await?
            .unwrap_or(false);

    if !has_products {
        info!("Migration v1: skipped (products table not found)");
        return Ok(());
    }

    info!("Migration v1: rebuilding instances table, removing products FK");

    sqlx::query("PRAGMA foreign_keys=OFF")
        .execute(&mut **conn)
        .await?;

    // Rebuild instances without products FK (matches INSTANCES_TABLE DDL)
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS instances_new (
            instance_id INTEGER NOT NULL PRIMARY KEY,
            instance_name TEXT NOT NULL UNIQUE,
            product_name TEXT NOT NULL,
            parent_id INTEGER,
            properties TEXT,
            created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
            updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
            FOREIGN KEY (parent_id) REFERENCES instances(instance_id) ON DELETE SET NULL
        )",
    )
    .execute(&mut **conn)
    .await?;

    // Copy data — old table has no parent_id, defaults to NULL
    sqlx::query(
        "INSERT INTO instances_new
            (instance_id, instance_name, product_name, properties, created_at, updated_at)
         SELECT instance_id, instance_name, product_name, properties, created_at, updated_at
         FROM instances",
    )
    .execute(&mut **conn)
    .await?;

    sqlx::query("DROP TABLE instances")
        .execute(&mut **conn)
        .await?;
    sqlx::query("ALTER TABLE instances_new RENAME TO instances")
        .execute(&mut **conn)
        .await?;

    // Drop obsolete product-related tables
    for table in [
        "products",
        "measurement_points",
        "action_points",
        "property_templates",
        "product_library_meta",
    ] {
        sqlx::query(&format!("DROP TABLE IF EXISTS {table}"))
            .execute(&mut **conn)
            .await?;
    }

    sqlx::query("PRAGMA foreign_keys=ON")
        .execute(&mut **conn)
        .await?;

    info!("Migration v1: complete");
    Ok(())
}

/// v2 marker retained for schema-number continuity.
///
/// Domain product aliases were removed from the generic kernel in 0.5.0.
/// Distributions that used them must apply their Pack-owned compatibility
/// mapping before running the kernel schema upgrade.
async fn migrate_v2(_conn: &mut sqlx::pool::PoolConnection<Sqlite>) -> Result<()> {
    info!("Migration v2: domain product aliases are Pack-owned (kernel no-op)");
    Ok(())
}

/// v3: Add `channel_templates` table for protocol point-table template management
///
/// Stores JSON snapshots of channel point definitions and protocol mappings,
/// enabling "save once → apply many" workflows for identically-configured devices.
async fn migrate_v3(conn: &mut sqlx::pool::PoolConnection<Sqlite>) -> Result<()> {
    info!("Migration v3: creating channel_templates table");

    sqlx::query(CHANNEL_TEMPLATES_TABLE)
        .execute(&mut **conn)
        .await?;

    info!("Migration v3: complete");
    Ok(())
}

/// v4: Add `trigger_config` column to `rules` table
///
/// The column was added to DDL definitions but never migrated for existing databases.
/// Without it, `repository.rs::hydrate_rule()` fails with "no such column: trigger_config",
/// causing the rule engine to load zero rules.
async fn migrate_v4(conn: &mut sqlx::pool::PoolConnection<Sqlite>) -> Result<()> {
    info!("Migration v4: adding trigger_config column to rules table");

    // If the rules table doesn't exist yet, skip — it will be created fresh
    // with trigger_config included when aether init runs the full DDL.
    let table_exists: bool = sqlx::query_scalar(
        "SELECT COUNT(*) > 0 FROM sqlite_master WHERE type='table' AND name='rules'",
    )
    .fetch_one(&mut **conn)
    .await?;

    if !table_exists {
        info!("Migration v4: rules table not yet created, skipping ALTER TABLE");
        return Ok(());
    }

    // Check if column already exists (idempotent)
    let has_column: bool = sqlx::query_scalar(
        "SELECT COUNT(*) > 0 FROM pragma_table_info('rules') WHERE name = 'trigger_config'",
    )
    .fetch_one(&mut **conn)
    .await?;

    if !has_column {
        sqlx::query("ALTER TABLE rules ADD COLUMN trigger_config TEXT")
            .execute(&mut **conn)
            .await?;
        info!("Migration v4: added trigger_config column");
    } else {
        info!("Migration v4: trigger_config column already exists (skipped)");
    }

    Ok(())
}

/// v5: Move per-instance property values out of `instances.properties` JSON
/// column into a dedicated `instance_properties` table, then drop the column.
///
/// Old shape: each instance row had a `properties TEXT` column holding a
/// `{name: value}` JSON map. That made single-property writes require a
/// read-modify-write of the whole map (last-write-wins on concurrent edits)
/// and left no schema-level constraint on which keys are valid.
///
/// New shape: one row per (instance_id, property_id) in `instance_properties`
/// — mirrors `measurement_routing` / `action_routing`. Resolving legacy names
/// to Pack-owned numeric property IDs is a distribution migration. The generic
/// kernel performs only the structural drop after all legacy maps are empty.
async fn migrate_v5(conn: &mut sqlx::pool::PoolConnection<Sqlite>) -> Result<()> {
    info!("Migration v5: instance properties JSON column -> instance_properties table");

    // Wrap the entire migration in a transaction so a mid-flight crash leaves
    // no partial work behind. PRAGMA foreign_keys must be set outside any
    // transaction (SQLite no-ops it inside), so we toggle around the BEGIN.
    sqlx::query("PRAGMA foreign_keys=OFF")
        .execute(&mut **conn)
        .await?;
    sqlx::query("BEGIN IMMEDIATE").execute(&mut **conn).await?;

    // From here on, any `?` early-return propagates an error AFTER triggering
    // implicit rollback (sqlx wraps the connection state). We explicitly
    // COMMIT at the end if everything succeeded.

    // 1) Create the new table (idempotent — `init_database` also creates it
    //    on fresh installs, but a partial v4→v5 upgrade hits this first).
    sqlx::query(INSTANCE_PROPERTIES_TABLE)
        .execute(&mut **conn)
        .await?;

    // 2) Bail early if `instances` doesn't exist yet (very fresh DB before
    //    any DDL ran). Nothing to migrate.
    let has_instances: bool =
        sqlx::query_scalar("SELECT 1 FROM sqlite_master WHERE type='table' AND name='instances'")
            .fetch_optional(&mut **conn)
            .await?
            .unwrap_or(false);

    if !has_instances {
        info!("Migration v5: instances table missing, skipping data migration");
        // Must COMMIT before returning — the BEGIN IMMEDIATE above
        // started a transaction this connection still owns. A bare
        // `return Ok(())` would leave it open, and the next migration's
        // BEGIN IMMEDIATE would fail with "cannot start a transaction
        // within a transaction" (silently, since the runner reports
        // the error against the *next* migration). The
        // INSTANCE_PROPERTIES_TABLE created at step 1 above is the
        // only side effect to keep; COMMIT preserves it (it is also
        // re-created idempotently by init_database, so a ROLLBACK
        // would also be safe — COMMIT is the lower-surprise choice).
        sqlx::query("COMMIT").execute(&mut **conn).await?;
        sqlx::query("PRAGMA foreign_keys=ON")
            .execute(&mut **conn)
            .await?;
        return Ok(());
    }

    // 3) Check if the legacy `properties` column actually exists. Re-running
    //    migrate_v5 on a v5+ database (column already dropped) should no-op.
    let has_properties_col: bool = sqlx::query_scalar(
        "SELECT COUNT(*) > 0 FROM pragma_table_info('instances') WHERE name = 'properties'",
    )
    .fetch_one(&mut **conn)
    .await?;

    if !has_properties_col {
        info!("Migration v5: properties column already dropped, nothing to migrate");
        // See the COMMIT note in the !has_instances branch above —
        // the transaction must be closed before returning so the next
        // migration can BEGIN a fresh one.
        sqlx::query("COMMIT").execute(&mut **conn).await?;
        sqlx::query("PRAGMA foreign_keys=ON")
            .execute(&mut **conn)
            .await?;
        return Ok(());
    }

    // 4) A generic kernel cannot resolve domain property names to Pack-owned
    //    numeric templates. Refuse to drop non-empty legacy data. The owning
    //    distribution must first apply its versioned Pack migration asset.
    let legacy_property_rows: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM instances \
         WHERE properties IS NOT NULL AND TRIM(properties) NOT IN ('', '{}')",
    )
    .fetch_one(&mut **conn)
    .await?;
    if legacy_property_rows > 0 {
        sqlx::query("ROLLBACK").execute(&mut **conn).await?;
        sqlx::query("PRAGMA foreign_keys=ON")
            .execute(&mut **conn)
            .await?;
        anyhow::bail!(
            "{legacy_property_rows} instances still contain Pack-owned legacy properties; \
             apply the distribution's pre-v5 property migration before upgrading"
        );
    }

    // 5) Rebuild `instances` without the `properties` column. SQLite < 3.35
    //    cannot DROP COLUMN, and even on newer versions table rebuild keeps
    //    behaviour consistent across deployments.
    sqlx::query(
        "CREATE TABLE instances_new (
            instance_id INTEGER NOT NULL PRIMARY KEY,
            instance_name TEXT NOT NULL UNIQUE,
            product_name TEXT NOT NULL,
            parent_id INTEGER,
            created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
            updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
            FOREIGN KEY (parent_id) REFERENCES instances(instance_id) ON DELETE SET NULL
        )",
    )
    .execute(&mut **conn)
    .await?;

    sqlx::query(
        "INSERT INTO instances_new \
            (instance_id, instance_name, product_name, parent_id, created_at, updated_at) \
         SELECT instance_id, instance_name, product_name, parent_id, created_at, updated_at \
         FROM instances",
    )
    .execute(&mut **conn)
    .await?;

    sqlx::query("DROP TABLE instances")
        .execute(&mut **conn)
        .await?;
    sqlx::query("ALTER TABLE instances_new RENAME TO instances")
        .execute(&mut **conn)
        .await?;

    // Commit atomic block, then re-enable FK enforcement for this connection.
    sqlx::query("COMMIT").execute(&mut **conn).await?;
    sqlx::query("PRAGMA foreign_keys=ON")
        .execute(&mut **conn)
        .await?;

    info!("Migration v5: complete (properties column dropped)");
    Ok(())
}

/// v6: Structural integrity pass.
///
/// Rolls up several long-overdue fixes in one shot:
/// - `rules.id` gains AUTOINCREMENT so deleted ids are never reused
/// - `rule_history.rule_id` gains `ON DELETE CASCADE` (no more orphan history)
/// - `channel_templates.source_channel_id` gains FK to channels + an index
/// - Drops 2 unused indexes on `alert_rule` (description and created_at —
///   the former never matched equality, the latter rarely queried)
///
/// All work runs inside a single transaction with FK off; if anything fails
/// we leave the DB untouched.
async fn migrate_v6(conn: &mut sqlx::pool::PoolConnection<Sqlite>) -> Result<()> {
    info!("Migration v6: structural integrity pass");

    sqlx::query("PRAGMA foreign_keys=OFF")
        .execute(&mut **conn)
        .await?;
    sqlx::query("BEGIN IMMEDIATE").execute(&mut **conn).await?;

    // ── 1. Rebuild `rules` with AUTOINCREMENT ────────────────────────────
    //
    // Two guards are required. `rules_has_autoinc=false` is true both for
    // "old table without AUTOINCREMENT" (the case we want to migrate) AND
    // for "table does not exist yet" (fresh DB before init_database
    // creates the modern definition). The rebuild block does
    // `INSERT INTO rules_new SELECT FROM rules`, which fails on a fresh
    // DB with `no such table: rules`. Add `rules_exists` so we only
    // touch an existing legacy table, matching the pattern used by
    // sections 2 (rule_history) and 3 (channel_templates) below.
    let rules_exists: bool = sqlx::query_scalar(
        "SELECT COUNT(*) > 0 FROM sqlite_master WHERE type='table' AND name='rules'",
    )
    .fetch_one(&mut **conn)
    .await?;

    let rules_has_autoinc: bool = sqlx::query_scalar(
        "SELECT COUNT(*) > 0 FROM sqlite_master \
         WHERE type='table' AND name='rules' AND sql LIKE '%AUTOINCREMENT%'",
    )
    .fetch_one(&mut **conn)
    .await?;

    if rules_exists && !rules_has_autoinc {
        sqlx::query(
            "CREATE TABLE rules_new (
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
            )",
        )
        .execute(&mut **conn)
        .await?;
        sqlx::query(
            "INSERT INTO rules_new \
                (id, name, description, enabled, priority, cooldown_ms, trigger_config, \
                 nodes_json, flow_json, format, created_at, updated_at) \
             SELECT id, name, description, enabled, priority, cooldown_ms, trigger_config, \
                    nodes_json, flow_json, format, created_at, updated_at \
             FROM rules",
        )
        .execute(&mut **conn)
        .await?;
        // Seed sqlite_sequence so AUTOINCREMENT continues past the highest id.
        sqlx::query(
            "INSERT OR REPLACE INTO sqlite_sequence (name, seq) \
             SELECT 'rules_new', COALESCE(MAX(id), 0) FROM rules_new",
        )
        .execute(&mut **conn)
        .await?;
        sqlx::query("DROP TABLE rules").execute(&mut **conn).await?;
        sqlx::query("ALTER TABLE rules_new RENAME TO rules")
            .execute(&mut **conn)
            .await?;
        // Rename the sequence row too so it matches the renamed table.
        sqlx::query("UPDATE sqlite_sequence SET name='rules' WHERE name='rules_new'")
            .execute(&mut **conn)
            .await?;
        info!("Migration v6: rules table rebuilt with AUTOINCREMENT");
    }

    // ── 2. Rebuild `rule_history` with ON DELETE CASCADE ─────────────────
    let history_has_cascade: bool = sqlx::query_scalar(
        "SELECT COUNT(*) > 0 FROM sqlite_master \
         WHERE type='table' AND name='rule_history' AND sql LIKE '%ON DELETE CASCADE%'",
    )
    .fetch_one(&mut **conn)
    .await?;

    let history_exists: bool = sqlx::query_scalar(
        "SELECT COUNT(*) > 0 FROM sqlite_master WHERE type='table' AND name='rule_history'",
    )
    .fetch_one(&mut **conn)
    .await?;

    if history_exists && !history_has_cascade {
        // Clean orphaned history rows first — they would block the FK CHECK
        // once enforcement is enabled at the pool level (P1-1 follow-up).
        let orphans: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM rule_history \
             WHERE rule_id NOT IN (SELECT id FROM rules)",
        )
        .fetch_one(&mut **conn)
        .await?;
        if orphans > 0 {
            warn!(
                "Migration v6: deleting {} orphaned rule_history rows (no matching rule)",
                orphans
            );
            sqlx::query("DELETE FROM rule_history WHERE rule_id NOT IN (SELECT id FROM rules)")
                .execute(&mut **conn)
                .await?;
        }

        sqlx::query(
            "CREATE TABLE rule_history_new (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                rule_id INTEGER NOT NULL REFERENCES rules(id) ON DELETE CASCADE,
                triggered_at TEXT NOT NULL,
                execution_result TEXT,
                error TEXT
            )",
        )
        .execute(&mut **conn)
        .await?;
        sqlx::query(
            "INSERT INTO rule_history_new (id, rule_id, triggered_at, execution_result, error) \
             SELECT id, rule_id, triggered_at, execution_result, error FROM rule_history",
        )
        .execute(&mut **conn)
        .await?;
        sqlx::query("DROP TABLE rule_history")
            .execute(&mut **conn)
            .await?;
        sqlx::query("ALTER TABLE rule_history_new RENAME TO rule_history")
            .execute(&mut **conn)
            .await?;
        info!("Migration v6: rule_history rebuilt with ON DELETE CASCADE");
    }

    // ── 3. Rebuild `channel_templates` with FK + index ───────────────────
    let templates_has_fk: bool = sqlx::query_scalar(
        "SELECT COUNT(*) > 0 FROM sqlite_master \
         WHERE type='table' AND name='channel_templates' \
           AND sql LIKE '%REFERENCES channels%'",
    )
    .fetch_one(&mut **conn)
    .await?;

    let templates_exists: bool = sqlx::query_scalar(
        "SELECT COUNT(*) > 0 FROM sqlite_master \
         WHERE type='table' AND name='channel_templates'",
    )
    .fetch_one(&mut **conn)
    .await?;

    if templates_exists && !templates_has_fk {
        // Null out any source_channel_id that no longer points at a real
        // channel — once FK is declared, those rows would fail constraint.
        sqlx::query(
            "UPDATE channel_templates SET source_channel_id = NULL \
             WHERE source_channel_id IS NOT NULL \
               AND source_channel_id NOT IN (SELECT channel_id FROM channels)",
        )
        .execute(&mut **conn)
        .await?;

        sqlx::query(
            "CREATE TABLE channel_templates_new (
                template_id       INTEGER PRIMARY KEY AUTOINCREMENT,
                name              TEXT NOT NULL UNIQUE,
                description       TEXT,
                protocol          TEXT NOT NULL,
                points_snapshot   TEXT NOT NULL,
                mappings_snapshot TEXT NOT NULL,
                source_channel_id INTEGER REFERENCES channels(channel_id) ON DELETE SET NULL,
                created_at        TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
                updated_at        TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
            )",
        )
        .execute(&mut **conn)
        .await?;
        sqlx::query(
            "INSERT INTO channel_templates_new \
                (template_id, name, description, protocol, points_snapshot, \
                 mappings_snapshot, source_channel_id, created_at, updated_at) \
             SELECT template_id, name, description, protocol, points_snapshot, \
                    mappings_snapshot, source_channel_id, created_at, updated_at \
             FROM channel_templates",
        )
        .execute(&mut **conn)
        .await?;
        sqlx::query("DROP TABLE channel_templates")
            .execute(&mut **conn)
            .await?;
        sqlx::query("ALTER TABLE channel_templates_new RENAME TO channel_templates")
            .execute(&mut **conn)
            .await?;
        info!("Migration v6: channel_templates rebuilt with source_channel_id FK");
    }

    // (Re)create the index — cheap if it already exists. Gated on
    // `templates_exists` so a fresh DB (where `channel_templates`
    // hasn't been created by init_database yet) does not error on
    // CREATE INDEX against a missing table. The index is recreated
    // after init_database's CHANNEL_TEMPLATES_TABLE bootstrap via the
    // helper below; fresh DBs do not need this migration step to wire
    // it up.
    if templates_exists {
        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_channel_templates_source \
             ON channel_templates(source_channel_id)",
        )
        .execute(&mut **conn)
        .await?;
    }

    // ── 4. Drop unused alert_rule indexes ────────────────────────────────
    sqlx::query("DROP INDEX IF EXISTS idx_alert_rule_description")
        .execute(&mut **conn)
        .await?;
    sqlx::query("DROP INDEX IF EXISTS idx_alert_rule_created_at")
        .execute(&mut **conn)
        .await?;

    // Commit the whole block, then restore FK enforcement.
    sqlx::query("COMMIT").execute(&mut **conn).await?;
    sqlx::query("PRAGMA foreign_keys=ON")
        .execute(&mut **conn)
        .await?;

    info!("Migration v6: complete");
    Ok(())
}

struct PointTableMigration {
    table: &'static str,
    new_table: &'static str,
    legacy_backup_table: &'static str,
    create_sql: &'static str,
    copy_sql: &'static str,
}

const POINT_TABLE_MIGRATIONS: [PointTableMigration; 4] = [
    PointTableMigration {
        table: "telemetry_points",
        new_table: "telemetry_points_new",
        legacy_backup_table: "telemetry_points_backup",
        create_sql: r#"
            CREATE TABLE telemetry_points_new (
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
        "#,
        copy_sql: r#"
            INSERT INTO telemetry_points_new
                (point_id, channel_id, signal_name, scale, offset, unit, reverse,
                 data_type, description, protocol_mappings)
            SELECT point_id, channel_id, signal_name, scale, offset, unit, reverse,
                   data_type, description, protocol_mappings
            FROM telemetry_points
        "#,
    },
    PointTableMigration {
        table: "signal_points",
        new_table: "signal_points_new",
        legacy_backup_table: "signal_points_backup",
        create_sql: r#"
            CREATE TABLE signal_points_new (
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
        "#,
        copy_sql: r#"
            INSERT INTO signal_points_new
                (point_id, channel_id, signal_name, scale, offset, unit, reverse,
                 normal_state, data_type, description, protocol_mappings)
            SELECT point_id, channel_id, signal_name, scale, offset, unit, reverse,
                   normal_state, data_type, description, protocol_mappings
            FROM signal_points
        "#,
    },
    PointTableMigration {
        table: "control_points",
        new_table: "control_points_new",
        legacy_backup_table: "control_points_backup",
        create_sql: r#"
            CREATE TABLE control_points_new (
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
        "#,
        copy_sql: r#"
            INSERT INTO control_points_new
                (point_id, channel_id, signal_name, scale, offset, unit, reverse,
                 data_type, description, protocol_mappings)
            SELECT point_id, channel_id, signal_name, scale, offset, unit, reverse,
                   data_type, description, protocol_mappings
            FROM control_points
        "#,
    },
    PointTableMigration {
        table: "adjustment_points",
        new_table: "adjustment_points_new",
        legacy_backup_table: "adjustment_points_backup",
        create_sql: r#"
            CREATE TABLE adjustment_points_new (
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
        "#,
        copy_sql: r#"
            INSERT INTO adjustment_points_new
                (point_id, channel_id, signal_name, scale, offset, unit, reverse,
                 data_type, description, protocol_mappings)
            SELECT point_id, channel_id, signal_name, scale, offset, unit, reverse,
                   data_type, description, protocol_mappings
            FROM adjustment_points
        "#,
    },
];

/// v7: Add cascading channel ownership to every point table.
///
/// Older installers rebuilt these tables one process invocation at a time,
/// which exposed a window where live point data had been moved into ad-hoc
/// backup tables. This migration rebuilds all four tables under one
/// `BEGIN IMMEDIATE`, copies named columns only, and validates both row counts
/// and foreign keys before committing. Existing explicit indexes and triggers
/// are restored and verified in the same transaction. Stale `*_new` and legacy
/// installer `*_backup` tables are treated as errors; they are never
/// overwritten or adopted.
async fn migrate_v7(conn: &mut sqlx::pool::PoolConnection<Sqlite>) -> Result<()> {
    info!("Migration v7: point tables gain ON DELETE CASCADE");

    // Foreign-key enforcement is a connection setting and SQLite ignores
    // attempts to change it inside a transaction. Enable and verify it first.
    sqlx::query("PRAGMA foreign_keys=ON")
        .execute(&mut **conn)
        .await?;
    let foreign_keys_enabled: i64 = sqlx::query_scalar("PRAGMA foreign_keys")
        .fetch_one(&mut **conn)
        .await?;
    ensure!(
        foreign_keys_enabled == 1,
        "Migration v7 requires SQLite foreign key enforcement"
    );

    let mut transaction = begin_v7_immediate_transaction(conn).await?;
    let migration_result = migrate_v7_in_transaction(&mut transaction).await;

    match migration_result {
        Ok(()) => match transaction.commit().await {
            Ok(()) => {
                info!("Migration v7: complete");
                Ok(())
            },
            Err(commit_error) => {
                Err(commit_error).context("commit Migration v7 immediate transaction")
            },
        },
        Err(migration_error) => match transaction.rollback().await {
            Ok(()) => Err(migration_error),
            Err(rollback_error) => Err(anyhow!(
                "Migration v7 failed: {migration_error:#}; \
                     rollback also failed: {rollback_error}"
            )),
        },
    }
}

async fn begin_v7_immediate_transaction(
    conn: &mut SqliteConnection,
) -> Result<sqlx::Transaction<'_, Sqlite>> {
    conn.begin_with("BEGIN IMMEDIATE")
        .await
        .context("begin Migration v7 immediate transaction")
}

async fn migrate_v7_in_transaction(conn: &mut SqliteConnection) -> Result<()> {
    ensure_no_legacy_point_backups(conn).await?;

    let mut existing_tables = 0_usize;
    for migration in &POINT_TABLE_MIGRATIONS {
        if sqlite_table_exists(conn, migration.table).await? {
            existing_tables += 1;
        }
    }

    if existing_tables == 0 {
        ensure_no_stale_point_tables(conn).await?;
        info!("Migration v7: point tables not yet created, skipping rebuild");
        return Ok(());
    }
    if existing_tables != POINT_TABLE_MIGRATIONS.len() {
        ensure_no_stale_point_tables(conn).await?;
        bail!("Migration v7 requires all four live point tables; found {existing_tables}");
    }

    let mut all_have_cascade = true;
    for migration in &POINT_TABLE_MIGRATIONS {
        all_have_cascade &= point_table_has_cascade(conn, migration.table).await?;
    }
    if all_have_cascade {
        ensure_no_stale_point_tables(conn).await?;
        info!("Migration v7: point tables already use ON DELETE CASCADE");
        return Ok(());
    }

    // Any trigger that references a point table makes SQLite reparse that
    // dependency while the table is being rebuilt. Remove the canonical
    // integrity layer inside this same transaction and restore it after all
    // four identities are stable; rollback restores the original definitions
    // if any rebuild step fails.
    for trigger in LOGICAL_ROUTING_INTEGRITY_TRIGGER_NAMES {
        sqlx::query(&format!("DROP TRIGGER IF EXISTS {trigger}"))
            .execute(&mut *conn)
            .await?;
    }

    // Check each staging name immediately before it is used. If a later table
    // is stale, earlier rebuilds have already happened inside this transaction;
    // the outer rollback must restore every live table and its data.
    for migration in &POINT_TABLE_MIGRATIONS {
        if sqlite_table_exists(conn, migration.new_table).await? {
            bail!(
                "Migration v7 refuses stale staging table `{}`",
                migration.new_table
            );
        }

        let original_rows: i64 =
            sqlx::query_scalar(&format!("SELECT COUNT(*) FROM {}", migration.table))
                .fetch_one(&mut *conn)
                .await?;
        let schema_objects = point_table_schema_objects(conn, migration.table).await?;

        sqlx::query(migration.create_sql)
            .execute(&mut *conn)
            .await
            .with_context(|| format!("create {}", migration.new_table))?;
        ensure!(
            point_table_has_cascade(conn, migration.new_table).await?,
            "Migration v7 staging table `{}` has the wrong channel foreign key",
            migration.new_table
        );

        sqlx::query(migration.copy_sql)
            .execute(&mut *conn)
            .await
            .with_context(|| format!("copy named columns into {}", migration.new_table))?;

        let copied_rows: i64 =
            sqlx::query_scalar(&format!("SELECT COUNT(*) FROM {}", migration.new_table))
                .fetch_one(&mut *conn)
                .await?;
        ensure!(
            copied_rows == original_rows,
            "Migration v7 row-count mismatch for `{}`: expected {original_rows}, copied {copied_rows}",
            migration.table
        );
        ensure_point_foreign_keys_valid(conn, migration.new_table).await?;

        sqlx::query(&format!("DROP TABLE {}", migration.table))
            .execute(&mut *conn)
            .await?;
        sqlx::query(&format!(
            "ALTER TABLE {} RENAME TO {}",
            migration.new_table, migration.table
        ))
        .execute(&mut *conn)
        .await?;

        restore_point_table_schema_objects(conn, migration.table, &schema_objects).await?;

        ensure!(
            point_table_has_cascade(conn, migration.table).await?,
            "Migration v7 live table `{}` lost its cascading channel foreign key",
            migration.table
        );
        ensure_point_foreign_keys_valid(conn, migration.table).await?;
    }

    for trigger in LOGICAL_ROUTING_INTEGRITY_TRIGGERS {
        sqlx::query(trigger).execute(&mut *conn).await?;
    }

    Ok(())
}

/// v8: Persist safety constraints for writable adjustment points.
///
/// The runtime model has always carried these fields, but the SQLite schema
/// discarded them. The additive migration has no destructive window and is a
/// no-op during first-install migration (the tables are created afterwards
/// from the current DDL).
async fn migrate_v8(conn: &mut sqlx::pool::PoolConnection<Sqlite>) -> Result<()> {
    if !sqlite_table_exists(conn, "adjustment_points").await? {
        info!("Migration v8: adjustment_points not yet created, skipping ALTER");
        return Ok(());
    }

    let mut transaction = conn.begin().await?;
    for (column, definition) in [
        ("min_value", "REAL"),
        ("max_value", "REAL"),
        ("step", "REAL DEFAULT 1.0"),
    ] {
        let exists = sqlx::query_scalar::<_, i64>(
            "SELECT 1 FROM pragma_table_info('adjustment_points') WHERE name = ?",
        )
        .bind(column)
        .fetch_optional(&mut *transaction)
        .await?
        .is_some();
        if !exists {
            sqlx::query(&format!(
                "ALTER TABLE adjustment_points ADD COLUMN {column} {definition}"
            ))
            .execute(&mut *transaction)
            .await?;
        }
    }
    transaction.commit().await?;
    info!("Migration v8: adjustment point safety constraints persisted");
    Ok(())
}

/// v9: Add optimistic concurrency to authoritative channel configuration.
///
/// The trigger keeps legacy sync/import writers safe: an update that leaves
/// `revision` unchanged receives exactly one automatic increment. Formal CAS
/// writers set the next revision explicitly, so the trigger does not fire.
async fn migrate_v9(conn: &mut sqlx::pool::PoolConnection<Sqlite>) -> Result<()> {
    if !sqlite_table_exists(conn, "channels").await? {
        info!("Migration v9: channels not yet created, deferring to current DDL");
        return Ok(());
    }

    let mut transaction = conn.begin().await?;
    let revision_exists = sqlx::query_scalar::<_, i64>(
        "SELECT 1 FROM pragma_table_info('channels') WHERE name = 'revision'",
    )
    .fetch_optional(&mut *transaction)
    .await?
    .is_some();
    if !revision_exists {
        sqlx::query(
            "ALTER TABLE channels ADD COLUMN revision INTEGER NOT NULL DEFAULT 1 \
             CHECK (TYPEOF(revision) = 'integer' AND revision >= 1)",
        )
        .execute(&mut *transaction)
        .await?;
    }
    sqlx::query(CHANNEL_REVISION_EXHAUSTED_TRIGGER)
        .execute(&mut *transaction)
        .await?;
    sqlx::query(CHANNEL_REVISION_BUMP_TRIGGER)
        .execute(&mut *transaction)
        .await?;
    transaction.commit().await?;
    info!("Migration v9: channel revision CAS and compatibility trigger installed");
    Ok(())
}

/// v10: Retain a revision high-water mark across channel deletion.
///
/// This prevents a stale compare-and-set token for a deleted entity from
/// matching a later entity that uses the same explicit channel identity.
/// Legacy inserts are advanced beyond the tombstone by a compatibility
/// trigger; formal writers supply that revision directly.
async fn migrate_v10(conn: &mut sqlx::pool::PoolConnection<Sqlite>) -> Result<()> {
    if !sqlite_table_exists(conn, "channels").await? {
        info!("Migration v10: channels not yet created, deferring to current DDL");
        return Ok(());
    }

    let mut transaction = conn.begin().await?;
    let revision_exists = sqlx::query_scalar::<_, i64>(
        "SELECT 1 FROM pragma_table_info('channels') WHERE name = 'revision'",
    )
    .fetch_optional(&mut *transaction)
    .await?
    .is_some();
    ensure!(
        revision_exists,
        "Migration v10 requires the v9 channels.revision column"
    );

    let has_exhausted_revision: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM channels WHERE revision >= 9223372036854775807)",
    )
    .fetch_one(&mut *transaction)
    .await?;
    ensure!(
        !has_exhausted_revision,
        "Migration v10 cannot invalidate an exhausted channel revision"
    );
    // Invalidate every token issued before the tombstone generation existed.
    // This closes the only recoverable v9 delete/recreate ABA case for live
    // rows; deleted identities absent from the v9 schema had no durable fact.
    sqlx::query("UPDATE channels SET revision = revision + 1")
        .execute(&mut *transaction)
        .await?;

    for statement in [
        CHANNEL_REVISION_TOMBSTONES_TABLE,
        CHANNEL_REVISION_INSERT_GUARD_TRIGGER,
        CHANNEL_REVISION_INSERT_ADVANCE_TRIGGER,
        CHANNEL_REVISION_DELETE_EXHAUSTED_TRIGGER,
        CHANNEL_REVISION_DELETE_TOMBSTONE_TRIGGER,
    ] {
        sqlx::query(statement).execute(&mut *transaction).await?;
    }
    transaction.commit().await?;
    info!("Migration v10: channel revision tombstone and ABA guards installed");
    Ok(())
}

/// v11: Add one shared CAS head for the complete logical-routing aggregate.
async fn migrate_v11(conn: &mut sqlx::pool::PoolConnection<Sqlite>) -> Result<()> {
    let mut transaction = conn.begin().await?;
    sqlx::query(CONFIGURATION_REVISIONS_TABLE)
        .execute(&mut *transaction)
        .await?;
    sqlx::query(
        "INSERT INTO configuration_revisions (scope, revision) \
         VALUES ('logical_routing', 1) ON CONFLICT(scope) DO NOTHING",
    )
    .execute(&mut *transaction)
    .await?;
    transaction.commit().await?;
    info!("Migration v11: logical-routing CAS head installed");
    Ok(())
}

#[derive(Debug, sqlx::FromRow)]
struct LegacyJsonPointMappingRow {
    id: i64,
    channel_id: i64,
    point_id: i64,
    point_type: String,
    json_path: String,
    data_type: Option<String>,
    scale: Option<f64>,
    offset: Option<f64>,
    description: Option<String>,
}

/// v12: Merge the legacy MQTT/HTTP mapping table into point-owned inline JSON.
///
/// The migration takes an immediate write transaction, validates every legacy
/// row and any pre-existing inline value, then drops the old table only after
/// the complete generation is represented in the four point tables.
async fn migrate_v12(conn: &mut sqlx::pool::PoolConnection<Sqlite>) -> Result<()> {
    if !sqlite_table_exists(conn, "json_point_mappings").await? {
        info!("Migration v12: no legacy JSON mapping table present");
        return Ok(());
    }

    let mut transaction = conn
        .begin_with("BEGIN IMMEDIATE")
        .await
        .context("begin Migration v12 immediate transaction")?;
    let migration_result = migrate_v12_in_transaction(&mut transaction).await;
    match migration_result {
        Ok(()) => {
            transaction.commit().await?;
            info!("Migration v12: inline JSON mapping authority installed");
            Ok(())
        },
        Err(error) => match transaction.rollback().await {
            Ok(()) => Err(error),
            Err(rollback_error) => Err(anyhow!(
                "Migration v12 failed: {error:#}; rollback also failed: {rollback_error}"
            )),
        },
    }
}

async fn migrate_v12_in_transaction(conn: &mut SqliteConnection) -> Result<()> {
    let rows = sqlx::query_as::<_, LegacyJsonPointMappingRow>(
        "SELECT id, channel_id, point_id, point_type, json_path, data_type, \
                scale, offset, description \
         FROM json_point_mappings ORDER BY id",
    )
    .fetch_all(&mut *conn)
    .await?;

    for row in rows {
        migrate_legacy_json_mapping(conn, &row).await?;
    }

    sqlx::query("DROP TABLE json_point_mappings")
        .execute(&mut *conn)
        .await?;
    Ok(())
}

async fn migrate_legacy_json_mapping(
    conn: &mut SqliteConnection,
    row: &LegacyJsonPointMappingRow,
) -> Result<()> {
    ensure!(
        u32::try_from(row.channel_id).is_ok(),
        "legacy JSON mapping row {} has channel_id outside u32",
        row.id
    );
    ensure!(
        u32::try_from(row.point_id).is_ok(),
        "legacy JSON mapping row {} has point_id outside u32",
        row.id
    );
    let protocol: String = sqlx::query_scalar("SELECT protocol FROM channels WHERE channel_id = ?")
        .bind(row.channel_id)
        .fetch_optional(&mut *conn)
        .await?
        .with_context(|| {
            format!(
                "legacy JSON mapping row {} references missing channel {}",
                row.id, row.channel_id
            )
        })?;
    ensure!(
        matches!(
            protocol.trim().to_ascii_lowercase().as_str(),
            "mqtt" | "http"
        ),
        "legacy JSON mapping row {} targets non-JSON protocol `{protocol}`",
        row.id
    );

    let point_table = match row.point_type.trim().to_ascii_uppercase().as_str() {
        "T" => "telemetry_points",
        "S" => "signal_points",
        "C" => "control_points",
        "A" => "adjustment_points",
        other => bail!(
            "legacy JSON mapping row {} has invalid point_type `{other}`",
            row.id
        ),
    };
    let existing_row: Option<Option<String>> = sqlx::query_scalar::<_, Option<String>>(&format!(
        "SELECT protocol_mappings FROM {point_table} WHERE channel_id = ? AND point_id = ?"
    ))
    .bind(row.channel_id)
    .bind(row.point_id)
    .fetch_optional(&mut *conn)
    .await?;
    let existing = existing_row.with_context(|| {
        format!(
            "legacy JSON mapping row {} references missing {} point {}:{}",
            row.id, row.point_type, row.channel_id, row.point_id
        )
    })?;

    let canonical = canonical_legacy_json_mapping(row)?;
    if let Some(existing) = existing.as_deref()
        && !existing.trim().is_empty()
    {
        let existing_value: serde_json::Value =
            serde_json::from_str(existing).with_context(|| {
                format!(
                    "legacy JSON mapping row {} conflicts with invalid inline JSON",
                    row.id
                )
            })?;
        if !existing_value.is_null()
            && !existing_value
                .as_object()
                .is_some_and(serde_json::Map::is_empty)
        {
            let normalized_existing = canonical_inline_json_mapping(&existing_value)
                .with_context(|| format!("validate inline JSON for legacy row {}", row.id))?;
            ensure!(
                normalized_existing == canonical,
                "legacy JSON mapping row {} conflicts with non-equivalent inline protocol_mappings",
                row.id
            );
        }
    }

    let serialized = serde_json::to_string(&canonical)?;
    let updated = sqlx::query(&format!(
        "UPDATE {point_table} SET protocol_mappings = ? WHERE channel_id = ? AND point_id = ?"
    ))
    .bind(serialized)
    .bind(row.channel_id)
    .bind(row.point_id)
    .execute(&mut *conn)
    .await?;
    ensure!(
        updated.rows_affected() == 1,
        "legacy JSON mapping row {} was not durably represented inline",
        row.id
    );
    Ok(())
}

fn canonical_legacy_json_mapping(row: &LegacyJsonPointMappingRow) -> Result<serde_json::Value> {
    let data_type = canonical_json_mapping_data_type(row.data_type.as_deref())
        .with_context(|| format!("legacy JSON mapping row {} has invalid data_type", row.id))?;
    let scale = finite_legacy_mapping_number(row.scale.unwrap_or(1.0), row.id, "scale")?;
    let offset = finite_legacy_mapping_number(row.offset.unwrap_or(0.0), row.id, "offset")?;
    ensure!(
        !row.json_path.trim().is_empty(),
        "legacy JSON mapping row {} has blank json_path",
        row.id
    );
    serde_json_path::JsonPath::parse(&row.json_path).with_context(|| {
        format!(
            "legacy JSON mapping row {} has invalid JSONPath `{}`",
            row.id, row.json_path
        )
    })?;

    let mut mapping = serde_json::Map::new();
    mapping.insert("json_path".to_string(), row.json_path.clone().into());
    mapping.insert("data_type".to_string(), data_type.into());
    mapping.insert("scale".to_string(), scale.into());
    mapping.insert("offset".to_string(), offset.into());
    if let Some(description) = &row.description {
        mapping.insert("description".to_string(), description.clone().into());
    }
    Ok(mapping.into())
}

fn canonical_inline_json_mapping(value: &serde_json::Value) -> Result<serde_json::Value> {
    let values = value
        .as_object()
        .context("inline JSON mapping must be an object")?;
    for field in values.keys() {
        ensure!(
            matches!(
                field.as_str(),
                "json_path" | "data_type" | "scale" | "offset" | "description"
            ),
            "unsupported inline JSON mapping field `{field}`"
        );
    }
    let json_path = values
        .get("json_path")
        .and_then(serde_json::Value::as_str)
        .filter(|path| !path.trim().is_empty())
        .context("inline json_path must be a nonblank string")?;
    serde_json_path::JsonPath::parse(json_path)
        .with_context(|| format!("invalid inline JSONPath `{json_path}`"))?;
    let data_type = match values.get("data_type") {
        None => canonical_json_mapping_data_type(None)?,
        Some(value) => canonical_json_mapping_data_type(Some(
            value
                .as_str()
                .context("inline data_type must be a string")?,
        ))?,
    };
    let scale = inline_finite_mapping_number(values, "scale", 1.0)?;
    let offset = inline_finite_mapping_number(values, "offset", 0.0)?;
    let description = values
        .get("description")
        .map(|value| {
            value
                .as_str()
                .context("inline description must be a string")
        })
        .transpose()?;

    let mut canonical = serde_json::Map::new();
    canonical.insert("json_path".to_string(), json_path.into());
    canonical.insert("data_type".to_string(), data_type.into());
    canonical.insert("scale".to_string(), scale.into());
    canonical.insert("offset".to_string(), offset.into());
    if let Some(description) = description {
        canonical.insert("description".to_string(), description.into());
    }
    Ok(canonical.into())
}

fn canonical_json_mapping_data_type(value: Option<&str>) -> Result<&'static str> {
    match value.unwrap_or("float") {
        "float" => Ok("float"),
        "int" | "integer" => Ok("int"),
        "bool" | "boolean" => Ok("bool"),
        "string" | "str" => Ok("string"),
        other => bail!("unsupported JSON mapping data_type `{other}`"),
    }
}

fn finite_legacy_mapping_number(value: f64, row_id: i64, field: &str) -> Result<f64> {
    ensure!(
        value.is_finite(),
        "legacy JSON mapping row {row_id} has non-finite {field}"
    );
    Ok(value)
}

fn inline_finite_mapping_number(
    values: &serde_json::Map<String, serde_json::Value>,
    field: &str,
    default: f64,
) -> Result<f64> {
    let Some(value) = values.get(field) else {
        return Ok(default);
    };
    let value = value
        .as_f64()
        .with_context(|| format!("inline {field} must be a number"))?;
    ensure!(value.is_finite(), "inline {field} must be finite");
    Ok(value)
}

async fn sqlite_table_exists(conn: &mut SqliteConnection, table: &str) -> Result<bool> {
    Ok(
        sqlx::query_scalar("SELECT COUNT(*) > 0 FROM sqlite_master WHERE type='table' AND name=?")
            .bind(table)
            .fetch_one(&mut *conn)
            .await?,
    )
}

async fn point_table_has_cascade(conn: &mut SqliteConnection, table: &str) -> Result<bool> {
    let matching_foreign_keys: i64 = sqlx::query_scalar(&format!(
        "SELECT COUNT(*) FROM pragma_foreign_key_list('{table}') \
         WHERE \"table\" = 'channels' \
           AND \"from\" = 'channel_id' \
           AND \"to\" = 'channel_id' \
           AND UPPER(on_delete) = 'CASCADE'"
    ))
    .fetch_one(&mut *conn)
    .await?;
    Ok(matching_foreign_keys == 1)
}

async fn ensure_point_foreign_keys_valid(conn: &mut SqliteConnection, table: &str) -> Result<()> {
    let violation = sqlx::query(&format!("PRAGMA foreign_key_check('{table}')"))
        .fetch_optional(&mut *conn)
        .await?;
    ensure!(
        violation.is_none(),
        "Migration v7 foreign-key validation failed for `{table}`"
    );
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PointTableSchemaObject {
    object_type: String,
    name: String,
    sql: String,
}

async fn point_table_schema_objects(
    conn: &mut SqliteConnection,
    table: &str,
) -> Result<Vec<PointTableSchemaObject>> {
    let rows: Vec<(String, String, String)> = sqlx::query_as(
        "SELECT type, name, sql FROM sqlite_master \
         WHERE tbl_name = ? AND type IN ('index', 'trigger') AND sql IS NOT NULL \
         ORDER BY type, name",
    )
    .bind(table)
    .fetch_all(&mut *conn)
    .await?;

    Ok(rows
        .into_iter()
        .map(|(object_type, name, sql)| PointTableSchemaObject {
            object_type,
            name,
            sql,
        })
        .collect())
}

async fn restore_point_table_schema_objects(
    conn: &mut SqliteConnection,
    table: &str,
    schema_objects: &[PointTableSchemaObject],
) -> Result<()> {
    for object in schema_objects {
        sqlx::query(&object.sql)
            .execute(&mut *conn)
            .await
            .with_context(|| {
                format!(
                    "restore {} `{}` for point table `{table}`",
                    object.object_type, object.name
                )
            })?;
    }

    let restored = point_table_schema_objects(conn, table).await?;
    ensure!(
        restored == schema_objects,
        "Migration v7 did not exactly restore indexes/triggers for `{table}`"
    );
    Ok(())
}

async fn ensure_no_stale_point_tables(conn: &mut SqliteConnection) -> Result<()> {
    for migration in &POINT_TABLE_MIGRATIONS {
        if sqlite_table_exists(conn, migration.new_table).await? {
            bail!(
                "Migration v7 refuses stale staging table `{}`",
                migration.new_table
            );
        }
    }
    Ok(())
}

async fn ensure_no_legacy_point_backups(conn: &mut SqliteConnection) -> Result<()> {
    for migration in &POINT_TABLE_MIGRATIONS {
        if sqlite_table_exists(conn, migration.legacy_backup_table).await? {
            bail!(
                "Migration v7 refuses legacy installer backup table `{}`; \
                 recover its data before retrying",
                migration.legacy_backup_table
            );
        }
    }
    Ok(())
}

/// Initialize all database tables in aether.db
///
/// Creates all tables, indexes, and triggers needed by Aether services.
/// This is a unified initialization that replaces the old per-service approach.
///
/// @input db_path: `impl AsRef<Path>` - Path to SQLite database file
/// @output `Result<()>` - Success or initialization error
/// @throws anyhow::Error - Database connection or schema creation failure
/// @side-effects Creates database file if not exists, creates all tables/indexes/triggers
pub async fn init_database(db_path: impl AsRef<Path>) -> Result<()> {
    let db_path = db_path.as_ref();

    // Ensure data directory exists
    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    // Connect to database with shared options (foreign_keys=ON, WAL, create_if_missing)
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .connect_with(common::bootstrap_database::sqlite_connect_options(
            db_path.to_str().unwrap_or_default(),
        ))
        .await
        .with_context(|| "Failed to connect to database")?;

    // Set file permissions for Docker compatibility
    file_utils::set_database_permissions(db_path)?;

    // Run versioned schema migrations (PRAGMA user_version based).
    // The legacy "id TEXT" rules-table rebuild is now `migrate_v0`, gated on
    // `current < 1` — there is no separate post-migration step any more.
    run_migrations(&pool).await?;

    // === Shared tables ===
    sqlx::query(SERVICE_CONFIG_TABLE).execute(&pool).await?;
    sqlx::query(SYNC_METADATA_TABLE).execute(&pool).await?;
    sqlx::query(CONFIGURATION_REVISIONS_TABLE)
        .execute(&pool)
        .await?;
    sqlx::query(
        "INSERT INTO configuration_revisions (scope, revision) \
         VALUES ('logical_routing', 1) ON CONFLICT(scope) DO NOTHING",
    )
    .execute(&pool)
    .await?;

    // === Channel & Point tables ===
    sqlx::query(CHANNELS_TABLE).execute(&pool).await?;
    sqlx::query(CHANNEL_REVISION_TOMBSTONES_TABLE)
        .execute(&pool)
        .await?;
    sqlx::query(TELEMETRY_POINTS_TABLE).execute(&pool).await?;
    sqlx::query(SIGNAL_POINTS_TABLE).execute(&pool).await?;
    sqlx::query(CONTROL_POINTS_TABLE).execute(&pool).await?;
    sqlx::query(ADJUSTMENT_POINTS_TABLE).execute(&pool).await?;

    // === Channel templates table ===
    sqlx::query(CHANNEL_TEMPLATES_TABLE).execute(&pool).await?;

    // === Instance tables ===
    sqlx::query(INSTANCES_TABLE).execute(&pool).await?;
    sqlx::query(MEASUREMENT_ROUTING_TABLE)
        .execute(&pool)
        .await?;
    sqlx::query(ACTION_ROUTING_TABLE).execute(&pool).await?;
    sqlx::query(INSTANCE_PROPERTIES_TABLE)
        .execute(&pool)
        .await?;

    // === Rule tables (rules engine) ===
    sqlx::query(RULE_CHAINS_TABLE).execute(&pool).await?;
    sqlx::query(RULE_HISTORY_TABLE).execute(&pool).await?;

    // === Indexes ===
    create_indexes(&pool).await?;

    // === Triggers ===
    create_triggers(&pool).await?;

    info!("DB init: {}", db_path.display());
    Ok(())
}

/// Create all database indexes
async fn create_indexes(pool: &SqlitePool) -> Result<()> {
    // Point tables indexes
    sqlx::query(
        "CREATE INDEX IF NOT EXISTS idx_telemetry_points_channel ON telemetry_points(channel_id)",
    )
    .execute(pool)
    .await?;
    sqlx::query(
        "CREATE INDEX IF NOT EXISTS idx_signal_points_channel ON signal_points(channel_id)",
    )
    .execute(pool)
    .await?;
    sqlx::query(
        "CREATE INDEX IF NOT EXISTS idx_control_points_channel ON control_points(channel_id)",
    )
    .execute(pool)
    .await?;
    sqlx::query(
        "CREATE INDEX IF NOT EXISTS idx_adjustment_points_channel ON adjustment_points(channel_id)",
    )
    .execute(pool)
    .await?;

    // Channel templates index for source_channel_id lookups (added in v6)
    sqlx::query(
        "CREATE INDEX IF NOT EXISTS idx_channel_templates_source ON channel_templates(source_channel_id)",
    )
    .execute(pool)
    .await?;

    // Instance routing indexes
    sqlx::query(
        "CREATE INDEX IF NOT EXISTS idx_measurement_routing_instance ON measurement_routing(instance_id)",
    )
    .execute(pool)
    .await?;
    sqlx::query(
        "CREATE INDEX IF NOT EXISTS idx_action_routing_instance ON action_routing(instance_id)",
    )
    .execute(pool)
    .await?;

    // Rule indexes
    sqlx::query("CREATE INDEX IF NOT EXISTS idx_rules_enabled ON rules(enabled)")
        .execute(pool)
        .await?;
    sqlx::query("CREATE INDEX IF NOT EXISTS idx_rule_history_rule ON rule_history(rule_id)")
        .execute(pool)
        .await?;
    sqlx::query("CREATE INDEX IF NOT EXISTS idx_rule_history_time ON rule_history(triggered_at)")
        .execute(pool)
        .await?;

    Ok(())
}

/// Create routing cleanup and governance triggers.
async fn create_triggers(pool: &SqlitePool) -> Result<()> {
    // Keep this helper self-contained: SQLite accepts a trigger that names a
    // missing table, but later schema rebuilds can then fail while reparsing
    // that invalid trigger. This also makes legacy migration fixtures safe
    // when they install the current trigger set before running migrations.
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

    // Validation reads and trigger replacement must share one write-intent
    // transaction. A deferred transaction lets concurrent initializers all
    // acquire read locks and then fail while upgrading to a writer; IMMEDIATE
    // serializes that upgrade through SQLite's configured busy timeout.
    let mut transaction = pool
        .begin_with("BEGIN IMMEDIATE")
        .await
        .context("begin logical-routing trigger transaction")?;
    validate_existing_logical_routing(&mut transaction).await?;

    // Legacy point cleanup triggers silently changed the logical-routing
    // aggregate without its CAS/audit boundary. Remove them even from an
    // already initialized database; governed point deletion now rejects a
    // referenced T/S point until routing authority removes the route.
    for trigger in [
        "cleanup_routing_on_telemetry_delete",
        "cleanup_routing_on_signal_delete",
        "cleanup_routing_on_control_delete",
        "cleanup_routing_on_adjustment_delete",
    ]
    .into_iter()
    .chain(LOGICAL_ROUTING_INTEGRITY_TRIGGER_NAMES.iter().copied())
    {
        sqlx::query(&format!("DROP TRIGGER IF EXISTS {trigger}"))
            .execute(&mut *transaction)
            .await?;
    }

    // Routing is a separate governed aggregate. Direct SQL may not create a
    // dangling/mistyped target or let parent/point foreign-key actions mutate
    // routing without its CAS and audit boundary.
    for trigger in LOGICAL_ROUTING_INTEGRITY_TRIGGERS {
        sqlx::query(trigger).execute(&mut *transaction).await?;
    }

    transaction.commit().await?;

    Ok(())
}

async fn validate_existing_logical_routing(
    transaction: &mut sqlx::Transaction<'_, Sqlite>,
) -> Result<()> {
    let invalid_measurement_routes: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM measurement_routing AS route \
         WHERE NOT EXISTS (\
                   SELECT 1 FROM instances \
                   WHERE instance_id = route.instance_id \
                     AND instance_name = route.instance_name\
               ) \
            OR route.channel_id IS NULL \
            OR route.channel_type IS NULL \
            OR route.channel_point_id IS NULL \
            OR (route.channel_type = 'T' AND NOT EXISTS (\
                   SELECT 1 FROM telemetry_points \
                   WHERE channel_id = route.channel_id \
                     AND point_id = route.channel_point_id\
               )) \
            OR (route.channel_type = 'S' AND NOT EXISTS (\
                   SELECT 1 FROM signal_points \
                   WHERE channel_id = route.channel_id \
                     AND point_id = route.channel_point_id\
               )) \
            OR route.channel_type NOT IN ('T', 'S')",
    )
    .fetch_one(&mut **transaction)
    .await?;
    ensure!(
        invalid_measurement_routes == 0,
        "logical-routing integrity validation found {invalid_measurement_routes} invalid measurement route(s); repair them through the governed measurement-routing command before initialization"
    );

    let invalid_action_routes: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM action_routing AS route \
         WHERE NOT EXISTS (\
                   SELECT 1 FROM instances \
                   WHERE instance_id = route.instance_id \
                     AND instance_name = route.instance_name\
               ) \
            OR route.channel_id IS NULL \
            OR route.channel_type IS NULL \
            OR route.channel_point_id IS NULL \
            OR (route.channel_type = 'C' AND NOT EXISTS (\
                   SELECT 1 FROM control_points \
                   WHERE channel_id = route.channel_id \
                     AND point_id = route.channel_point_id\
               )) \
            OR (route.channel_type = 'A' AND NOT EXISTS (\
                   SELECT 1 FROM adjustment_points \
                   WHERE channel_id = route.channel_id \
                     AND point_id = route.channel_point_id\
               )) \
            OR route.channel_type NOT IN ('C', 'A')",
    )
    .fetch_one(&mut **transaction)
    .await?;
    ensure!(
        invalid_action_routes == 0,
        "logical-routing integrity validation found {invalid_action_routes} invalid action route(s); repair them through the governed action-routing command before initialization"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use anyhow::Context as _;
    use sqlx::sqlite::SqlitePoolOptions;
    use tempfile::TempDir;

    use super::*;

    const LEGACY_JSON_POINT_MAPPINGS_TABLE: &str = r#"
        CREATE TABLE json_point_mappings (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            channel_id INTEGER NOT NULL REFERENCES channels(channel_id) ON DELETE CASCADE,
            point_id INTEGER NOT NULL,
            point_type TEXT NOT NULL,
            json_path TEXT NOT NULL,
            data_type TEXT DEFAULT 'float',
            scale REAL DEFAULT 1.0,
            offset REAL DEFAULT 0.0,
            description TEXT,
            UNIQUE(channel_id, point_id, point_type)
        )
    "#;

    async fn legacy_json_mapping_pool() -> Result<SqlitePool> {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await?;
        sqlx::query(CHANNELS_TABLE).execute(&pool).await?;
        for ddl in [
            TELEMETRY_POINTS_TABLE,
            SIGNAL_POINTS_TABLE,
            CONTROL_POINTS_TABLE,
            ADJUSTMENT_POINTS_TABLE,
        ] {
            sqlx::query(ddl).execute(&pool).await?;
        }
        sqlx::query(LEGACY_JSON_POINT_MAPPINGS_TABLE)
            .execute(&pool)
            .await?;
        sqlx::query("PRAGMA user_version = 11")
            .execute(&pool)
            .await?;
        Ok(pool)
    }

    #[tokio::test]
    async fn migrate_v12_atomically_moves_every_json_mapping_inline_and_drops_legacy_table()
    -> Result<()> {
        let pool = legacy_json_mapping_pool().await?;
        sqlx::query(
            "INSERT INTO channels (channel_id, name, protocol, enabled, config) \
             VALUES (7, 'mqtt-channel', 'mqtt', 0, '{}')",
        )
        .execute(&pool)
        .await?;
        for (table, point_id) in [
            ("telemetry_points", 1_i64),
            ("signal_points", 2),
            ("control_points", 3),
            ("adjustment_points", 4),
        ] {
            sqlx::query(&format!(
                "INSERT INTO {table} (channel_id, point_id, signal_name) \
                 VALUES (7, ?, 'legacy')"
            ))
            .bind(point_id)
            .execute(&pool)
            .await?;
        }
        sqlx::query(
            "INSERT INTO json_point_mappings \
             (channel_id, point_id, point_type, json_path, data_type, scale, offset, description) \
             VALUES (7, 1, 'T', '$.telemetry', 'float', 0.1, -1.0, 'power'), \
                    (7, 2, 'S', '$.signal', 'boolean', 1.0, 0.0, NULL), \
                    (7, 3, 'C', '$.control', 'integer', 2.0, 1.0, NULL), \
                    (7, 4, 'A', '$.adjustment', 'str', 1.0, 0.0, NULL)",
        )
        .execute(&pool)
        .await?;

        run_migrations(&pool).await?;

        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM sqlite_master \
                 WHERE type = 'table' AND name = 'json_point_mappings'",
            )
            .fetch_one(&pool)
            .await?,
            0
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>("PRAGMA user_version")
                .fetch_one(&pool)
                .await?,
            12
        );
        let telemetry: String = sqlx::query_scalar(
            "SELECT protocol_mappings FROM telemetry_points \
             WHERE channel_id = 7 AND point_id = 1",
        )
        .fetch_one(&pool)
        .await?;
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&telemetry)?,
            serde_json::json!({
                "json_path": "$.telemetry",
                "data_type": "float",
                "scale": 0.1,
                "offset": -1.0,
                "description": "power"
            })
        );
        for (table, point_id, expected_type) in [
            ("signal_points", 2_i64, "bool"),
            ("control_points", 3, "int"),
            ("adjustment_points", 4, "string"),
        ] {
            let mapping: String = sqlx::query_scalar(&format!(
                "SELECT protocol_mappings FROM {table} \
                 WHERE channel_id = 7 AND point_id = ?"
            ))
            .bind(point_id)
            .fetch_one(&pool)
            .await?;
            assert_eq!(
                serde_json::from_str::<serde_json::Value>(&mapping)?["data_type"],
                expected_type
            );
        }
        Ok(())
    }

    #[tokio::test]
    async fn migrate_v12_rolls_back_all_rows_on_non_equivalent_inline_conflict() -> Result<()> {
        let pool = legacy_json_mapping_pool().await?;
        sqlx::query(
            "INSERT INTO channels (channel_id, name, protocol, enabled, config) \
             VALUES (7, 'http-channel', 'http', 0, '{}')",
        )
        .execute(&pool)
        .await?;
        sqlx::query(
            "INSERT INTO telemetry_points \
             (channel_id, point_id, signal_name, protocol_mappings) \
             VALUES (7, 1, 'first', NULL), \
                    (7, 2, 'conflict', '{\"json_path\":\"$.new\"}')",
        )
        .execute(&pool)
        .await?;
        sqlx::query(
            "INSERT INTO json_point_mappings \
             (channel_id, point_id, point_type, json_path) \
             VALUES (7, 1, 'T', '$.first'), (7, 2, 'T', '$.old')",
        )
        .execute(&pool)
        .await?;

        let error = run_migrations(&pool)
            .await
            .expect_err("conflicting authority must fail closed");

        assert!(format!("{error:#}").contains("non-equivalent inline"));
        assert_eq!(
            sqlx::query_scalar::<_, i64>("PRAGMA user_version")
                .fetch_one(&pool)
                .await?,
            11
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM json_point_mappings")
                .fetch_one(&pool)
                .await?,
            2
        );
        let first: Option<String> = sqlx::query_scalar(
            "SELECT protocol_mappings FROM telemetry_points \
             WHERE channel_id = 7 AND point_id = 1",
        )
        .fetch_one(&pool)
        .await?;
        assert!(first.is_none(), "earlier row must roll back with conflict");
        Ok(())
    }

    #[tokio::test]
    async fn migrate_v12_rejects_non_json_protocol_without_dropping_legacy_rows() -> Result<()> {
        let pool = legacy_json_mapping_pool().await?;
        sqlx::query(
            "INSERT INTO channels (channel_id, name, protocol, enabled, config) \
             VALUES (7, 'modbus-channel', 'modbus_tcp', 0, '{}')",
        )
        .execute(&pool)
        .await?;
        sqlx::query(
            "INSERT INTO telemetry_points (channel_id, point_id, signal_name) \
             VALUES (7, 1, 'wrong-protocol')",
        )
        .execute(&pool)
        .await?;
        sqlx::query(
            "INSERT INTO json_point_mappings \
             (channel_id, point_id, point_type, json_path) \
             VALUES (7, 1, 'T', '$.value')",
        )
        .execute(&pool)
        .await?;

        let error = run_migrations(&pool)
            .await
            .expect_err("non-JSON protocol mapping must fail closed");

        assert!(format!("{error:#}").contains("non-JSON protocol"));
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM json_point_mappings")
                .fetch_one(&pool)
                .await?,
            1
        );
        Ok(())
    }

    #[tokio::test]
    async fn fresh_schema_has_no_legacy_json_mapping_table() -> Result<()> {
        let workspace = TempDir::new()?;
        let database_file = workspace.path().join("aether.db");
        init_database(&database_file).await?;
        let pool = SqlitePoolOptions::new()
            .connect_with(common::bootstrap_database::sqlite_connect_options(
                database_file.to_str().unwrap_or_default(),
            ))
            .await?;
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM sqlite_master \
                 WHERE type = 'table' AND name = 'json_point_mappings'",
            )
            .fetch_one(&pool)
            .await?,
            0
        );
        Ok(())
    }

    #[tokio::test]
    async fn migrate_v11_installs_one_stable_logical_routing_cas_head() -> Result<()> {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await?;
        sqlx::query("PRAGMA user_version = 10")
            .execute(&pool)
            .await?;

        run_migrations(&pool).await?;

        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT revision FROM configuration_revisions \
                 WHERE scope = 'logical_routing'",
            )
            .fetch_one(&pool)
            .await?,
            1
        );
        sqlx::query(
            "UPDATE configuration_revisions SET revision = 7 \
             WHERE scope = 'logical_routing'",
        )
        .execute(&pool)
        .await?;
        let mut connection = pool.acquire().await?;
        migrate_v11(&mut connection).await?;
        drop(connection);
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT revision FROM configuration_revisions \
                 WHERE scope = 'logical_routing'",
            )
            .fetch_one(&pool)
            .await?,
            7,
            "idempotent migration must not reset the authority head"
        );
        Ok(())
    }

    const POINT_TABLE_COLUMNS: [(&str, &[&str]); 4] = [
        (
            "telemetry_points",
            &[
                "point_id",
                "channel_id",
                "signal_name",
                "scale",
                "offset",
                "unit",
                "reverse",
                "data_type",
                "description",
                "protocol_mappings",
            ],
        ),
        (
            "signal_points",
            &[
                "point_id",
                "channel_id",
                "signal_name",
                "scale",
                "offset",
                "unit",
                "reverse",
                "normal_state",
                "data_type",
                "description",
                "protocol_mappings",
            ],
        ),
        (
            "control_points",
            &[
                "point_id",
                "channel_id",
                "signal_name",
                "scale",
                "offset",
                "unit",
                "reverse",
                "data_type",
                "description",
                "protocol_mappings",
            ],
        ),
        (
            "adjustment_points",
            &[
                "point_id",
                "channel_id",
                "signal_name",
                "scale",
                "offset",
                "unit",
                "reverse",
                "data_type",
                "description",
                "protocol_mappings",
            ],
        ),
    ];

    #[tokio::test]
    async fn database_initialization_is_repeatable() -> Result<()> {
        let workspace = TempDir::new()?;
        let database_file = workspace.path().join("aether.db");

        init_database(&database_file).await?;
        init_database(&database_file).await?;

        Ok(())
    }

    #[tokio::test]
    async fn database_initialization_is_safe_when_repeated_concurrently() -> Result<()> {
        let workspace = TempDir::new()?;
        let database_file = workspace.path().join("aether.db");
        init_database(&database_file).await?;

        let (first, second, third, fourth) = tokio::join!(
            init_database(&database_file),
            init_database(&database_file),
            init_database(&database_file),
            init_database(&database_file),
        );
        for result in [first, second, third, fourth] {
            result?;
        }

        Ok(())
    }

    #[tokio::test]
    async fn fresh_v10_database_installs_channel_revision_contract() -> Result<()> {
        let workspace = TempDir::new()?;
        let database_file = workspace.path().join("aether.db");
        init_database(&database_file).await?;
        let pool = SqlitePoolOptions::new()
            .connect_with(common::bootstrap_database::sqlite_connect_options(
                database_file.to_str().unwrap_or_default(),
            ))
            .await?;

        sqlx::query(
            "INSERT INTO channels (channel_id, name, protocol, enabled, config) \
             VALUES (7, 'fresh-v10', 'virtual', 0, '{}')",
        )
        .execute(&pool)
        .await?;
        sqlx::query("UPDATE channels SET name = 'legacy-write' WHERE channel_id = 7")
            .execute(&pool)
            .await?;
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT revision FROM channels WHERE channel_id = 7")
                .fetch_one(&pool)
                .await?,
            2
        );

        sqlx::query(
            "UPDATE channels SET protocol = 'virtual', revision = revision + 1 \
             WHERE channel_id = 7 AND revision = 2",
        )
        .execute(&pool)
        .await?;
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT revision FROM channels WHERE channel_id = 7")
                .fetch_one(&pool)
                .await?,
            3,
            "explicit governed update must not be bumped twice"
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM sqlite_master \
                 WHERE type = 'trigger' \
                   AND name IN (\
                       'bump_channel_revision',\
                       'reject_exhausted_channel_revision',\
                       'reject_exhausted_channel_revision_on_recreate',\
                       'advance_channel_revision_on_recreate',\
                       'reject_exhausted_channel_revision_on_delete',\
                       'tombstone_channel_revision_on_delete'\
                   )",
            )
            .fetch_one(&pool)
            .await?,
            6
        );
        Ok(())
    }

    #[tokio::test]
    async fn deleting_an_action_target_fails_closed_instead_of_deleting_its_route() -> Result<()> {
        let workspace = TempDir::new()?;
        let database_file = workspace.path().join("aether.db");
        init_database(&database_file).await?;
        let pool = SqlitePoolOptions::new()
            .connect_with(common::bootstrap_database::sqlite_connect_options(
                database_file.to_str().unwrap_or_default(),
            ))
            .await?;

        sqlx::query(
            "INSERT INTO channels (channel_id, name, protocol, enabled, config) \
             VALUES (7, 'governed-channel', 'virtual', 0, '{}')",
        )
        .execute(&pool)
        .await?;
        sqlx::query(
            "INSERT INTO instances (instance_id, instance_name, product_name) \
             VALUES (1, 'governed-instance', 'ExampleDevice')",
        )
        .execute(&pool)
        .await?;

        for (point_table, channel_type, point_id, action_id) in [
            ("control_points", "C", 103_i64, 1_i64),
            ("adjustment_points", "A", 104_i64, 2_i64),
        ] {
            sqlx::query(&format!(
                "INSERT INTO {point_table} \
                 (point_id, channel_id, signal_name, reverse, data_type) \
                 VALUES (?, 7, 'target', 0, 'bool')"
            ))
            .bind(point_id)
            .execute(&pool)
            .await?;
            sqlx::query(
                "INSERT INTO action_routing \
                 (instance_id, instance_name, action_id, channel_id, channel_type, channel_point_id) \
                 VALUES (1, 'governed-instance', ?, 7, ?, ?)",
            )
            .bind(action_id)
            .bind(channel_type)
            .bind(point_id)
            .execute(&pool)
            .await?;

            let error = sqlx::query(&format!(
                "DELETE FROM {point_table} WHERE channel_id = 7 AND point_id = ?"
            ))
            .bind(point_id)
            .execute(&pool)
            .await
            .err()
            .context("action target deletion unexpectedly bypassed governance")?;
            assert!(
                error
                    .to_string()
                    .contains("governed action-routing command"),
                "unexpected error: {error}"
            );

            let error = sqlx::query(&format!(
                "UPDATE {point_table} SET point_id = point_id + 1000 \
                 WHERE channel_id = 7 AND point_id = ?"
            ))
            .bind(point_id)
            .execute(&pool)
            .await
            .err()
            .context("action target identity update unexpectedly bypassed governance")?;
            assert!(
                error
                    .to_string()
                    .contains("governed action-routing command"),
                "unexpected error: {error}"
            );

            let route_count: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM action_routing \
                 WHERE instance_id = 1 AND action_id = ?",
            )
            .bind(action_id)
            .fetch_one(&pool)
            .await?;
            assert_eq!(route_count, 1);
        }

        for (statement, context) in [
            (
                "DELETE FROM channels WHERE channel_id = 7",
                "channel deletion unexpectedly mutated action routing",
            ),
            (
                "DELETE FROM instances WHERE instance_id = 1",
                "instance deletion unexpectedly cascaded into action routing",
            ),
        ] {
            let error = sqlx::query(statement)
                .execute(&pool)
                .await
                .err()
                .with_context(|| context)?;
            assert!(
                error
                    .to_string()
                    .contains("governed action-routing command"),
                "unexpected error: {error}"
            );
        }

        let route_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM action_routing")
            .fetch_one(&pool)
            .await?;
        assert_eq!(route_count, 2);

        Ok(())
    }

    #[tokio::test]
    async fn measurement_targets_and_parents_fail_closed_for_direct_sql() -> Result<()> {
        let workspace = TempDir::new()?;
        let database_file = workspace.path().join("aether.db");
        init_database(&database_file).await?;
        let pool = SqlitePoolOptions::new()
            .connect_with(common::bootstrap_database::sqlite_connect_options(
                database_file.to_str().unwrap_or_default(),
            ))
            .await?;

        for statement in [
            "INSERT INTO channels (channel_id, name, protocol, enabled, config) \
             VALUES (8, 'measurement-channel', 'virtual', 0, '{}')",
            "INSERT INTO instances (instance_id, instance_name, product_name) \
             VALUES (2, 'measurement-instance', 'ExampleDevice')",
            "INSERT INTO telemetry_points (point_id, channel_id, signal_name) \
             VALUES (201, 8, 'temperature')",
            "INSERT INTO signal_points (point_id, channel_id, signal_name) \
             VALUES (202, 8, 'running')",
        ] {
            sqlx::query(statement).execute(&pool).await?;
        }

        for (instance_name, channel_type, point_id) in [
            ("measurement-instance", "T", 999_i64),
            ("measurement-instance", "T", 202_i64),
            ("wrong-instance-name", "S", 202_i64),
        ] {
            let error = sqlx::query(
                "INSERT INTO measurement_routing \
                 (instance_id, instance_name, channel_id, channel_type, channel_point_id, measurement_id) \
                 VALUES (2, ?, 8, ?, ?, 99)",
            )
            .bind(instance_name)
            .bind(channel_type)
            .bind(point_id)
            .execute(&pool)
            .await
            .err()
            .context("invalid measurement route unexpectedly bypassed target validation")?;
            assert!(
                error
                    .to_string()
                    .contains("matching instance and T/S physical target"),
                "unexpected error: {error}"
            );
        }

        sqlx::query(
            "INSERT INTO measurement_routing \
             (instance_id, instance_name, channel_id, channel_type, channel_point_id, measurement_id) \
             VALUES (2, 'measurement-instance', 8, 'T', 201, 1), \
                    (2, 'measurement-instance', 8, 'S', 202, 2)",
        )
        .execute(&pool)
        .await?;

        let error = sqlx::query(
            "UPDATE measurement_routing SET channel_point_id = 202 \
             WHERE instance_id = 2 AND measurement_id = 1",
        )
        .execute(&pool)
        .await
        .err()
        .context("mistyped measurement target update unexpectedly succeeded")?;
        assert!(
            error
                .to_string()
                .contains("matching instance and T/S physical target"),
            "unexpected error: {error}"
        );

        for (statement, expected_context) in [
            (
                "UPDATE telemetry_points SET point_id = 301 \
                 WHERE channel_id = 8 AND point_id = 201",
                "changing a measurement target identity",
            ),
            (
                "DELETE FROM signal_points WHERE channel_id = 8 AND point_id = 202",
                "deleting a measurement target",
            ),
            (
                "DELETE FROM channels WHERE channel_id = 8",
                "deleting a measurement channel",
            ),
            (
                "DELETE FROM instances WHERE instance_id = 2",
                "deleting a measurement instance",
            ),
        ] {
            let error = sqlx::query(statement)
                .execute(&pool)
                .await
                .err()
                .context("measurement routing parent mutation unexpectedly succeeded")?;
            assert!(
                error.to_string().contains(expected_context),
                "unexpected error: {error}"
            );
        }

        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM measurement_routing")
                .fetch_one(&pool)
                .await?,
            2
        );
        Ok(())
    }

    #[tokio::test]
    async fn action_route_direct_sql_requires_the_matching_physical_kind() -> Result<()> {
        let workspace = TempDir::new()?;
        let database_file = workspace.path().join("aether.db");
        init_database(&database_file).await?;
        let pool = SqlitePoolOptions::new()
            .connect_with(common::bootstrap_database::sqlite_connect_options(
                database_file.to_str().unwrap_or_default(),
            ))
            .await?;

        for statement in [
            "INSERT INTO channels (channel_id, name, protocol, enabled, config) \
             VALUES (9, 'action-channel', 'virtual', 0, '{}')",
            "INSERT INTO instances (instance_id, instance_name, product_name) \
             VALUES (3, 'action-instance', 'ExampleDevice')",
            "INSERT INTO adjustment_points (point_id, channel_id, signal_name) \
             VALUES (301, 9, 'setpoint')",
        ] {
            sqlx::query(statement).execute(&pool).await?;
        }

        let error = sqlx::query(
            "INSERT INTO action_routing \
             (instance_id, instance_name, action_id, channel_id, channel_type, channel_point_id) \
             VALUES (3, 'action-instance', 1, 9, 'C', 301)",
        )
        .execute(&pool)
        .await
        .err()
        .context("mistyped action target unexpectedly bypassed target validation")?;
        assert!(
            error
                .to_string()
                .contains("matching instance and C/A physical target"),
            "unexpected error: {error}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn repeated_initialization_replaces_stale_routing_triggers() -> Result<()> {
        let workspace = TempDir::new()?;
        let database_file = workspace.path().join("aether.db");
        init_database(&database_file).await?;
        let pool = SqlitePoolOptions::new()
            .connect_with(common::bootstrap_database::sqlite_connect_options(
                database_file.to_str().unwrap_or_default(),
            ))
            .await?;

        sqlx::query("DROP TRIGGER validate_measurement_routing_target_on_insert")
            .execute(&pool)
            .await?;
        sqlx::query(
            "CREATE TRIGGER validate_measurement_routing_target_on_insert \
             BEFORE INSERT ON measurement_routing BEGIN SELECT 1; END",
        )
        .execute(&pool)
        .await?;
        sqlx::query(
            "CREATE TRIGGER cleanup_routing_on_telemetry_delete \
             AFTER DELETE ON telemetry_points BEGIN DELETE FROM measurement_routing; END",
        )
        .execute(&pool)
        .await?;

        init_database(&database_file).await?;

        let trigger_sql: String = sqlx::query_scalar(
            "SELECT sql FROM sqlite_master \
             WHERE type = 'trigger' AND name = 'validate_measurement_routing_target_on_insert'",
        )
        .fetch_one(&pool)
        .await?;
        assert!(trigger_sql.contains("matching instance and T/S physical target"));
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM sqlite_master \
                 WHERE type = 'trigger' AND name = 'cleanup_routing_on_telemetry_delete'",
            )
            .fetch_one(&pool)
            .await?,
            0
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'trigger' \
                 AND name IN (\
                    'validate_measurement_routing_target_on_insert',\
                    'validate_measurement_routing_target_on_update',\
                    'validate_action_routing_target_on_insert',\
                    'validate_action_routing_target_on_update',\
                    'protect_measurement_routing_on_telemetry_delete',\
                    'protect_measurement_routing_on_telemetry_identity_update',\
                    'protect_measurement_routing_on_signal_delete',\
                    'protect_measurement_routing_on_signal_identity_update',\
                    'protect_action_routing_on_control_delete',\
                    'protect_action_routing_on_control_identity_update',\
                    'protect_action_routing_on_adjustment_delete',\
                    'protect_action_routing_on_adjustment_identity_update',\
                    'protect_measurement_routing_on_channel_delete',\
                    'protect_measurement_routing_on_instance_delete',\
                    'protect_action_routing_on_channel_delete',\
                    'protect_action_routing_on_instance_delete'\
                 )",
            )
            .fetch_one(&pool)
            .await?,
            LOGICAL_ROUTING_INTEGRITY_TRIGGER_NAMES.len() as i64
        );
        Ok(())
    }

    #[tokio::test]
    async fn initialization_rejects_preexisting_invalid_routes_without_changing_triggers()
    -> Result<()> {
        let workspace = TempDir::new()?;
        let database_file = workspace.path().join("aether.db");
        init_database(&database_file).await?;
        let pool = SqlitePoolOptions::new()
            .connect_with(common::bootstrap_database::sqlite_connect_options(
                database_file.to_str().unwrap_or_default(),
            ))
            .await?;

        for statement in [
            "INSERT INTO channels (channel_id, name, protocol, enabled, config) \
             VALUES (10, 'legacy-channel', 'virtual', 0, '{}')",
            "INSERT INTO instances (instance_id, instance_name, product_name) \
             VALUES (4, 'legacy-instance', 'ExampleDevice')",
            "DROP TRIGGER validate_measurement_routing_target_on_insert",
            "CREATE TRIGGER cleanup_routing_on_telemetry_delete \
             AFTER DELETE ON telemetry_points BEGIN DELETE FROM measurement_routing; END",
            "INSERT INTO measurement_routing \
             (instance_id, instance_name, measurement_id, channel_id, channel_type, channel_point_id) \
             VALUES (4, 'legacy-instance', 1, 10, 'T', 404)",
        ] {
            sqlx::query(statement).execute(&pool).await?;
        }

        let error = init_database(&database_file)
            .await
            .err()
            .context("invalid legacy route unexpectedly passed initialization")?;
        assert!(
            format!("{error:#}").contains("invalid measurement route"),
            "unexpected error: {error:#}"
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM measurement_routing")
                .fetch_one(&pool)
                .await?,
            1
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM sqlite_master \
                 WHERE type = 'trigger' AND name = 'cleanup_routing_on_telemetry_delete'",
            )
            .fetch_one(&pool)
            .await?,
            1,
            "failed validation must not partially replace the trigger set"
        );
        Ok(())
    }

    #[tokio::test]
    async fn generic_schema_refuses_to_drop_pack_owned_legacy_properties() -> Result<()> {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await?;
        sqlx::query(
            "CREATE TABLE instances (\
                 instance_id INTEGER PRIMARY KEY,\
                 instance_name TEXT NOT NULL UNIQUE,\
                 product_name TEXT NOT NULL,\
                 properties TEXT,\
                 parent_id INTEGER,\
                 created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,\
                 updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP\
             )",
        )
        .execute(&pool)
        .await?;
        sqlx::query(
            "INSERT INTO instances (instance_id, instance_name, product_name, properties) \
             VALUES (1, 'legacy', 'DistributionDevice', '{\"domain_key\":42}')",
        )
        .execute(&pool)
        .await?;
        let mut connection = pool.acquire().await?;

        let error = migrate_v5(&mut connection)
            .await
            .err()
            .context("generic v5 migration unexpectedly consumed Pack-owned properties")?;

        assert!(format!("{error:#}").contains("Pack-owned legacy properties"));
        let properties: String =
            sqlx::query_scalar("SELECT properties FROM instances WHERE instance_id = 1")
                .fetch_one(&mut *connection)
                .await?;
        assert_eq!(properties, r#"{"domain_key":42}"#);
        Ok(())
    }

    #[tokio::test]
    async fn generic_v2_marker_does_not_rewrite_distribution_product_names() -> Result<()> {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await?;
        sqlx::query(
            "CREATE TABLE instances (instance_id INTEGER PRIMARY KEY, product_name TEXT NOT NULL)",
        )
        .execute(&pool)
        .await?;
        sqlx::query(
            "INSERT INTO instances (instance_id, product_name) VALUES (1, 'distribution_alias')",
        )
        .execute(&pool)
        .await?;
        let mut connection = pool.acquire().await?;

        migrate_v2(&mut connection).await?;

        let product: String =
            sqlx::query_scalar("SELECT product_name FROM instances WHERE instance_id = 1")
                .fetch_one(&mut *connection)
                .await?;
        assert_eq!(product, "distribution_alias");
        Ok(())
    }

    async fn legacy_point_pool() -> Result<SqlitePool> {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await?;

        sqlx::query("PRAGMA foreign_keys=ON").execute(&pool).await?;
        sqlx::query(CHANNELS_TABLE).execute(&pool).await?;

        for ddl in [
            TELEMETRY_POINTS_TABLE,
            SIGNAL_POINTS_TABLE,
            CONTROL_POINTS_TABLE,
            ADJUSTMENT_POINTS_TABLE,
        ] {
            let legacy_ddl = ddl.replace(" ON DELETE CASCADE", "");
            sqlx::query(&legacy_ddl).execute(&pool).await?;
        }

        sqlx::query(
            "INSERT INTO channels (channel_id, name, protocol, enabled, config) \
             VALUES (7, 'legacy-channel', 'modbus_tcp', 1, '{\"host\":\"127.0.0.1\"}')",
        )
        .execute(&pool)
        .await?;
        sqlx::query(
            "INSERT INTO telemetry_points \
             (point_id, channel_id, signal_name, scale, offset, unit, reverse, data_type, \
              description, protocol_mappings) \
             VALUES (101, 7, 'temperature', 1.5, -2.0, 'C', 1, 'f64', \
                     'telemetry sentinel', '{\"register\":1}')",
        )
        .execute(&pool)
        .await?;
        sqlx::query(
            "INSERT INTO signal_points \
             (point_id, channel_id, signal_name, scale, offset, unit, reverse, normal_state, \
              data_type, description, protocol_mappings) \
             VALUES (102, 7, 'breaker_closed', 2.5, 3.0, NULL, 0, 1, 'bool', \
                     'signal sentinel', '{\"bit\":2}')",
        )
        .execute(&pool)
        .await?;
        sqlx::query(
            "INSERT INTO control_points \
             (point_id, channel_id, signal_name, scale, offset, unit, reverse, data_type, \
              description, protocol_mappings) \
             VALUES (103, 7, 'start', 1.0, 0.0, NULL, 1, 'bool', \
                     'control sentinel', '{\"coil\":3}')",
        )
        .execute(&pool)
        .await?;
        sqlx::query(
            "INSERT INTO adjustment_points \
             (point_id, channel_id, signal_name, scale, offset, unit, reverse, data_type, \
              description, protocol_mappings) \
             VALUES (104, 7, 'setpoint', 0.1, -10.0, 'kW', 0, 'f32', \
                     'adjustment sentinel', '{\"holding\":4}')",
        )
        .execute(&pool)
        .await?;

        // Schema objects attached to rebuilt point tables must survive the
        // migration. Include both Aether's standard objects and arbitrary
        // operator-created objects in the fixture.
        for (index, table) in [
            ("idx_telemetry_points_channel", "telemetry_points"),
            ("idx_signal_points_channel", "signal_points"),
            ("idx_control_points_channel", "control_points"),
            ("idx_adjustment_points_channel", "adjustment_points"),
        ] {
            sqlx::query(&format!("CREATE INDEX {index} ON {table}(channel_id)"))
                .execute(&pool)
                .await?;
        }
        sqlx::query(
            "CREATE INDEX operator_signal_description \
             ON signal_points(description) WHERE description IS NOT NULL",
        )
        .execute(&pool)
        .await?;

        sqlx::query(INSTANCES_TABLE).execute(&pool).await?;
        sqlx::query(MEASUREMENT_ROUTING_TABLE)
            .execute(&pool)
            .await?;
        sqlx::query(ACTION_ROUTING_TABLE).execute(&pool).await?;
        sqlx::query(
            "INSERT INTO instances (instance_id, instance_name, product_name) \
             VALUES (1, 'migration-instance', 'ExampleDevice')",
        )
        .execute(&pool)
        .await?;
        sqlx::query(
            "INSERT INTO measurement_routing \
             (instance_id, instance_name, channel_id, channel_type, channel_point_id, measurement_id) \
             VALUES (1, 'migration-instance', 7, 'T', 101, 1), \
                    (1, 'migration-instance', 7, 'S', 102, 2)",
        )
        .execute(&pool)
        .await?;
        sqlx::query(
            "INSERT INTO action_routing \
             (instance_id, instance_name, action_id, channel_id, channel_type, channel_point_id) \
             VALUES (1, 'migration-instance', 1, 7, 'C', 103), \
                    (1, 'migration-instance', 2, 7, 'A', 104)",
        )
        .execute(&pool)
        .await?;
        create_triggers(&pool).await?;

        sqlx::query("CREATE TABLE operator_point_audit (message TEXT NOT NULL)")
            .execute(&pool)
            .await?;
        sqlx::query(
            "CREATE TRIGGER operator_control_update \
             AFTER UPDATE ON control_points FOR EACH ROW \
             BEGIN \
                 INSERT INTO operator_point_audit(message) VALUES (NEW.signal_name); \
             END",
        )
        .execute(&pool)
        .await?;

        sqlx::query("PRAGMA user_version = 6")
            .execute(&pool)
            .await?;

        Ok(pool)
    }

    async fn point_snapshot(pool: &SqlitePool) -> Result<BTreeMap<String, Vec<Vec<String>>>> {
        let mut snapshot = BTreeMap::new();

        for (table, columns) in POINT_TABLE_COLUMNS {
            let quoted_columns = columns
                .iter()
                .map(|column| format!("quote({column})"))
                .collect::<Vec<_>>()
                .join(", ");
            let rows = sqlx::query(&format!(
                "SELECT {quoted_columns} FROM {table} ORDER BY channel_id, point_id"
            ))
            .fetch_all(pool)
            .await?;
            let values = rows
                .iter()
                .map(|row| {
                    (0..columns.len())
                        .map(|index| row.try_get(index))
                        .collect::<std::result::Result<Vec<String>, sqlx::Error>>()
                })
                .collect::<std::result::Result<Vec<_>, sqlx::Error>>()?;
            snapshot.insert(table.to_owned(), values);
        }

        Ok(snapshot)
    }

    async fn point_schema_snapshot(
        pool: &SqlitePool,
    ) -> Result<Vec<(String, String, String, String)>> {
        Ok(sqlx::query_as(
            "SELECT tbl_name, type, name, sql FROM sqlite_master \
             WHERE tbl_name IN \
                 ('telemetry_points', 'signal_points', 'control_points', 'adjustment_points') \
               AND type IN ('index', 'trigger') AND sql IS NOT NULL \
             ORDER BY tbl_name, type, name",
        )
        .fetch_all(pool)
        .await?)
    }

    async fn routing_snapshot(pool: &SqlitePool) -> Result<BTreeMap<String, Vec<Vec<String>>>> {
        let mut snapshot = BTreeMap::new();
        for (table, columns) in [
            (
                "measurement_routing",
                &[
                    "routing_id",
                    "instance_id",
                    "instance_name",
                    "channel_id",
                    "channel_type",
                    "channel_point_id",
                    "measurement_id",
                    "description",
                    "enabled",
                    "created_at",
                    "updated_at",
                ][..],
            ),
            (
                "action_routing",
                &[
                    "routing_id",
                    "instance_id",
                    "instance_name",
                    "action_id",
                    "channel_id",
                    "channel_type",
                    "channel_point_id",
                    "description",
                    "enabled",
                    "created_at",
                    "updated_at",
                ][..],
            ),
        ] {
            let quoted_columns = columns
                .iter()
                .map(|column| format!("quote({column})"))
                .collect::<Vec<_>>()
                .join(", ");
            let rows = sqlx::query(&format!(
                "SELECT {quoted_columns} FROM {table} ORDER BY routing_id"
            ))
            .fetch_all(pool)
            .await?;
            let values = rows
                .iter()
                .map(|row| {
                    (0..columns.len())
                        .map(|index| row.try_get(index))
                        .collect::<std::result::Result<Vec<String>, sqlx::Error>>()
                })
                .collect::<std::result::Result<Vec<_>, sqlx::Error>>()?;
            snapshot.insert(table.to_owned(), values);
        }
        Ok(snapshot)
    }

    async fn point_fk_delete_action(pool: &SqlitePool, table: &str) -> Result<String> {
        sqlx::query_scalar(&format!(
            "SELECT on_delete FROM pragma_foreign_key_list('{table}') \
             WHERE \"table\" = 'channels' AND \"from\" = 'channel_id' AND \"to\" = 'channel_id'"
        ))
        .fetch_one(pool)
        .await
        .with_context(|| format!("read {table}.channel_id foreign key"))
    }

    #[tokio::test]
    async fn migrate_v7_preserves_all_point_data_and_adds_cascade() -> Result<()> {
        let pool = legacy_point_pool().await?;
        let before = point_snapshot(&pool).await?;
        let schema_before = point_schema_snapshot(&pool).await?;
        let routing_before = routing_snapshot(&pool).await?;

        run_migrations(&pool).await?;

        assert_eq!(
            sqlx::query_scalar::<_, i64>("PRAGMA user_version")
                .fetch_one(&pool)
                .await?,
            SCHEMA_VERSION as i64
        );
        assert_eq!(point_snapshot(&pool).await?, before);
        assert_eq!(point_schema_snapshot(&pool).await?, schema_before);
        assert_eq!(routing_snapshot(&pool).await?, routing_before);
        let constraints = sqlx::query_as::<_, (Option<f64>, Option<f64>, f64)>(
            "SELECT min_value, max_value, step FROM adjustment_points
             WHERE channel_id = 7 AND point_id = 104",
        )
        .fetch_one(&pool)
        .await?;
        assert_eq!(constraints, (None, None, 1.0));
        for (table, _) in POINT_TABLE_COLUMNS {
            assert_eq!(point_fk_delete_action(&pool, table).await?, "CASCADE");
        }

        // Simulate the governed route-removal command before deleting its
        // channel; v7's assertion here concerns point-table FK behavior.
        for routing_table in ["measurement_routing", "action_routing"] {
            sqlx::query(&format!("DELETE FROM {routing_table}"))
                .execute(&pool)
                .await?;
        }
        sqlx::query("DELETE FROM channels WHERE channel_id = 7")
            .execute(&pool)
            .await?;
        for (table, _) in POINT_TABLE_COLUMNS {
            let count: i64 = sqlx::query_scalar(&format!("SELECT COUNT(*) FROM {table}"))
                .fetch_one(&pool)
                .await?;
            assert_eq!(count, 0, "channel delete must cascade into {table}");
        }

        Ok(())
    }

    #[tokio::test]
    async fn migrate_v9_and_v10_add_revision_and_aba_guards() -> Result<()> {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await?;
        sqlx::query(
            "CREATE TABLE channels (\
                 channel_id INTEGER NOT NULL PRIMARY KEY,\
                 name TEXT NOT NULL UNIQUE,\
                 protocol TEXT,\
                 enabled INTEGER NOT NULL DEFAULT 1,\
                 config TEXT,\
                 created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,\
                 updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP\
             )",
        )
        .execute(&pool)
        .await?;
        sqlx::query(
            "INSERT INTO channels (channel_id, name, protocol, enabled, config) \
             VALUES (7, 'pre-v9', 'virtual', 0, '{}')",
        )
        .execute(&pool)
        .await?;
        sqlx::query("PRAGMA user_version = 8")
            .execute(&pool)
            .await?;

        run_migrations(&pool).await?;

        assert_eq!(
            sqlx::query_scalar::<_, i64>("PRAGMA user_version")
                .fetch_one(&pool)
                .await?,
            SCHEMA_VERSION as i64
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT revision FROM channels WHERE channel_id = 7")
                .fetch_one(&pool)
                .await?,
            2,
            "v10 must invalidate every token issued before tombstones existed"
        );
        sqlx::query("UPDATE channels SET config = '{\"legacy\":true}' WHERE channel_id = 7")
            .execute(&pool)
            .await?;
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT revision FROM channels WHERE channel_id = 7")
                .fetch_one(&pool)
                .await?,
            3
        );
        sqlx::query(
            "UPDATE channels SET protocol = 'virtual', revision = 4 \
             WHERE channel_id = 7 AND revision = 3",
        )
        .execute(&pool)
        .await?;
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT revision FROM channels WHERE channel_id = 7")
                .fetch_one(&pool)
                .await?,
            4,
            "explicit CAS revision must not be incremented twice"
        );

        sqlx::query("DELETE FROM channels WHERE channel_id = 7")
            .execute(&pool)
            .await?;
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT last_revision FROM channel_revision_tombstones WHERE channel_id = 7",
            )
            .fetch_one(&pool)
            .await?,
            5
        );
        sqlx::query(
            "INSERT INTO channels (channel_id, name, protocol, enabled, config) \
             VALUES (7, 'legacy-recreate', 'virtual', 0, '{}')",
        )
        .execute(&pool)
        .await?;
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT revision FROM channels WHERE channel_id = 7")
                .fetch_one(&pool)
                .await?,
            6,
            "legacy recreation must advance beyond the delete tombstone"
        );

        Ok(())
    }

    #[tokio::test]
    async fn migrate_v10_upgrades_an_existing_v9_database() -> Result<()> {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await?;
        sqlx::query(CHANNELS_TABLE).execute(&pool).await?;
        sqlx::query(CHANNEL_REVISION_EXHAUSTED_TRIGGER)
            .execute(&pool)
            .await?;
        sqlx::query(CHANNEL_REVISION_BUMP_TRIGGER)
            .execute(&pool)
            .await?;
        sqlx::query(
            "INSERT INTO channels \
             (channel_id, name, protocol, enabled, config, revision) \
             VALUES (7, 'original-v9-entity', 'virtual', 0, '{}', 1)",
        )
        .execute(&pool)
        .await?;
        sqlx::query("DELETE FROM channels WHERE channel_id = 7")
            .execute(&pool)
            .await?;
        sqlx::query(
            "INSERT INTO channels \
             (channel_id, name, protocol, enabled, config, revision) \
             VALUES (7, 'replacement-v9-entity', 'virtual', 0, '{}', 1)",
        )
        .execute(&pool)
        .await?;
        sqlx::query("PRAGMA user_version = 9")
            .execute(&pool)
            .await?;

        run_migrations(&pool).await?;

        assert_eq!(
            sqlx::query_scalar::<_, i64>("PRAGMA user_version")
                .fetch_one(&pool)
                .await?,
            SCHEMA_VERSION as i64
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT revision FROM channels WHERE channel_id = 7")
                .fetch_one(&pool)
                .await?,
            2,
            "v10 must invalidate a token reused by a v9 delete/recreate cycle"
        );
        sqlx::query("DELETE FROM channels WHERE channel_id = 7")
            .execute(&pool)
            .await?;
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT last_revision FROM channel_revision_tombstones WHERE channel_id = 7",
            )
            .fetch_one(&pool)
            .await?,
            3
        );
        sqlx::query(
            "INSERT INTO channels (channel_id, name, protocol, enabled, config) \
             VALUES (7, 'legacy-recreate', 'virtual', 0, '{}')",
        )
        .execute(&pool)
        .await?;
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT revision FROM channels WHERE channel_id = 7")
                .fetch_one(&pool)
                .await?,
            4,
            "legacy recreation must advance beyond the v10 tombstone"
        );
        assert_eq!(
            sqlx::query(
                "UPDATE channels SET name = 'stale-cas' WHERE channel_id = 7 AND revision = 1"
            )
            .execute(&pool)
            .await?
            .rows_affected(),
            0,
            "a stale v9 CAS token must not match the recreated entity"
        );

        Ok(())
    }

    #[tokio::test]
    async fn migrate_v7_stale_new_table_rolls_back_every_live_table() -> Result<()> {
        let pool = legacy_point_pool().await?;
        let before = point_snapshot(&pool).await?;
        let schema_before = point_schema_snapshot(&pool).await?;
        let routing_before = routing_snapshot(&pool).await?;
        sqlx::query("CREATE TABLE adjustment_points_new (marker TEXT NOT NULL)")
            .execute(&pool)
            .await?;
        sqlx::query("INSERT INTO adjustment_points_new (marker) VALUES ('stale sentinel')")
            .execute(&pool)
            .await?;

        let error = run_migrations(&pool)
            .await
            .err()
            .context("v7 migration unexpectedly accepted a stale *_new table")?;
        assert!(
            format!("{error:#}").contains("adjustment_points_new"),
            "unexpected migration error: {error:#}"
        );

        assert_eq!(
            sqlx::query_scalar::<_, i64>("PRAGMA user_version")
                .fetch_one(&pool)
                .await?,
            6
        );
        assert_eq!(point_snapshot(&pool).await?, before);
        assert_eq!(point_schema_snapshot(&pool).await?, schema_before);
        assert_eq!(routing_snapshot(&pool).await?, routing_before);
        for (table, _) in POINT_TABLE_COLUMNS {
            assert_eq!(point_fk_delete_action(&pool, table).await?, "NO ACTION");
        }
        for table in [
            "telemetry_points_new",
            "signal_points_new",
            "control_points_new",
        ] {
            let exists: bool = sqlx::query_scalar(
                "SELECT COUNT(*) > 0 FROM sqlite_master WHERE type='table' AND name=?",
            )
            .bind(table)
            .fetch_one(&pool)
            .await?;
            assert!(!exists, "rollback left temporary table {table}");
        }
        assert_eq!(
            sqlx::query_scalar::<_, String>("SELECT marker FROM adjustment_points_new LIMIT 1")
                .fetch_one(&pool)
                .await?,
            "stale sentinel"
        );

        // A successful immediate transaction proves the failed migration did
        // not leak its transaction into the pooled connection.
        let mut conn = pool.acquire().await?;
        begin_v7_immediate_transaction(&mut conn)
            .await?
            .rollback()
            .await?;

        Ok(())
    }

    #[tokio::test]
    async fn migrate_v7_legacy_backup_table_fails_closed() -> Result<()> {
        let pool = legacy_point_pool().await?;
        let points_before = point_snapshot(&pool).await?;
        let schema_before = point_schema_snapshot(&pool).await?;
        let routing_before = routing_snapshot(&pool).await?;
        sqlx::query(
            "CREATE TABLE telemetry_points_backup AS \
             SELECT * FROM telemetry_points",
        )
        .execute(&pool)
        .await?;

        let error = run_migrations(&pool)
            .await
            .err()
            .context("v7 migration unexpectedly accepted a legacy *_backup table")?;
        assert!(
            format!("{error:#}").contains("telemetry_points_backup"),
            "unexpected migration error: {error:#}"
        );
        assert_eq!(point_snapshot(&pool).await?, points_before);
        assert_eq!(point_schema_snapshot(&pool).await?, schema_before);
        assert_eq!(routing_snapshot(&pool).await?, routing_before);
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM telemetry_points_backup")
                .fetch_one(&pool)
                .await?,
            1
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>("PRAGMA user_version")
                .fetch_one(&pool)
                .await?,
            6
        );

        Ok(())
    }

    #[tokio::test]
    async fn dropped_v7_transaction_guard_rolls_back_and_releases_write_lock() -> Result<()> {
        let pool = legacy_point_pool().await?;
        let before = point_snapshot(&pool).await?;
        let worker_pool = pool.clone();
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();

        let task = tokio::spawn(async move {
            let mut conn = worker_pool.acquire().await?;
            let mut transaction = begin_v7_immediate_transaction(&mut conn).await?;
            sqlx::query(
                "UPDATE telemetry_points SET signal_name = 'uncommitted' \
                 WHERE channel_id = 7 AND point_id = 101",
            )
            .execute(&mut *transaction)
            .await?;
            entered_tx
                .send(())
                .map_err(|()| anyhow!("cancellation test receiver disappeared"))?;
            std::future::pending::<()>().await;
            Ok::<(), anyhow::Error>(())
        });

        entered_rx
            .await
            .context("v7 transaction did not reach its cancellation point")?;
        task.abort();
        let join_error = task
            .await
            .err()
            .context("v7 transaction task unexpectedly completed")?;
        assert!(join_error.is_cancelled());

        assert_eq!(point_snapshot(&pool).await?, before);
        let mut conn = pool.acquire().await?;
        begin_v7_immediate_transaction(&mut conn)
            .await?
            .rollback()
            .await?;

        Ok(())
    }
}
