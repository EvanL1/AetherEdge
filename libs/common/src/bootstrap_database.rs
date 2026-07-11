//! Database connection and setup utilities
//!
//! Provides common functions for setting up SQLite connections and, behind
//! the explicit `redis` feature, optional Redis connections.

use errors::{AetherError, AetherResult};
use sqlx::SqlitePool;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use std::path::Path;
use std::str::FromStr;
#[cfg(feature = "redis")]
use std::sync::Arc;
use tracing::{debug, error, info, warn};

#[cfg(feature = "redis")]
use crate::config_loader::{
    DEFAULT_REDIS_MAX_ATTEMPTS, build_redis_candidates, connect_redis_with_retry,
};
#[cfg(feature = "redis")]
use crate::redis::RedisClient;

/// Database connection configuration
#[derive(Debug, Clone)]
pub struct DatabaseConfig {
    /// SQLite database path
    pub sqlite_path: String,
    /// Redis URL for the optional mirror extension.
    #[cfg(feature = "redis")]
    pub redis_url: Option<String>,
    /// Maximum SQLite connections
    pub sqlite_max_connections: u32,
    /// Connection timeout in seconds
    pub connection_timeout: u64,
}

impl Default for DatabaseConfig {
    fn default() -> Self {
        Self {
            sqlite_path: "data/service.db".to_string(),
            #[cfg(feature = "redis")]
            redis_url: None,
            sqlite_max_connections: 5,
            connection_timeout: 10,
        }
    }
}

/// Build `SqliteConnectOptions` for a AetherEMS database path.
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

/// Setup Redis connection with exponential backoff retry (base=2s, max=15s, retries=5)
#[cfg(feature = "redis")]
pub async fn setup_redis_with_retry(
    redis_url: Option<String>,
) -> AetherResult<(String, Arc<RedisClient>)> {
    const MAX_RETRIES: u32 = 5;
    const BASE_DELAY_MS: u64 = 2000;
    const MAX_DELAY_MS: u64 = 15000;

    let candidates = build_redis_candidates(redis_url, "redis://127.0.0.1:6379");
    let url = candidates
        .first()
        .map(|(_, u)| u.clone())
        .unwrap_or_else(|| "redis://127.0.0.1:6379".to_string());

    let mut retry_count = 0u32;
    loop {
        match RedisClient::new(&url).await {
            Ok(client) => match client.ping().await {
                Ok(_) => {
                    info!("Redis connected: {}", url);
                    return Ok((url, Arc::new(client)));
                },
                Err(e) if retry_count < MAX_RETRIES => {
                    let delay = (BASE_DELAY_MS * 2u64.pow(retry_count)).min(MAX_DELAY_MS);
                    warn!(
                        "Redis ping failed (retry {}/{}): {} — retrying in {}ms",
                        retry_count + 1,
                        MAX_RETRIES,
                        e,
                        delay
                    );
                    tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
                    retry_count += 1;
                },
                Err(e) => {
                    return Err(AetherError::Internal(format!(
                        "Redis ping failed after {} retries: {}",
                        MAX_RETRIES, e
                    )));
                },
            },
            Err(e) if retry_count < MAX_RETRIES => {
                let delay = (BASE_DELAY_MS * 2u64.pow(retry_count)).min(MAX_DELAY_MS);
                warn!(
                    "Redis not ready (retry {}/{}): {} — retrying in {}ms",
                    retry_count + 1,
                    MAX_RETRIES,
                    e,
                    delay
                );
                tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
                retry_count += 1;
            },
            Err(e) => {
                return Err(AetherError::Internal(format!(
                    "Redis connection failed after {} retries: {}",
                    MAX_RETRIES, e
                )));
            },
        }
    }
}

/// Setup Redis connection with retry logic
#[cfg(feature = "redis")]
pub async fn setup_redis_connection(
    redis_url: Option<String>,
) -> AetherResult<(String, Arc<RedisClient>)> {
    setup_redis_with_retry(redis_url).await
}

