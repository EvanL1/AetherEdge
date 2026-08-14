/// Persistent service configuration stored in the shared SQLite database.
///
/// All AetherEdge services share the same SQLite file (`AETHER_DB_PATH`).
/// history adds a `history_config` table to that database for its own settings.
///
/// Two separate sets of settings are stored:
/// - **Operational** (`ServiceConfig`) – exposed via `/hisApi/config`.
/// - **Storage connection** (`StorageSettings`) – exposed via `/hisApi/storage`.
use sqlx::SqlitePool;
use std::borrow::Cow;
use std::collections::HashMap;
use std::fmt::Display;
use std::str::FromStr;
use tracing::info;

use crate::models::{ServiceConfig, StorageSettings, pattern_serde};

const DEFAULTS: &[(&str, &str, &str)] = &[
    // ── Operational ──────────────────────────────────────────────────────────
    (
        "collection_interval_secs",
        "30",
        "How often (s) selected SHM series are sampled",
    ),
    (
        "flush_interval_secs",
        "60",
        "How often (s) buffer is flushed to storage",
    ),
    ("batch_size", "1000", "Max rows per storage write call"),
    ("cleanup_enabled", "true", "Whether old-data cleanup runs"),
    (
        "cleanup_older_than_days",
        "30",
        "Retain data for this many days",
    ),
    ("default_page_size", "100", "Default query page size"),
    ("max_page_size", "1000", "Maximum allowed page size"),
    (
        "max_time_range_days",
        "365",
        "Maximum query time range (days)",
    ),
    (
        "subscribe_patterns",
        r#"{"inst:*:M":null,"inst:*:A":null}"#,
        "JSON object mapping logical-series glob patterns to optional intervals",
    ),
    (
        "exclude_patterns",
        "[]",
        "JSON array of regex patterns to exclude",
    ),
];

pub async fn create_config_table(
    pool: &SqlitePool,
    local_history_path: &str,
) -> anyhow::Result<()> {
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS history_config (
            key         TEXT PRIMARY KEY,
            value       TEXT NOT NULL,
            description TEXT,
            updated_at  TEXT DEFAULT (datetime('now'))
        )",
    )
    .execute(pool)
    .await?;

    // Insert defaults for any missing keys (ON CONFLICT DO NOTHING).
    for (key, value, desc) in DEFAULTS {
        sqlx::query(
            "INSERT OR IGNORE INTO history_config (key, value, description)
             VALUES (?, ?, ?)",
        )
        .bind(key)
        .bind(value)
        .bind(desc)
        .execute(pool)
        .await?;
    }
    for (key, value, description) in [
        (
            "storage_enabled",
            "true",
            "Whether the storage backend is active",
        ),
        (
            "storage_backend",
            "sqlite",
            "Backend type: sqlite | postgres | timescaledb",
        ),
        (
            "storage_url",
            local_history_path,
            "Local path or external database DSN",
        ),
    ] {
        sqlx::query(
            "INSERT OR IGNORE INTO history_config (key, value, description) VALUES (?, ?, ?)",
        )
        .bind(key)
        .bind(value)
        .bind(description)
        .execute(pool)
        .await?;
    }

    info!("history_config table ready");
    Ok(())
}

// ── Shared internal helper ────────────────────────────────────────────────────

async fn load_all_kv(pool: &SqlitePool) -> anyhow::Result<HashMap<String, String>> {
    let rows: Vec<(String, String)> = sqlx::query_as("SELECT key, value FROM history_config")
        .fetch_all(pool)
        .await?;
    Ok(rows.into_iter().collect())
}

fn value_or_default<'a>(
    values: &'a HashMap<String, String>,
    key: &str,
    default: &'static str,
) -> &'a str {
    values.get(key).map(String::as_str).unwrap_or(default)
}

fn parse_value<T>(
    values: &HashMap<String, String>,
    key: &str,
    default: &'static str,
) -> anyhow::Result<T>
where
    T: FromStr,
    T::Err: Display,
{
    let value = value_or_default(values, key, default);
    value
        .parse()
        .map_err(|error| anyhow::anyhow!("invalid history_config {key}={value:?}: {error}"))
}

