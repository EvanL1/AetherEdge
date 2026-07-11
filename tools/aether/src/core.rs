//! Aether Core - Configuration management core functionality
//!
//! This module provides the core functionality for managing service configurations
//! in the AetherEMS system. It supports both read-only and read-write access modes
//! and handles the synchronization between YAML/CSV files and SQLite databases.

use anyhow::{Context, Result};
use sqlx::SqlitePool;
use std::path::{Path, PathBuf};
use tracing::info;

// Module declarations
pub mod exporter;
pub mod file_utils;
pub mod schema;
pub mod syncer;
pub mod validator;

// Re-export key types
pub use common::ValidationResult;
pub use exporter::{ConfigExporter, ExportResult};
pub use syncer::{ConfigSyncer, SyncResult};
pub use validator::ConfigValidator;

/// Access mode for the Aether core
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum AccessMode {
    /// Read-write mode (for management tools)
    ReadWrite,
}

/// Aether core configuration management
pub struct AetherCore {
    /// Database path
    db_path: PathBuf,
    /// Configuration files path
    config_path: PathBuf,
    /// Access mode
    mode: AccessMode,
    /// Database connection pool
    pool: Option<SqlitePool>,
}

impl AetherCore {
    /// Create a read-write instance (for management tools)
    pub async fn readwrite(
        db_path: impl AsRef<Path>,
        config_path: impl AsRef<Path>,
        service: &str,
    ) -> Result<Self> {
        let config_path = config_path.as_ref().to_path_buf();
        let (db_dir, db_file, _explicit_file) = normalise_db_path(db_path.as_ref(), service);

        // Ensure database directory exists
        if let Some(parent) = db_file.parent() {
            std::fs::create_dir_all(parent).context("Failed to create database directory")?;
        }

        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .connect_with(common::bootstrap_database::sqlite_connect_options(
                db_file.to_str().unwrap_or_default(),
            ))
            .await
            .context("Failed to connect to database in read-write mode")?;

        info!("DB: {:?}", db_file);

        Ok(Self {
            db_path: db_dir,
            config_path,
            mode: AccessMode::ReadWrite,
            pool: Some(pool),
        })
    }

    /// Create an instance without connecting to database (for initialization)
    pub fn new(config_path: impl AsRef<Path>) -> Self {
        Self {
            db_path: PathBuf::from("data"),
            config_path: config_path.as_ref().to_path_buf(),
            mode: AccessMode::ReadWrite,
            pool: None,
        }
    }

    /// Validate configuration for a service
    pub async fn validate(&self, service: &str) -> Result<ValidationResult> {
        let validator = ConfigValidator::new(&self.config_path);
        validator.validate_service(service).await
    }

    /// Apply every configuration domain as one SQLite transaction.
    pub async fn sync_all(&self, force: bool) -> Result<Vec<(&'static str, SyncResult)>> {
        self.require_write_mode()?;

        let syncer = ConfigSyncer::new(&self.config_path, &self.db_path).with_force(force);
        syncer.sync_all().await
    }

    /// Apply the safe first-run configuration only while the database remains
    /// uncommissioned under the same SQLite writer transaction.
    pub async fn sync_empty_site(&self) -> Result<Vec<(&'static str, SyncResult)>> {
        self.require_write_mode()?;

        ConfigSyncer::new(&self.config_path, &self.db_path)
            .requiring_empty_site()
            .sync_all()
            .await
    }

    /// Export configuration from database to files
    pub async fn export(
        &self,
        service: &str,
        output_dir: impl AsRef<Path>,
    ) -> Result<ExportResult> {
        self.require_write_mode()?;

        let pool = self
            .pool
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Database not connected"))?;

        let exporter = ConfigExporter::new(pool.clone());
        exporter.export_service(service, output_dir).await
    }

    // Private helper methods

    fn require_write_mode(&self) -> Result<()> {
        if self.mode != AccessMode::ReadWrite {
            Err(anyhow::anyhow!("Operation requires write mode"))
        } else {
            Ok(())
        }
    }
}

/// Resolve the CLI data-directory contract to the unified database file.
///
/// `--db-path`, `AETHER_DATA_PATH`, and the install context all describe a
/// directory. Treating a not-yet-created directory as a file silently wrote
/// first-install databases into its parent, so file-path inference is
/// deliberately not supported here.
fn normalise_db_path(input: &Path, _service: &str) -> (PathBuf, PathBuf, bool) {
    let directory = input.to_path_buf();
    let file = directory.join("aether.db");
    (directory, file, false)
}

#[cfg(test)]
mod tests {
    use super::normalise_db_path;

    #[test]
    fn nonexistent_cli_data_path_is_a_directory_not_a_database_file() {
        let workspace = tempfile::tempdir().unwrap();
        let data_directory = workspace.path().join("not-created-yet");

        let (directory, file, explicit_file) = normalise_db_path(&data_directory, "all");

        assert_eq!(directory, data_directory);
        assert_eq!(file, directory.join("aether.db"));
        assert!(!explicit_file);
    }
}