/// Setup Redis with custom timeout
#[cfg(feature = "redis")]
pub async fn setup_redis_with_timeout(
    redis_url: Option<String>,
    timeout: tokio::time::Duration,
) -> AetherResult<(String, Arc<RedisClient>)> {
    // Build connection candidates with priority
    let candidates = build_redis_candidates(redis_url, "redis://127.0.0.1:6379");

    info!("Redis: {} candidates", candidates.len());

    // Connect with retry logic (use default max attempts)
    let (url, client) = connect_redis_with_retry(candidates, timeout, DEFAULT_REDIS_MAX_ATTEMPTS)
        .await
        .map_err(AetherError::Internal)?;

    info!("Redis connected: {}", url);
    Ok((url, Arc::new(client)))
}

/// Setup Redis with custom configuration (including dynamic connection pool)
///
/// This function allows fine-grained control over Redis connection pool settings,
/// particularly useful for dynamically adjusting pool size based on workload.
///
/// # Arguments
/// * `redis_url` - Optional Redis URL (falls back to environment or default)
/// * `redis_config` - Custom Redis configuration with pool settings
///
/// # Example
/// ```ignore
/// // In an async context:
/// let channel_count = 50;
/// let max_connections = channel_count * 2 + 30; // Dynamic calculation
///
/// let mut redis_config = RedisPoolConfig::default();
/// redis_config.max_connections = max_connections;
///
/// let (url, client) = setup_redis_with_config(None, redis_config).await?;
/// ```
#[cfg(feature = "redis")]
pub async fn setup_redis_with_config(
    redis_url: Option<String>,
    redis_config: crate::redis::RedisPoolConfig,
) -> AetherResult<(String, Arc<RedisClient>)> {
    // Build connection candidates with priority
    let candidates = build_redis_candidates(redis_url, "redis://127.0.0.1:6379");

    info!(
        "Redis: {} candidates (pool:{})",
        candidates.len(),
        redis_config.max_connections
    );

    // Connect with retry logic using custom config
    let timeout = tokio::time::Duration::from_secs(redis_config.connection_timeout);
    let (url, _) =
        connect_redis_with_retry(candidates.clone(), timeout, DEFAULT_REDIS_MAX_ATTEMPTS)
            .await
            .map_err(AetherError::Internal)?;

    // Create client with custom configuration
    let mut final_config = redis_config;
    final_config.url = url.clone();
    let pool_size = final_config.max_connections;

    let client = RedisClient::with_config(final_config)
        .await
        .map_err(|e| AetherError::Internal(format!("Failed to create Redis client: {}", e)))?;

    info!("Redis connected: {} (pool:{})", url, pool_size);
    Ok((url, Arc::new(client)))
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

/// Test Redis connection and basic operations
#[cfg(feature = "redis")]
pub async fn test_redis_connection(client: &RedisClient) -> AetherResult<()> {
    debug!("Testing Redis");

    // Test PING
    let pong: String = client
        .ping()
        .await
        .map_err(|e| AetherError::Communication(format!("Redis PING failed: {}", e)))?;

    if pong != "PONG" {
        return Err(AetherError::Communication(format!(
            "Unexpected PING response: {}",
            pong
        )));
    }

    // Test SET/GET
    let test_key = "aether:test:connection";
    let test_value = "ok";

    client
        .set(test_key, test_value)
        .await
        .map_err(|e| AetherError::Communication(format!("Redis SET failed: {}", e)))?;

    let retrieved: Option<String> = client
        .get(test_key)
        .await
        .map_err(|e| AetherError::Communication(format!("Redis GET failed: {}", e)))?;

    if retrieved != Some(test_value.to_string()) {
        return Err(AetherError::Communication(
            "Redis GET returned unexpected value".to_string(),
        ));
    }

    // Clean up test key
    let _: u32 = client
        .del(&[test_key])
        .await
        .map_err(|e| AetherError::Communication(format!("Redis DEL failed: {}", e)))?;

    debug!("Redis ok");
    Ok(())
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
