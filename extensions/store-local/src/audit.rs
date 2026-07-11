//! In-memory audit sink for embedded SDK use and tests.

use std::sync::Mutex;

use aether_ports::{AuditRecord, AuditSink, PortResult};
use async_trait::async_trait;

use crate::lock_error;

#[cfg(feature = "sqlite-audit")]
use aether_ports::{AuditOutcome, PortError, PortErrorKind};
#[cfg(feature = "sqlite-audit")]
use sqlx::SqlitePool;

/// Ordered in-memory audit destination.
#[derive(Debug, Default)]
pub struct MemoryAuditSink {
    records: Mutex<Vec<AuditRecord>>,
}

impl MemoryAuditSink {
    /// Creates an empty audit sink.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns a consistent snapshot of audit records.
    pub fn records(&self) -> PortResult<Vec<AuditRecord>> {
        self.records
            .lock()
            .map(|records| records.clone())
            .map_err(|_| lock_error("audit"))
    }
}

#[async_trait]
impl AuditSink for MemoryAuditSink {
    async fn record(&self, record: AuditRecord) -> PortResult<()> {
        self.records
            .lock()
            .map(|mut records| records.push(record))
            .map_err(|_| lock_error("audit"))
    }
}

/// Durable audit destination in an embedded SQLite database.
#[cfg(feature = "sqlite-audit")]
#[derive(Clone)]
pub struct SqliteAuditSink {
    pool: SqlitePool,
}

#[cfg(feature = "sqlite-audit")]
impl SqliteAuditSink {
    /// Creates the local audit schema and returns a ready sink.
    pub async fn initialize(pool: SqlitePool) -> PortResult<Self> {
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS command_audit_events (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                request_id TEXT NOT NULL,
                actor_id TEXT NOT NULL,
                capability TEXT NOT NULL,
                outcome TEXT NOT NULL,
                occurred_at_ms INTEGER NOT NULL,
                detail TEXT,
                created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
            )",
        )
        .execute(&pool)
        .await
        .map_err(audit_database_error)?;
        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_command_audit_request
             ON command_audit_events(request_id, id)",
        )
        .execute(&pool)
        .await
        .map_err(audit_database_error)?;
        Ok(Self { pool })
    }
}

#[cfg(feature = "sqlite-audit")]
#[async_trait]
impl AuditSink for SqliteAuditSink {
    async fn record(&self, record: AuditRecord) -> PortResult<()> {
        let occurred_at_ms = i64::try_from(record.timestamp().get()).map_err(|_| {
            PortError::new(
                PortErrorKind::InvalidData,
                "audit timestamp exceeds SQLite INTEGER range",
            )
        })?;
        sqlx::query(
            "INSERT INTO command_audit_events
             (request_id, actor_id, capability, outcome, occurred_at_ms, detail)
             VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(record.request_id())
        .bind(record.actor_id())
        .bind(record.capability())
        .bind(audit_outcome_name(record.outcome()))
        .bind(occurred_at_ms)
        .bind(record.detail())
        .execute(&self.pool)
        .await
        .map_err(audit_database_error)?;
        Ok(())
    }
}

#[cfg(feature = "sqlite-audit")]
fn audit_outcome_name(outcome: AuditOutcome) -> &'static str {
    match outcome {
        AuditOutcome::Rejected => "rejected",
        AuditOutcome::Attempted => "attempted",
        AuditOutcome::Succeeded => "succeeded",
        AuditOutcome::Failed => "failed",
    }
}

#[cfg(feature = "sqlite-audit")]
fn audit_database_error(error: sqlx::Error) -> PortError {
    PortError::new(
        PortErrorKind::Unavailable,
        format!("local command audit unavailable: {error}"),
    )
}