async fn upsert_pairs(pool: &SqlitePool, pairs: &[(&str, Cow<'_, str>)]) -> anyhow::Result<()> {
    let mut tx = pool.begin().await?;
    for (key, value) in pairs {
        sqlx::query(
            "INSERT INTO history_config (key, value)
             VALUES (?, ?)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value,
                                            updated_at = datetime('now')",
        )
        .bind(key)
        .bind(value.as_ref())
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(())
}

// ── Operational config ────────────────────────────────────────────────────────

/// Load operational settings from the DB (`/hisApi/config`).
pub async fn load_config(pool: &SqlitePool) -> anyhow::Result<ServiceConfig> {
    let map = load_all_kv(pool).await?;

    let subscribe_patterns = crate::models::pattern_serde::from_json_str(value_or_default(
        &map,
        "subscribe_patterns",
        r#"{"inst:*:M":null,"inst:*:A":null}"#,
    ))
    .map_err(|error| anyhow::anyhow!("invalid history_config subscribe_patterns: {error}"))?;
    let exclude_patterns: Vec<String> =
        serde_json::from_str(value_or_default(&map, "exclude_patterns", "[]"))
            .map_err(|error| anyhow::anyhow!("invalid history_config exclude_patterns: {error}"))?;

    let mut cfg = ServiceConfig {
        collection_interval_secs: parse_value(&map, "collection_interval_secs", "30")?,
        flush_interval_secs: parse_value(&map, "flush_interval_secs", "60")?,
        batch_size: parse_value(&map, "batch_size", "1000")?,
        cleanup_enabled: parse_value(&map, "cleanup_enabled", "true")?,
        cleanup_older_than_days: parse_value(&map, "cleanup_older_than_days", "30")?,
        default_page_size: parse_value(&map, "default_page_size", "100")?,
        max_page_size: parse_value(&map, "max_page_size", "1000")?,
        max_time_range_days: parse_value(&map, "max_time_range_days", "365")?,
        subscribe_patterns,
        exclude_patterns,
    };
    cfg.normalize();
    Ok(cfg)
}

/// Persist operational settings back to the DB.
pub async fn save_config(pool: &SqlitePool, cfg: &ServiceConfig) -> anyhow::Result<()> {
    let mut cfg = cfg.clone();
    cfg.normalize();
    let pairs: Vec<(&str, Cow<'_, str>)> = vec![
        (
            "collection_interval_secs",
            Cow::Owned(cfg.collection_interval_secs.to_string()),
        ),
        (
            "flush_interval_secs",
            Cow::Owned(cfg.flush_interval_secs.to_string()),
        ),
        ("batch_size", Cow::Owned(cfg.batch_size.to_string())),
        (
            "cleanup_enabled",
            Cow::Owned(cfg.cleanup_enabled.to_string()),
        ),
        (
            "cleanup_older_than_days",
            Cow::Owned(cfg.cleanup_older_than_days.to_string()),
        ),
        (
            "default_page_size",
            Cow::Owned(cfg.default_page_size.to_string()),
        ),
        ("max_page_size", Cow::Owned(cfg.max_page_size.to_string())),
        (
            "max_time_range_days",
            Cow::Owned(cfg.max_time_range_days.to_string()),
        ),
        (
            "subscribe_patterns",
            Cow::Owned(pattern_serde::to_json_str(&cfg.subscribe_patterns)?),
        ),
        (
            "exclude_patterns",
            Cow::Owned(serde_json::to_string(&cfg.exclude_patterns)?),
        ),
    ];
    upsert_pairs(pool, &pairs).await
}

// ── Storage connection settings ───────────────────────────────────────────────

/// Load storage connection settings from the DB (`/hisApi/storage`).
pub async fn load_storage(pool: &SqlitePool) -> anyhow::Result<StorageSettings> {
    let map = load_all_kv(pool).await?;

    Ok(StorageSettings {
        enabled: parse_value(&map, "storage_enabled", "true")?,
        backend: value_or_default(&map, "storage_backend", "sqlite").to_owned(),
        url: value_or_default(&map, "storage_url", "").to_owned(),
    })
}

/// Persist storage connection settings to the DB.
pub async fn save_storage(pool: &SqlitePool, s: &StorageSettings) -> anyhow::Result<()> {
    let pairs: Vec<(&str, Cow<'_, str>)> = vec![
        ("storage_enabled", Cow::Owned(s.enabled.to_string())),
        ("storage_backend", Cow::Borrowed(s.backend.as_str())),
        ("storage_url", Cow::Borrowed(s.url.as_str())),
    ];
    upsert_pairs(pool, &pairs).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::SqlitePoolOptions;

    async fn config_pool() -> SqlitePool {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("open in-memory history config database");
        create_config_table(&pool, "history.db")
            .await
            .expect("create history config table");
        pool
    }

    #[tokio::test]
    async fn corrupt_operational_values_fail_closed() {
        let pool = config_pool().await;
        for (key, invalid, valid) in [
            ("collection_interval_secs", "thirty", "30"),
            ("flush_interval_secs", "sixty", "60"),
            ("batch_size", "many", "1000"),
            ("cleanup_enabled", "yes", "true"),
            ("cleanup_older_than_days", "old", "30"),
            ("default_page_size", "normal", "100"),
            ("max_page_size", "large", "1000"),
            ("max_time_range_days", "year", "365"),
            ("subscribe_patterns", "[]", r#"{"inst:*:M":null}"#),
            ("exclude_patterns", "not-json", "[]"),
        ] {
            sqlx::query("UPDATE history_config SET value = ? WHERE key = ?")
                .bind(invalid)
                .bind(key)
                .execute(&pool)
                .await
                .expect("write corrupt fixture");
            let error = load_config(&pool)
                .await
                .expect_err("present corrupt configuration must be rejected");
            assert!(error.to_string().contains(key), "{key}: {error:#}");
            sqlx::query("UPDATE history_config SET value = ? WHERE key = ?")
                .bind(valid)
                .bind(key)
                .execute(&pool)
                .await
                .expect("restore valid fixture");
        }
    }

    #[tokio::test]
    async fn corrupt_storage_enabled_fails_closed() {
        let pool = config_pool().await;
        sqlx::query("UPDATE history_config SET value = '1' WHERE key = 'storage_enabled'")
            .execute(&pool)
            .await
            .expect("write corrupt fixture");
        let error = load_storage(&pool)
            .await
            .expect_err("non-boolean storage_enabled must be rejected");
        assert!(error.to_string().contains("storage_enabled"), "{error:#}");
    }
}
