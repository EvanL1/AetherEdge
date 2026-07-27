//! SQLite connection and setup utilities shared by local services.

use errors::{AetherError, AetherResult};
use sqlx::SqlitePool;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use std::path::Path;
use std::str::FromStr;
use tracing::{debug, error, info, warn};

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

/// Setup SQLite with custom configuration (still applies FK + WAL via shared options).
pub async fn setup_sqlite_with_config(config: &DatabaseConfig) -> AetherResult<SqlitePool> {
    // Check if database file exists
    if !Path::new(&config.sqlite_path).exists() {
        error!("DB not found: {}", config.sqlite_path);
        return Err(AetherError::DatabaseNotFound {
            path: config.sqlite_path.clone(),
            service: "unknown".to_string(),
        });
    }

    info!("SQLite: {}", config.sqlite_path);

    let pool = SqlitePoolOptions::new()
        .max_connections(config.sqlite_max_connections)
        .acquire_timeout(std::time::Duration::from_secs(config.connection_timeout))
        .connect_with(sqlite_connect_options(&config.sqlite_path))
        .await
        .map_err(|e| AetherError::Database(format!("Failed to connect to SQLite: {}", e)))?;

    Ok(pool)
}

/// Validate database exists and has required tables
pub async fn validate_sqlite_schema(
    pool: &SqlitePool,
    required_tables: &[&str],
) -> AetherResult<()> {
    debug!("Validating schema");

    for table in required_tables {
        let query = format!(
            "SELECT name FROM sqlite_master WHERE type='table' AND name='{}'",
            table
        );

        let result: Option<(String,)> =
            sqlx::query_as(&query)
                .fetch_optional(pool)
                .await
                .map_err(|e| {
                    AetherError::Database(format!("Failed to check table {}: {}", table, e))
                })?;

        if result.is_none() {
            error!("Missing table: {}", table);
            return Err(AetherError::Configuration(format!(
                "Missing required table: {}. Please run: aether init",
                table
            )));
        }

        debug!("Table ok: {}", table);
    }

    debug!("Schema valid");
    Ok(())
}

/// Check database file permissions
pub fn check_database_permissions(db_path: &str) -> AetherResult<()> {
    let path = Path::new(db_path);

    // Check if file exists
    if !path.exists() {
        return Err(AetherError::DatabaseNotFound {
            path: db_path.to_string(),
            service: "unknown".to_string(),
        });
    }

    // Check if we can read the file
    if !path.is_file() {
        return Err(AetherError::Configuration(format!(
            "{} is not a file",
            db_path
        )));
    }

    // Check parent directory for write permissions (for WAL files)
    if let Some(parent) = path.parent() {
        let metadata = parent.metadata().map_err(|e| {
            AetherError::Configuration(format!("Cannot access database directory: {}", e))
        })?;

        if metadata.permissions().readonly() {
            warn!("Read-only dir: {}", parent.display());
        }
    }

    Ok(())
}

/// Initialize database with retry logic
pub async fn initialize_database_with_retry(
    db_path: &str,
    max_retries: u32,
) -> AetherResult<SqlitePool> {
    let mut last_error = None;

    for attempt in 1..=max_retries {
        debug!("DB retry {}/{}", attempt, max_retries);

        match setup_sqlite_pool(db_path).await {
            Ok(pool) => {
                debug!("DB connected");
                return Ok(pool);
            },
            Err(e) => {
                warn!("DB retry {} failed: {}", attempt, e);
                last_error = Some(e);

                if attempt < max_retries {
                    let delay = std::time::Duration::from_secs(attempt as u64);
                    tokio::time::sleep(delay).await;
                }
            },
        }
    }

    Err(last_error.unwrap_or_else(|| {
        AetherError::Database("Failed to connect to database after all retries".to_string())
    }))
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

    #[tokio::test]
    async fn test_check_database_permissions() {
        // Test with non-existent file
        let result = check_database_permissions("/non/existent/path.db");
        assert!(result.is_err());

        // Test with existing file (use temp file in real tests)
        // This would require creating a temp file for proper testing
    }
}
