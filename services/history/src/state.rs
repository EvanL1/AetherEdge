use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

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
    /// Cheap process-local counters for the write path. Database-wide totals
    /// remain owned by the active backend.
    pub runtime_metrics: Arc<HistoryRuntimeMetrics>,
}

#[derive(Default)]
pub struct HistoryRuntimeMetrics {
    points_written: AtomicU64,
    points_dropped: AtomicU64,
    flush_failures: AtomicU64,
    flush_timeouts: AtomicU64,
    last_flush_duration_ms: AtomicU64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HistoryRuntimeSnapshot {
    pub points_written: u64,
    pub points_dropped: u64,
    pub flush_failures: u64,
    pub flush_timeouts: u64,
    pub last_flush_duration_ms: u64,
}

impl HistoryRuntimeMetrics {
    pub fn record_written(&self, count: usize) {
        self.points_written
            .fetch_add(u64::try_from(count).unwrap_or(u64::MAX), Ordering::Relaxed);
    }

    pub fn record_dropped(&self, count: usize) {
        self.points_dropped
            .fetch_add(u64::try_from(count).unwrap_or(u64::MAX), Ordering::Relaxed);
    }

    pub fn record_flush_failure(&self) {
        self.flush_failures.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_flush_timeout(&self) {
        self.flush_timeouts.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_flush_duration(&self, duration: std::time::Duration) {
        self.last_flush_duration_ms.store(
            u64::try_from(duration.as_millis()).unwrap_or(u64::MAX),
            Ordering::Relaxed,
        );
    }

    #[must_use]
    pub fn snapshot(&self) -> HistoryRuntimeSnapshot {
        HistoryRuntimeSnapshot {
            points_written: self.points_written.load(Ordering::Relaxed),
            points_dropped: self.points_dropped.load(Ordering::Relaxed),
            flush_failures: self.flush_failures.load(Ordering::Relaxed),
            flush_timeouts: self.flush_timeouts.load(Ordering::Relaxed),
            last_flush_duration_ms: self.last_flush_duration_ms.load(Ordering::Relaxed),
        }
    }
}
