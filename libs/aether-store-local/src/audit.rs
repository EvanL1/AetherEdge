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

/// Read access to the audit events recorded by [`SqliteAuditSink`].
///
/// Deliberately a separate type from the sink: recording happens on every
/// governed command and must stay minimal, while reading is an operator-facing
/// query that only the interfaces exposing accountability need to hold.
#[cfg(feature = "sqlite-audit")]
#[derive(Clone)]
pub struct SqliteAuditQuery {
    pool: SqlitePool,
}

#[cfg(feature = "sqlite-audit")]
impl SqliteAuditQuery {
    /// Reads the audit events already present in `pool`.
    #[must_use]
    pub const fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }
}

#[cfg(feature = "sqlite-audit")]
#[async_trait]
impl aether_ports::AuditQuery for SqliteAuditQuery {
    async fn query(
        &self,
        filter: &aether_ports::AuditQueryFilter,
    ) -> PortResult<(Vec<AuditRecord>, u64)> {
        // One WHERE clause serves both the page and the total so they can never
        // describe different result sets.
        let mut where_sql = String::from(" WHERE 1 = 1");
        if filter.request_id().is_some() {
            where_sql.push_str(" AND request_id = ?");
        }
        if filter.actor_id().is_some() {
            where_sql.push_str(" AND actor_id = ?");
        }
        if filter.capability().is_some() {
            where_sql.push_str(" AND capability = ?");
        }
        if filter.outcome().is_some() {
            where_sql.push_str(" AND outcome = ?");
        }
        if filter.since().is_some() {
            where_sql.push_str(" AND occurred_at_ms >= ?");
        }
        if filter.until().is_some() {
            where_sql.push_str(" AND occurred_at_ms <= ?");
        }

        use sqlx::Row;

        let count_sql = format!("SELECT COUNT(*) AS total FROM command_audit_events{where_sql}");
        let page_sql = format!(
            "SELECT request_id, actor_id, capability, outcome, occurred_at_ms, detail
             FROM command_audit_events{where_sql}
             ORDER BY occurred_at_ms DESC, id DESC
             LIMIT ? OFFSET ?"
        );

        let total_row = bind_audit_filters(sqlx::query(&count_sql), filter)
            .fetch_one(&self.pool)
            .await
            .map_err(audit_database_error)?;
        let total: i64 = total_row.try_get("total").map_err(audit_database_error)?;

        let rows = bind_audit_filters(sqlx::query(&page_sql), filter)
            .bind(i64::from(filter.limit()))
            .bind(i64::from(filter.offset()))
            .fetch_all(&self.pool)
            .await
            .map_err(audit_database_error)?;

        let records = rows
            .into_iter()
            .map(|row| {
                let outcome: String = row.try_get("outcome").map_err(audit_database_error)?;
                let occurred_at_ms: i64 = row
                    .try_get("occurred_at_ms")
                    .map_err(audit_database_error)?;
                Ok(AuditRecord::new(
                    row.try_get::<String, _>("request_id")
                        .map_err(audit_database_error)?,
                    row.try_get::<String, _>("actor_id")
                        .map_err(audit_database_error)?,
                    row.try_get::<String, _>("capability")
                        .map_err(audit_database_error)?,
                    audit_outcome_from_name(&outcome)?,
                    aether_domain::TimestampMs::new(occurred_at_ms.unsigned_abs()),
                    row.try_get::<Option<String>, _>("detail")
                        .map_err(audit_database_error)?,
                ))
            })
            .collect::<PortResult<Vec<_>>>()?;

        Ok((records, total.unsigned_abs()))
    }
}

/// Binds the filter's constraints in the same order the WHERE clause lists
/// them, so the page query and the count query always describe one result set.
#[cfg(feature = "sqlite-audit")]
fn bind_audit_filters<'q>(
    mut query: sqlx::query::Query<'q, sqlx::Sqlite, sqlx::sqlite::SqliteArguments<'q>>,
    filter: &aether_ports::AuditQueryFilter,
) -> sqlx::query::Query<'q, sqlx::Sqlite, sqlx::sqlite::SqliteArguments<'q>> {
    if let Some(request_id) = filter.request_id() {
        query = query.bind(request_id.to_owned());
    }
    if let Some(actor_id) = filter.actor_id() {
        query = query.bind(actor_id.to_owned());
    }
    if let Some(capability) = filter.capability() {
        query = query.bind(capability.to_owned());
    }
    if let Some(outcome) = filter.outcome() {
        query = query.bind(audit_outcome_name(outcome));
    }
    if let Some(since) = filter.since() {
        query = query.bind(timestamp_bound(since));
    }
    if let Some(until) = filter.until() {
        query = query.bind(timestamp_bound(until));
    }
    query
}

/// Clamps a timestamp into the SQLite INTEGER range used by the stored column.
#[cfg(feature = "sqlite-audit")]
fn timestamp_bound(timestamp: aether_domain::TimestampMs) -> i64 {
    i64::try_from(timestamp.get()).unwrap_or(i64::MAX)
}

/// Restores the outcome an event was recorded with.
///
/// An unrecognised value is a corrupted audit row rather than a benign default:
/// silently reading it as `Rejected` or `Succeeded` would misreport what
/// actually happened, which is the one thing this table exists to prevent.
#[cfg(feature = "sqlite-audit")]
fn audit_outcome_from_name(name: &str) -> PortResult<AuditOutcome> {
    match name {
        "rejected" => Ok(AuditOutcome::Rejected),
        "attempted" => Ok(AuditOutcome::Attempted),
        "succeeded" => Ok(AuditOutcome::Succeeded),
        "failed" => Ok(AuditOutcome::Failed),
        other => Err(PortError::new(
            PortErrorKind::InvalidData,
            format!("audit event stores an unknown outcome '{other}'"),
        )),
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
