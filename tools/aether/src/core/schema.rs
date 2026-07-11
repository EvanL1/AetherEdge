//! Database schema initialization
//!
//! Provides unified database initialization for all AetherEMS tables.
//! All tables are created in a single `aether.db` file.

use anyhow::{Context, Result, anyhow, bail, ensure};
use sqlx::{Connection, Row, Sqlite, SqliteConnection, SqlitePool};
use std::path::Path;
use tracing::{info, warn};

// Import DDL constants from common (shared schema definitions)
use common::test_utils::schema::{
    ACTION_ROUTING_TABLE, ADJUSTMENT_POINTS_TABLE, CHANNEL_TEMPLATES_TABLE, CHANNELS_TABLE,
    CONTROL_POINTS_TABLE, INSTANCE_PROPERTIES_TABLE, INSTANCES_TABLE, MEASUREMENT_ROUTING_TABLE,
    SERVICE_CONFIG_TABLE, SIGNAL_POINTS_TABLE, SYNC_METADATA_TABLE, TELEMETRY_POINTS_TABLE,
};

use super::file_utils;

// ============================================================================
// JSON Point Mappings DDL (MQTT/HTTP protocol support)
// ============================================================================

/// JSON point mappings table DDL for MQTT/HTTP protocols
///
/// This table enables configuration-driven device integration:
/// - MQTT devices publish JSON payloads with vendor-specific formats
/// - HTTP devices return JSON responses with custom schemas
/// - JSONPath expressions extract values without code changes
const JSON_POINT_MAPPINGS_TABLE: &str = r#"
    CREATE TABLE IF NOT EXISTS json_point_mappings (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        channel_id INTEGER NOT NULL REFERENCES channels(channel_id) ON DELETE CASCADE,
        point_id INTEGER NOT NULL,
        point_type TEXT NOT NULL,
        json_path TEXT NOT NULL,
        data_type TEXT DEFAULT 'float',
        scale REAL DEFAULT 1.0,
        offset REAL DEFAULT 0.0,
        description TEXT,
        created_at TEXT DEFAULT CURRENT_TIMESTAMP,
        updated_at TEXT DEFAULT CURRENT_TIMESTAMP,
        UNIQUE(channel_id, point_id, point_type)
    )
"#;

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
pub(crate) const SCHEMA_VERSION: i32 = 8;

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

