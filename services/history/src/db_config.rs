/// Persistent service configuration stored in the shared SQLite database.
///
/// All AetherEMS services share the same SQLite file (`AETHER_DB_PATH`).
/// history adds a `history_config` table to that database for its own settings.
///
/// Two separate sets of settings are stored:
/// - **Operational** (`ServiceConfig`) – exposed via `/hisApi/config`.
/// - **Storage connection** (`StorageSettings`) – exposed via `/hisApi/storage`.
use sqlx::SqlitePool;
use std::borrow::Cow;
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
        r#"["inst:*:M","inst:*:A"]"#,
        "JSON logical-series glob patterns to collect",
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

async fn load_all_kv(
    pool: &SqlitePool,
) -> anyhow::Result<std::collections::HashMap<String, String>> {
    let rows: Vec<(String, String)> = sqlx::query_as("SELECT key, value FROM history_config")
        .fetch_all(pool)
        .await?;
    Ok(rows.into_iter().collect())
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
    let get = |k: &str, d: &str| map.get(k).cloned().unwrap_or_else(|| d.to_string());

    let subscribe_patterns = crate::models::pattern_serde::from_json_str(&get(
        "subscribe_patterns",
        r#"["inst:*:M","inst:*:A"]"#,
    ));
    let exclude_patterns: Vec<String> =
        serde_json::from_str(&get("exclude_patterns", "[]")).unwrap_or_default();

    let mut cfg = ServiceConfig {
        collection_interval_secs: get("collection_interval_secs", "30").parse().unwrap_or(30),
        flush_interval_secs: get("flush_interval_secs", "60").parse().unwrap_or(60),
        batch_size: get("batch_size", "1000").parse().unwrap_or(1000),
        cleanup_enabled: get("cleanup_enabled", "true") == "true",
        cleanup_older_than_days: get("cleanup_older_than_days", "30").parse().unwrap_or(30),
        default_page_size: get("default_page_size", "100").parse().unwrap_or(100),
        max_page_size: get("max_page_size", "1000").parse().unwrap_or(1000),
        max_time_range_days: get("max_time_range_days", "365").parse().unwrap_or(365),
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
    let get = |k: &str, d: &str| map.get(k).cloned().unwrap_or_else(|| d.to_string());

    Ok(StorageSettings {
        enabled: get("storage_enabled", "true") == "true",
        backend: get("storage_backend", "sqlite"),
        url: get("storage_url", ""),
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
