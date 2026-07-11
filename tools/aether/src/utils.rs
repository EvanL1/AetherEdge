//! Utility functions for aether CLI

use anyhow::Result;
use sqlx::SqlitePool;
use std::path::Path;
use tracing::debug;

/// Database status information
#[derive(Debug)]
#[allow(dead_code)] // Fields accessed via Debug trait for logging
pub struct DatabaseStatus {
    pub exists: bool,
    pub initialized: bool,
    pub last_sync: Option<String>,
    pub item_count: Option<usize>,
    pub schema_version: Option<String>,
}

/// Check database status
pub async fn check_database_status(db_path: &Path) -> Result<DatabaseStatus> {
    debug!("Checking database status: {:?}", db_path);

    // Check if database file exists
    if !db_path.exists() {
        return Ok(DatabaseStatus {
            exists: false,
            initialized: false,
            last_sync: None,
            item_count: None,
            schema_version: None,
        });
    }

    // Connect to database in read-only mode
    let connection_string = format!("sqlite://{}?mode=ro", db_path.display());
    let pool = SqlitePool::connect(&connection_string).await?;

    // Check if service_config table exists
    let table_exists: bool = sqlx::query_scalar(
        "SELECT 1 FROM sqlite_master WHERE type='table' AND name='service_config'",
    )
    .fetch_optional(&pool)
    .await?
    .unwrap_or(false);

    if !table_exists {
        return Ok(DatabaseStatus {
            exists: true,
            initialized: false,
            last_sync: None,
            item_count: None,
            schema_version: None,
        });
    }

    // Sync timestamps moved to the dedicated per-domain metadata table. Keep
    // the legacy key as a read-only fallback for pre-migration databases.
    let sync_metadata_exists: bool = sqlx::query_scalar(
        "SELECT 1 FROM sqlite_master WHERE type='table' AND name='sync_metadata'",
    )
    .fetch_optional(&pool)
    .await?
    .unwrap_or(false);
    let last_sync: Option<String> = if sync_metadata_exists {
        sqlx::query_scalar("SELECT MAX(last_sync) FROM sync_metadata")
            .fetch_one(&pool)
            .await?
    } else {
        sqlx::query_scalar("SELECT value FROM service_config WHERE key = '_sync_timestamp'")
            .fetch_optional(&pool)
            .await?
    };

    // Get item count
    let item_count: Option<i64> = sqlx::query_scalar("SELECT COUNT(*) FROM service_config")
        .fetch_optional(&pool)
        .await?;

    // Get schema version if available
    let schema_version: Option<String> =
        sqlx::query_scalar("SELECT value FROM service_config WHERE key = '_schema_version'")
            .fetch_optional(&pool)
            .await?;

    Ok(DatabaseStatus {
        exists: true,
        initialized: true,
        last_sync,
        item_count: item_count.map(|c| c as usize),
        schema_version,
    })
}

#[cfg(test)]
mod tests {
    use super::check_database_status;

    #[tokio::test]
    async fn database_status_reads_the_latest_sync_metadata_timestamp() {
        let workspace = tempfile::tempdir().unwrap();
        let database_file = workspace.path().join("aether.db");
        crate::core::schema::init_database(&database_file)
            .await
            .unwrap();
        let pool =
            sqlx::SqlitePool::connect(&format!("sqlite:{}", database_file.to_string_lossy()))
                .await
                .unwrap();
        sqlx::query(
            "INSERT INTO sync_metadata (service, last_sync) VALUES \
             ('global', '2026-07-11T01:00:00Z'), \
             ('automation', '2026-07-11T02:00:00Z')",
        )
        .execute(&pool)
        .await
        .unwrap();
        pool.close().await;

        let status = check_database_status(&database_file).await.unwrap();

        assert_eq!(status.last_sync.as_deref(), Some("2026-07-11T02:00:00Z"));
    }
}
