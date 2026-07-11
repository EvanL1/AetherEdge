use anyhow::Result;
use sqlx::{
    SqlitePool as SqlxSqlitePool,
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous},
};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use tracing::{info, warn};

pub type SqlitePool = SqlxSqlitePool;

#[derive(Clone)]
pub struct SqliteClient {
    pool: Arc<SqlitePool>,
    db_path: String,
}

impl SqliteClient {
    /// Create a new SQLite client with optimized settings for edge deployment
    pub async fn new(db_path: impl AsRef<Path>) -> Result<Self> {
        let db_path_str = db_path.as_ref().to_string_lossy().to_string();

        // Ensure parent directory exists
        if let Some(parent) = db_path.as_ref().parent() {
            std::fs::create_dir_all(parent)?;
        }

        let options = SqliteConnectOptions::new()
            .filename(&db_path_str)
            .journal_mode(SqliteJournalMode::Wal) // Enable WAL for concurrent reads
            .synchronous(SqliteSynchronous::Normal) // Balance performance and safety
            .busy_timeout(Duration::from_secs(5))
            .create_if_missing(true);

        let pool = SqlitePoolOptions::new()
            .max_connections(10) // Reasonable for edge deployment
            .connect_with(options)
            .await?;

        // Set cache size to 2MB (negative value means KB)
        sqlx::query("PRAGMA cache_size = -2000")
            .execute(&pool)
            .await?;

        // Set page size to 4KB (only effective for new databases)
        sqlx::query("PRAGMA page_size = 4096")
            .execute(&pool)
            .await?;

        // Enable foreign key constraints
        sqlx::query("PRAGMA foreign_keys = ON")
            .execute(&pool)
            .await?;

        info!("SQLite: {} (FK=ON)", db_path_str);

        Ok(Self {
            pool: Arc::new(pool),
            db_path: db_path_str,
        })
    }

    /// Create a read-only connection pool for services
    pub async fn new_readonly(db_path: impl AsRef<Path>) -> Result<Self> {
        let db_path_str = db_path.as_ref().to_string_lossy().to_string();

        if !db_path.as_ref().exists() {
            warn!("SQLite not found: {}", db_path_str);
            return Err(anyhow::anyhow!("Database file not found"));
        }

        let options = SqliteConnectOptions::new()
            .filename(&db_path_str)
            .journal_mode(SqliteJournalMode::Wal)
            .read_only(true);

        let pool = SqlitePoolOptions::new()
            .max_connections(5) // Fewer connections for read-only
            .connect_with(options)
            .await?;

        // Enable foreign key constraints (even for read-only to ensure consistency)
        sqlx::query("PRAGMA foreign_keys = ON")
            .execute(&pool)
            .await?;

        info!("SQLite: {} (RO,FK=ON)", db_path_str);

        Ok(Self {
            pool: Arc::new(pool),
            db_path: db_path_str,
        })
    }

    /// Create from an existing pool
    pub fn from_pool(pool: SqlitePool) -> Self {
        Self {
            pool: Arc::new(pool),
            db_path: "from_pool".to_string(),
        }
    }

    /// Get the underlying connection pool
    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    /// Get database file path
    pub fn path(&self) -> &str {
        &self.db_path
    }

    /// Check if database is accessible
    pub async fn ping(&self) -> Result<()> {
        sqlx::query("SELECT 1").execute(&*self.pool).await?;
        Ok(())
    }

    /// Get database file size in bytes
    pub fn size(&self) -> Result<u64> {
        let metadata = std::fs::metadata(&self.db_path)?;
        Ok(metadata.len())
    }

    /// Vacuum database to reclaim space
    pub async fn vacuum(&self) -> Result<()> {
        sqlx::query("VACUUM").execute(&*self.pool).await?;
        info!("SQLite vacuumed: {}", self.db_path);
        Ok(())
    }
}
