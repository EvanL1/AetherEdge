use std::sync::Arc;

use sqlx::SqlitePool;
use tokio::sync::{Mutex, RwLock};

use crate::collector::ShmHistoryCollector;
use crate::config::EnvConfig;
use crate::models::{DataPoint, ServiceConfig, StorageSettings};
use crate::storage::StorageBackend;

/// Shared application state injected into every Axum handler.
pub struct AppState {
    /// Service-owned topology generation, atomically reconciled in background.
    pub collector: Arc<ShmHistoryCollector>,
    /// Active storage backend, wrapped in RwLock so it can be replaced at
    /// runtime via `PUT /hisApi/storage` without restarting the service.
    /// Starts as `NullBackend` when storage is not yet configured.
    pub storage: Arc<RwLock<Arc<dyn StorageBackend>>>,
    /// Shared SQLite pool – used for the `history_config` table
    /// (same database file as alarm / api).
    pub sqlite: SqlitePool,
    /// Static environment config (ports, SHM and embedded storage paths).
    pub env: Arc<EnvConfig>,
    /// Operational config (intervals, patterns) – `/hisApi/config`.
    pub config: Arc<RwLock<ServiceConfig>>,
    /// Storage backend connection settings – `/hisApi/storage`.
    pub storage_settings: Arc<RwLock<StorageSettings>>,
    /// In-memory buffer: collector appends here, scheduler drains + writes.
    pub buffer: Arc<Mutex<Vec<DataPoint>>>,
}
