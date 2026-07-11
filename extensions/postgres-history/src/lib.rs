//! Optional PostgreSQL implementation of the history capability.

use aether_domain::{PointKind, PointQuality, PointSample};
use aether_ports::{HistorySink, PortError, PortErrorKind, PortResult};
use async_trait::async_trait;
use sqlx::postgres::PgPoolOptions;
use sqlx::{PgPool, Postgres, QueryBuilder};

/// Portable PostgreSQL schema used by the history extension.
pub const HISTORY_SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS aether_history (
    instance_id BIGINT NOT NULL,
    point_kind SMALLINT NOT NULL,
    point_id BIGINT NOT NULL,
    value DOUBLE PRECISION NOT NULL,
    timestamp_ms BIGINT NOT NULL,
    quality SMALLINT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_aether_history_point_time
ON aether_history (instance_id, point_kind, point_id, timestamp_ms DESC);
"#;

/// Database-neutral row mapping exposed for contract tests and bulk importers.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HistoryRow {
    /// Device-model instance identifier.
    pub instance_id: i64,
    /// Stable numeric representation of [`PointKind`].
    pub point_kind: i16,
    /// Point identifier within the instance.
    pub point_id: i64,
    /// Numeric point value.
    pub value: f64,
    /// Unix timestamp in milliseconds.
    pub timestamp_ms: i64,
    /// Stable numeric representation of [`PointQuality`].
    pub quality: i16,
}

/// Maps a domain sample into the extension's stable SQL row.
pub fn sample_to_row(sample: PointSample) -> PortResult<HistoryRow> {
    let timestamp_ms = i64::try_from(sample.timestamp().get()).map_err(|_| {
        PortError::new(
            PortErrorKind::InvalidData,
            "sample timestamp exceeds PostgreSQL BIGINT range",
        )
    })?;

    Ok(HistoryRow {
        instance_id: i64::from(sample.address().instance_id().get()),
        point_kind: kind_code(sample.address().kind()),
        point_id: i64::from(sample.address().point_id().get()),
        value: sample.value(),
        timestamp_ms,
        quality: quality_code(sample.quality()),
    })
}

/// Optional PostgreSQL implementation of [`HistorySink`].
#[derive(Debug, Clone)]
pub struct PostgresHistorySink {
    pool: PgPool,
}

impl PostgresHistorySink {
    /// Connects to PostgreSQL with a bounded pool.
    pub async fn connect(url: &str, max_connections: u32) -> PortResult<Self> {
        let pool = PgPoolOptions::new()
            .max_connections(max_connections)
            .connect(url)
            .await
            .map_err(sqlx_error)?;
        Ok(Self { pool })
    }

    /// Uses a host-managed PostgreSQL pool.
    #[must_use]
    pub const fn from_pool(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Creates the portable history table and indexes.
    pub async fn init_schema(&self) -> PortResult<()> {
        for statement in HISTORY_SCHEMA
            .split(';')
            .map(str::trim)
            .filter(|statement| !statement.is_empty())
        {
            sqlx::query(statement)
                .execute(&self.pool)
                .await
                .map_err(sqlx_error)?;
        }
        Ok(())
    }

    /// Returns the underlying pool for host-specific queries and health checks.
    #[must_use]
    pub const fn pool(&self) -> &PgPool {
        &self.pool
    }
}

#[async_trait]
impl HistorySink for PostgresHistorySink {
    async fn append(&self, samples: &[PointSample]) -> PortResult<usize> {
        if samples.is_empty() {
            return Ok(0);
        }

        let rows = samples
            .iter()
            .copied()
            .map(sample_to_row)
            .collect::<PortResult<Vec<_>>>()?;
        let row_count = rows.len();
        let mut query = QueryBuilder::<Postgres>::new(
            "INSERT INTO aether_history \
             (instance_id, point_kind, point_id, value, timestamp_ms, quality) ",
        );
        query.push_values(rows, |mut values, row| {
            values
                .push_bind(row.instance_id)
                .push_bind(row.point_kind)
                .push_bind(row.point_id)
                .push_bind(row.value)
                .push_bind(row.timestamp_ms)
                .push_bind(row.quality);
        });
        query
            .build()
            .execute(&self.pool)
            .await
            .map_err(sqlx_error)?;
        Ok(row_count)
    }
}

const fn kind_code(kind: PointKind) -> i16 {
    match kind {
        PointKind::Telemetry => 0,
        PointKind::Status => 1,
        PointKind::Command => 2,
        PointKind::Action => 3,
    }
}

const fn quality_code(quality: PointQuality) -> i16 {
    match quality {
        PointQuality::Good => 0,
        PointQuality::Uncertain => 1,
        PointQuality::Bad => 2,
        PointQuality::Unavailable => 3,
    }
}

fn sqlx_error(error: sqlx::Error) -> PortError {
    let kind = if matches!(error, sqlx::Error::Configuration(_)) {
        PortErrorKind::Permanent
    } else {
        PortErrorKind::Unavailable
    };
    PortError::new(kind, format!("PostgreSQL history unavailable: {error}"))
}