/// v2: Rename legacy snake_case product names to match new built-in product library
///
/// Old config templates used `battery_pack`, `pcs`, etc. The compile-time product
/// library uses `Battery`, `PCS`, `PVInverter`, `Diesel`. This migration updates
/// existing rows so `get_builtin_product()` can find them by exact name match.
async fn migrate_v2(conn: &mut sqlx::pool::PoolConnection<Sqlite>) -> Result<()> {
    // On a fresh database, instances table doesn't exist yet (created after migrations).
    // Only run the rename if the table is already present from a previous schema version.
    let has_instances: bool =
        sqlx::query_scalar("SELECT 1 FROM sqlite_master WHERE type='table' AND name='instances'")
            .fetch_optional(&mut **conn)
            .await?
            .unwrap_or(false);

    if !has_instances {
        info!("Migration v2: skipped (instances table not yet created)");
        return Ok(());
    }

    info!("Migration v2: renaming legacy product names");

    let mappings = [
        ("battery_pack", "Battery"),
        ("pcs", "PCS"),
        ("diesel_generator", "Diesel"),
        ("pv_inverter", "PVInverter"),
    ];

    for (old, new) in mappings {
        let result = sqlx::query("UPDATE instances SET product_name = ? WHERE product_name = ?")
            .bind(new)
            .bind(old)
            .execute(&mut **conn)
            .await?;

        if result.rows_affected() > 0 {
            info!(
                "Migration v2: renamed '{}' -> '{}' ({} rows)",
                old,
                new,
                result.rows_affected()
            );
        }
    }

    info!("Migration v2: complete");
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
/// — mirrors `measurement_routing` / `action_routing`. `property_id` is
/// resolved by looking up each JSON key against the instance's product's
/// PropertyTemplate (compile-time constants in the `aether-model` crate).
/// Keys that don't match any template are dropped with a warning — they
/// were unreachable from the existing `/api/instances/{id}/points` response
/// anyway, since that endpoint only emits points declared by the product.
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

    // 4) Read every (instance_id, product_name, properties_json) and translate
    //    each JSON entry to a row in instance_properties. We do this BEFORE
    //    dropping the column so failure mid-migration leaves data recoverable.
    let rows: Vec<(i64, String, Option<String>)> =
        sqlx::query_as("SELECT instance_id, product_name, properties FROM instances")
            .fetch_all(&mut **conn)
            .await?;

    let mut migrated_values = 0_usize;
    let mut dropped_keys = 0_usize;

    for (instance_id, product_name, properties_json) in rows {
        let Some(json_str) = properties_json else {
            continue;
        };
        let trimmed = json_str.trim();
        if trimmed.is_empty() || trimmed == "{}" {
            continue;
        }

        let map: serde_json::Map<String, serde_json::Value> = match serde_json::from_str(trimmed) {
            Ok(serde_json::Value::Object(m)) => m,
            Ok(_other) => {
                warn!(
                    "Migration v5: instance {} properties JSON is not an object, skipping",
                    instance_id
                );
                continue;
            },
            Err(e) => {
                warn!(
                    "Migration v5: instance {} properties JSON unparseable ({}), skipping",
                    instance_id, e
                );
                continue;
            },
        };

        let Some(product) = aether_model::product_lib::get_builtin_product(&product_name) else {
            warn!(
                "Migration v5: instance {} references unknown product '{}', dropping {} properties",
                instance_id,
                product_name,
                map.len()
            );
            dropped_keys += map.len();
            continue;
        };

        for (name, value) in map {
            let Some(tpl) = product.properties.iter().find(|p| p.name == name) else {
                warn!(
                    "Migration v5: instance {} key '{}' not in product '{}' PropertyTemplate, dropping",
                    instance_id, name, product_name
                );
                dropped_keys += 1;
                continue;
            };
            let value_json =
                serde_json::to_string(&value).context("encode property value to JSON")?;
            sqlx::query(
                "INSERT OR REPLACE INTO instance_properties \
                 (instance_id, property_id, value_json) VALUES (?, ?, ?)",
            )
            .bind(instance_id)
            .bind(tpl.id as i64)
            .bind(value_json)
            .execute(&mut **conn)
            .await?;
            migrated_values += 1;
        }
    }

    info!(
        "Migration v5: migrated {} property values, dropped {} unrecognized keys",
        migrated_values, dropped_keys
    );

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
/// Creates all tables, indexes, and triggers needed by AetherEMS services.
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

    // === Channel & Point tables ===
    sqlx::query(CHANNELS_TABLE).execute(&pool).await?;
    sqlx::query(TELEMETRY_POINTS_TABLE).execute(&pool).await?;
    sqlx::query(SIGNAL_POINTS_TABLE).execute(&pool).await?;
    sqlx::query(CONTROL_POINTS_TABLE).execute(&pool).await?;
    sqlx::query(ADJUSTMENT_POINTS_TABLE).execute(&pool).await?;

    // === JSON point mappings table (MQTT/HTTP protocols) ===
    sqlx::query(JSON_POINT_MAPPINGS_TABLE)
        .execute(&pool)
        .await?;

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

    // JSON point mappings indexes (for MQTT/HTTP protocols)
    sqlx::query(
        "CREATE INDEX IF NOT EXISTS idx_json_point_mappings_channel ON json_point_mappings(channel_id)",
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

/// Create all database triggers for automatic cleanup
async fn create_triggers(pool: &SqlitePool) -> Result<()> {
    // When a telemetry point is deleted, remove corresponding measurement_routing records
    sqlx::query(
        "CREATE TRIGGER IF NOT EXISTS cleanup_routing_on_telemetry_delete
         AFTER DELETE ON telemetry_points
         FOR EACH ROW
         BEGIN
             DELETE FROM measurement_routing
             WHERE channel_id = OLD.channel_id
               AND channel_type = 'T'
               AND channel_point_id = OLD.point_id;
         END",
    )
    .execute(pool)
    .await?;

    // When a signal point is deleted, remove corresponding measurement_routing records
    sqlx::query(
        "CREATE TRIGGER IF NOT EXISTS cleanup_routing_on_signal_delete
         AFTER DELETE ON signal_points
         FOR EACH ROW
         BEGIN
             DELETE FROM measurement_routing
             WHERE channel_id = OLD.channel_id
               AND channel_type = 'S'
               AND channel_point_id = OLD.point_id;
         END",
    )
    .execute(pool)
    .await?;

    // When a control point is deleted, remove corresponding action_routing records
    sqlx::query(
        "CREATE TRIGGER IF NOT EXISTS cleanup_routing_on_control_delete
         AFTER DELETE ON control_points
         FOR EACH ROW
         BEGIN
             DELETE FROM action_routing
             WHERE channel_id = OLD.channel_id
               AND channel_type = 'C'
               AND channel_point_id = OLD.point_id;
         END",
    )
    .execute(pool)
    .await?;

    // When an adjustment point is deleted, remove corresponding action_routing records
    sqlx::query(
        "CREATE TRIGGER IF NOT EXISTS cleanup_routing_on_adjustment_delete
         AFTER DELETE ON adjustment_points
         FOR EACH ROW
         BEGIN
             DELETE FROM action_routing
             WHERE channel_id = OLD.channel_id
               AND channel_type = 'A'
               AND channel_point_id = OLD.point_id;
         END",
    )
    .execute(pool)
    .await?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use anyhow::Context as _;
    use sqlx::sqlite::SqlitePoolOptions;

    use super::*;

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
             VALUES (1, 'migration-instance', 'Battery')",
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
