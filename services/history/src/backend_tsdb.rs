/// TimescaleDB storage backend.
///
/// Delegates all read/write operations to `PostgresBackend`.  The only
/// difference is `init_schema`: after creating the regular `history` table it
/// attempts to convert it into a TimescaleDB *hypertable* partitioned by
/// `time`.  If the TimescaleDB extension is absent the call is silently skipped
/// and the table behaves like plain PostgreSQL.
use async_trait::async_trait;
use chrono::Utc;
use sqlx::{PgPool, Row};
use tracing::{info, warn};

use crate::backend_pg::PostgresBackend;
use crate::models::{DataPoint, DataStats, HistoryRecord, QueryRangeParams, SeriesResult};
use crate::storage::StorageBackend;

pub struct TimescaleDbBackend {
    inner: PostgresBackend,
}

impl TimescaleDbBackend {
    pub fn new(pool: PgPool) -> Self {
        Self {
            inner: PostgresBackend::new(pool),
        }
    }
}

#[async_trait]
impl StorageBackend for TimescaleDbBackend {
    fn name(&self) -> &str {
        "timescaledb"
    }

    async fn init_schema(&self) -> anyhow::Result<()> {
        // Reuse the plain-PG schema creation (table + indexes).
        self.inner.init_schema().await?;

        // Step 1: Convert to hypertable (partitioned by time).
        // This is a no-op if TimescaleDB is not installed; we log a warning and continue.
        let hypertable_ok =
            match sqlx::query("SELECT create_hypertable('history', 'time', if_not_exists => TRUE)")
                .execute(&self.inner.pool)
                .await
            {
                Ok(_) => {
                    info!("TimescaleDB hypertable created (or already existed)");
                    true
                },
                Err(e) => {
                    warn!(
                        "create_hypertable failed – is the TimescaleDB extension installed? ({}). \
                     Falling back to plain PostgreSQL behaviour.",
                        e
                    );
                    false
                },
            };

        if !hypertable_ok {
            return Ok(());
        }

        // Step 2: Enable chunk-level compression.
        // Segment by (series_key, point_id) so that compressed chunks align with
        // the most common query filters.  Order by time DESC to match read patterns.
        // Re-running this on an already-compressed table is harmless.
        match sqlx::query(
            "ALTER TABLE history SET (
                timescaledb.compress,
                timescaledb.compress_segmentby = 'series_key,point_id',
                timescaledb.compress_orderby   = 'time DESC'
            )",
        )
        .execute(&self.inner.pool)
        .await
        {
            Ok(_) => info!("TimescaleDB compression enabled on history table"),
            Err(e) => {
                warn!(
                    "Failed to enable compression on history table ({}). \
                     Compression policy will not be active.",
                    e
                );
                return Ok(());
            },
        }

        // Step 3: Automatically compress chunks older than 7 days.
        // if_not_exists => TRUE makes this idempotent across restarts.
        match sqlx::query(
            "SELECT add_compression_policy('history', INTERVAL '7 days', if_not_exists => TRUE)",
        )
        .execute(&self.inner.pool)
        .await
        {
            Ok(_) => info!("TimescaleDB compression policy set (compress after 7 days)"),
            Err(e) => {
                warn!(
                    "Failed to add compression policy: {}. \
                     Run manually: SELECT add_compression_policy('history', INTERVAL '7 days');",
                    e
                );
            },
        }

        Ok(())
    }

    // All remaining methods delegate to the shared PostgreSQL implementation.

    async fn write_batch(&self, points: Vec<DataPoint>) -> anyhow::Result<usize> {
        self.inner.write_batch(points).await
    }

    async fn query_range(
        &self,
        params: &QueryRangeParams,
        default_page_size: i64,
        max_page_size: i64,
        max_time_range_days: i64,
    ) -> anyhow::Result<(Vec<HistoryRecord>, i64)> {
        self.inner
            .query_range(
                params,
                default_page_size,
                max_page_size,
                max_time_range_days,
            )
            .await
    }

    async fn query_latest(
        &self,
        series_key: &str,
        point_id: &str,
    ) -> anyhow::Result<Option<HistoryRecord>> {
        self.inner.query_latest(series_key, point_id).await
    }

    async fn get_stats(&self) -> anyhow::Result<DataStats> {
        self.inner.get_stats().await
    }

    async fn list_channels(&self) -> anyhow::Result<Vec<String>> {
        self.inner.list_channels().await
    }

    async fn query_batch(
        &self,
        series: &[(String, String)],
        start_time: chrono::DateTime<Utc>,
        end_time: chrono::DateTime<Utc>,
        limit_per_series: i64,
    ) -> anyhow::Result<Vec<SeriesResult>> {
        self.inner
            .query_batch(series, start_time, end_time, limit_per_series)
            .await
    }

    /// TimescaleDB-optimised cleanup: use `drop_chunks` instead of row-level DELETE.
    ///
    /// `drop_chunks` discards entire chunk files atomically, which is orders of
    /// magnitude faster than `DELETE … WHERE time < $1` – especially when the
    /// chunks are already compressed (DELETE would first decompress the chunk).
    ///
    /// Falls back to the PG `DELETE` path if `drop_chunks` is unavailable.
    async fn cleanup_old_data(&self, older_than_days: i32) -> anyhow::Result<u64> {
        let cutoff = Utc::now() - chrono::Duration::days(older_than_days as i64);

        let result = sqlx::query("SELECT count(*) FROM drop_chunks('history', $1::timestamptz)")
            .bind(cutoff)
            .fetch_one(&self.inner.pool)
            .await;

        match result {
            Ok(row) => {
                let dropped: i64 = row.try_get::<i64, _>(0).unwrap_or(0);
                info!(
                    "TimescaleDB cleanup: dropped {} chunk(s) older than {} days",
                    dropped, older_than_days
                );
                // drop_chunks reports chunk count, not row count; return as-is.
                Ok(dropped as u64)
            },
            Err(e) => {
                warn!(
                    "drop_chunks failed ({}), falling back to row-level DELETE",
                    e
                );
                self.inner.cleanup_old_data(older_than_days).await
            },
        }
    }

    async fn health_check(&self) -> bool {
        self.inner.health_check().await
    }
}
