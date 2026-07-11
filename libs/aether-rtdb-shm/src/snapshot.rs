//! Snapshot Manager for Unified Shared Memory
//!
//! Provides automatic periodic snapshots and graceful shutdown snapshot saving.
//!
//! # Architecture
//!
//! ```text
//! ┌─────────────────┐    periodic save     ┌──────────────────────┐
//! │ /dev/shm/*.shm  │ ──────────────────→ │ data/shm-snapshot.bin│
//! │ (volatile mem)  │                      │ (persistent storage) │
//! └─────────────────┘                      └──────────────────────┘
//!         ↑                                         │
//!         │          restore on restart             │
//!         └─────────────────────────────────────────┘
//! ```

use crate::ShmHandle;
use crate::shared_config::DEFAULT_SNAPSHOT_INTERVAL_SECS;
use anyhow::{Context, Result};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::watch;
use tokio::task::JoinHandle;

/// Configuration for snapshot management
#[derive(Debug, Clone)]
pub struct SnapshotConfig {
    /// Path to save snapshots
    pub path: PathBuf,
    /// Interval between automatic snapshots
    pub interval: Duration,
}

impl SnapshotConfig {
    /// Create new snapshot config
    pub fn new(path: impl Into<PathBuf>, interval: Duration) -> Self {
        Self {
            path: path.into(),
            interval,
        }
    }

    /// Create config with default interval (5 minutes)
    pub fn with_default_interval(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            interval: Duration::from_secs(DEFAULT_SNAPSHOT_INTERVAL_SECS),
        }
    }

    /// Create from environment variables
    ///
    /// Reads:
    /// - SHM_SNAPSHOT_PATH: Path to snapshot file (default: data/shm-snapshot.bin)
    /// - SHM_SNAPSHOT_INTERVAL: Interval in seconds (default: 300)
    pub fn from_env() -> Self {
        let path = std::env::var("SHM_SNAPSHOT_PATH")
            .unwrap_or_else(|_| "data/shm-snapshot.bin".to_string());

        let interval_secs = std::env::var("SHM_SNAPSHOT_INTERVAL")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(DEFAULT_SNAPSHOT_INTERVAL_SECS);

        Self {
            path: PathBuf::from(path),
            interval: Duration::from_secs(interval_secs),
        }
    }
}

impl Default for SnapshotConfig {
    fn default() -> Self {
        Self {
            path: PathBuf::from("data/shm-snapshot.bin"),
            interval: Duration::from_secs(DEFAULT_SNAPSHOT_INTERVAL_SECS),
        }
    }
}

/// Snapshot Manager for automatic background snapshots
///
/// Manages periodic snapshot saving and provides manual save capability.
pub struct SnapshotManager {
    shm_handle: Arc<ShmHandle>,
    config: SnapshotConfig,
    shutdown_rx: watch::Receiver<bool>,
}

impl SnapshotManager {
    /// Create new snapshot manager
    ///
    /// # Arguments
    /// - `shm_handle`: Arc to the ShmHandle (always gets latest writer after rebuild)
    /// - `config`: Snapshot configuration
    /// - `shutdown_rx`: Watch receiver for shutdown signal
    pub fn new(
        shm_handle: Arc<ShmHandle>,
        config: SnapshotConfig,
        shutdown_rx: watch::Receiver<bool>,
    ) -> Self {
        Self {
            shm_handle,
            config,
            shutdown_rx,
        }
    }

    /// Start the background snapshot task
    ///
    /// Runs until shutdown signal is received. Saves snapshots at configured interval.
    /// Returns a JoinHandle that can be awaited for task completion.
    pub fn start(self) -> JoinHandle<()> {
        tokio::spawn(async move {
            self.run_snapshot_loop().await;
        })
    }

    /// Run the snapshot loop
    async fn run_snapshot_loop(mut self) {
        let mut interval = tokio::time::interval(self.config.interval);
        // Skip the first immediate tick
        interval.tick().await;

        tracing::info!(
            "SnapshotManager started: interval={:?}, path={:?}",
            self.config.interval,
            self.config.path
        );

        loop {
            tokio::select! {
                // Shutdown signal received
                result = self.shutdown_rx.changed() => {
                    if result.is_err() || *self.shutdown_rx.borrow() {
                        tracing::info!("SnapshotManager: shutdown signal received");
                        // Save final snapshot before exiting
                        match self.save_now() { Err(e) => {
                            tracing::error!("Failed to save final snapshot: {}", e);
                        } _ => {
                            tracing::info!("Final snapshot saved on shutdown");
                        }}
                        break;
                    }
                }
                // Periodic snapshot
                _ = interval.tick() => {
                    match self.save_now() { Err(e) => {
                        tracing::warn!("Periodic snapshot failed: {}", e);
                    } _ => {
                        tracing::debug!("Periodic snapshot saved to {:?}", self.config.path);
                    }}
                }
            }
        }

        tracing::info!("SnapshotManager stopped");
    }

    /// Manually trigger a snapshot save
    ///
    /// Can be called at any time to force an immediate snapshot.
    pub fn save_now(&self) -> Result<()> {
        let layout = self
            .shm_handle
            .layout_arc()
            .context("SHM writer not available for snapshot")?;
        layout
            .writer
            .save_snapshot(&self.config.path)
            .context("Failed to save snapshot")
    }

    /// Get the configured snapshot path
    #[inline]
    pub fn snapshot_path(&self) -> &PathBuf {
        &self.config.path
    }
}

/// Helper function to check if a snapshot file exists
pub fn snapshot_exists(path: &std::path::Path) -> bool {
    path.exists() && path.is_file()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_snapshot_config_default() {
        let config = SnapshotConfig::default();
        assert_eq!(config.path, PathBuf::from("data/shm-snapshot.bin"));
        assert_eq!(config.interval, Duration::from_secs(300));
    }

    #[test]
    fn test_snapshot_config_new() {
        let config = SnapshotConfig::new("/custom/path.bin", Duration::from_secs(60));
        assert_eq!(config.path, PathBuf::from("/custom/path.bin"));
        assert_eq!(config.interval, Duration::from_secs(60));
    }

    #[test]
    fn test_snapshot_exists() {
        // Non-existent file
        assert!(!snapshot_exists(std::path::Path::new(
            "/nonexistent/file.bin"
        )));
    }
}
