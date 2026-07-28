//! SQLite connection setup shared by local service composition roots.

use sqlx::SqlitePool;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use std::path::Path;
use thiserror::Error;

/// Failures while opening the local runtime database.
#[derive(Debug, Error)]
pub enum DatabaseSetupError {
    #[error("configuration database not found at {0}")]
    NotFound(String),
    #[error("failed to connect to SQLite database: {0}")]
    Connect(#[source] sqlx::Error),
    #[error("failed to verify SQLite foreign-key enforcement: {0}")]
    VerifyForeignKeys(#[source] sqlx::Error),
    #[error("SQLite foreign-key enforcement is disabled")]
    ForeignKeysDisabled,
}

/// Build the common SQLite options for every local service pool.
pub fn sqlite_connect_options(db_path: &str) -> SqliteConnectOptions {
    SqliteConnectOptions::new()
        .filename(db_path)
        .create_if_missing(true)
        .foreign_keys(true)
        .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
        .busy_timeout(std::time::Duration::from_secs(5))
}

/// Open an existing local SQLite database with FK enforcement enabled.
pub async fn setup_sqlite_pool(db_path: &str) -> Result<SqlitePool, DatabaseSetupError> {
    if !Path::new(db_path).exists() {
        return Err(DatabaseSetupError::NotFound(db_path.to_owned()));
    }

    let pool = SqlitePoolOptions::new()
        .connect_with(sqlite_connect_options(db_path))
        .await
        .map_err(DatabaseSetupError::Connect)?;

    let foreign_keys: i64 = sqlx::query_scalar("PRAGMA foreign_keys")
        .fetch_one(&pool)
        .await
        .map_err(DatabaseSetupError::VerifyForeignKeys)?;
    if foreign_keys != 1 {
        return Err(DatabaseSetupError::ForeignKeysDisabled);
    }

    Ok(pool)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn missing_database_fails_before_pool_creation() {
        let result = setup_sqlite_pool("/nonexistent/aether/database.db").await;
        assert!(matches!(result, Err(DatabaseSetupError::NotFound(_))));
    }
}
