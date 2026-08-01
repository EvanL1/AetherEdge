//! Database connection and setup utilities
//!
//! Provides common functions for setting up SQLite connections.

use errors::{AetherError, AetherResult};
use sqlx::SqlitePool;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use std::path::Path;
use std::str::FromStr;
use tracing::{debug, error, info};

/// Database connection configuration
#[derive(Debug, Clone)]
pub struct DatabaseConfig {
    /// SQLite database path
    pub sqlite_path: String,
    /// Maximum SQLite connections
    pub sqlite_max_connections: u32,
    /// Connection timeout in seconds
    pub connection_timeout: u64,
}

impl Default for DatabaseConfig {
    fn default() -> Self {
        Self {
            sqlite_path: "data/service.db".to_string(),
            sqlite_max_connections: 5,
            connection_timeout: 10,
        }
    }
}

/// Build `SqliteConnectOptions` for a AetherEdge database path.
///
/// All pools across the system should be constructed with this so that
/// connection-scoped pragmas (`foreign_keys=ON`, `journal_mode=WAL`,
/// `create_if_missing=true`) are applied uniformly. SQLite's
/// `PRAGMA foreign_keys` is per-connection, so without this every newly
/// opened connection in a pool would default to FK enforcement OFF and
/// silently ignore declared constraints.
pub fn sqlite_connect_options(db_path: &str) -> SqliteConnectOptions {
    // `from_str` parses the URL form; we then layer concrete options on top.
    // Falls back to a path-based builder if URL parsing fails (it shouldn't,
    // but keep the helper total).
    let opts = SqliteConnectOptions::from_str(&format!("sqlite:{}?mode=rwc", db_path))
        .unwrap_or_else(|_| SqliteConnectOptions::new().filename(db_path));
    opts.create_if_missing(true)
        .foreign_keys(true)
        .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
        .busy_timeout(std::time::Duration::from_secs(5))
}

/// Open a service SQLite pool, creating the database's parent directory first.
///
/// Unlike [`setup_sqlite_pool`] this creates a missing database instead of
/// failing, which is what the long-running services need on first start.
pub async fn open_service_pool(db_path: &str) -> anyhow::Result<SqlitePool> {
    if let Some(dir) = Path::new(db_path).parent() {
        std::fs::create_dir_all(dir)?;
    }

    SqlitePoolOptions::new()
        .max_connections(5)
        .connect_with(sqlite_connect_options(db_path))
        .await
        .map_err(|e| anyhow::anyhow!("SQLite connect failed: {e} path={db_path}"))
}

/// Setup SQLite database connection pool with FK enforcement enabled.
pub async fn setup_sqlite_pool(db_path: &str) -> AetherResult<SqlitePool> {
    // Check if database file exists
    if !Path::new(db_path).exists() {
        error!("DB not found: {}", db_path);
        return Err(AetherError::DatabaseNotFound {
            path: db_path.to_string(),
            service: "unknown".to_string(),
        });
    }

    info!("SQLite: {}", db_path);

    let pool = SqlitePoolOptions::new()
        .connect_with(sqlite_connect_options(db_path))
        .await
        .map_err(|e| {
            AetherError::Database(format!("Failed to connect to SQLite database: {}", e))
        })?;

    // Confirm FK enforcement is live for this pool — if a future sqlx upgrade
    // ever changes default ordering, fail loudly instead of silently allowing
    // orphans to slip in.
    let fk_on: i64 = sqlx::query_scalar("PRAGMA foreign_keys")
        .fetch_one(&pool)
        .await
        .map_err(|e| {
            AetherError::Database(format!("Failed to verify PRAGMA foreign_keys: {}", e))
        })?;
    if fk_on != 1 {
        return Err(AetherError::Database(
            "PRAGMA foreign_keys did not engage on new pool".to_string(),
        ));
    }

    debug!("SQLite pool ready (foreign_keys=ON, journal_mode=WAL)");
    Ok(pool)
}

#[cfg(test)]
#[allow(clippy::disallowed_methods)] // Test code - unwrap is acceptable
mod tests {
    use super::*;

    #[test]
    fn test_default_database_config() {
        let config = DatabaseConfig::default();
        assert_eq!(config.sqlite_path, "data/service.db");
        assert_eq!(config.sqlite_max_connections, 5);
        assert_eq!(config.connection_timeout, 10);
    }
}
