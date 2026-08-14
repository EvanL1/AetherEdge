/// TimescaleDB storage backend.
///
/// Delegates all read/write operations to `PostgresBackend`.  The only
/// difference is `init_schema`: after creating the regular `history` table it
/// converts it into a TimescaleDB *hypertable* partitioned by `time`. If the
/// TimescaleDB extension is absent the adapter explicitly reports itself as
/// plain PostgreSQL. If the extension is present, conversion failures are fatal
/// so a configured TimescaleDB backend cannot silently run with the wrong
/// storage semantics.
use std::sync::atomic::{AtomicBool, Ordering};

use async_trait::async_trait;
use chrono::Utc;
use sqlx::{PgPool, Row};
use tracing::{info, warn};

use crate::backend_pg::PostgresBackend;
use crate::models::{DataPoint, DataStats, HistoryRangeQuery, HistoryRecord, SeriesResult};
use crate::storage::StorageBackend;

pub struct TimescaleDbBackend {
    inner: PostgresBackend,
    timescale_active: AtomicBool,
}

const CREATE_HYPERTABLE_SQL: &str =
    "SELECT create_hypertable('history', 'time', if_not_exists => TRUE, migrate_data => TRUE)";

impl TimescaleDbBackend {
    pub fn new(pool: PgPool) -> Self {
        Self {
            inner: PostgresBackend::new(pool),
            timescale_active: AtomicBool::new(false),
        }
    }

    fn timescale_is_active(&self) -> bool {
        self.timescale_active.load(Ordering::Acquire)
    }
}

#[async_trait]
impl StorageBackend for TimescaleDbBackend {
    fn name(&self) -> &str {
        if self.timescale_is_active() {
            "timescaledb"
        } else {
            "postgres"
        }
    }

    async fn init_schema(&self) -> anyhow::Result<()> {
        // Reuse the plain-PG schema creation (table + indexes).
        self.inner.init_schema().await?;

        // Step 1: distinguish an unavailable optional extension from a broken
        // migration. Only the former is an allowed plain-PostgreSQL fallback.
        let extension_available: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM pg_extension WHERE extname = 'timescaledb')",
        )
        .fetch_one(&self.inner.pool)
        .await?;
        if !extension_available {
            self.timescale_active.store(false, Ordering::Release);
            warn!("TimescaleDB extension is not installed; using and reporting plain PostgreSQL");
            return Ok(());
        }

        // A regular table may already contain legacy history. TimescaleDB
        // rejects non-empty tables unless migrate_data is explicitly enabled.
        sqlx::query(CREATE_HYPERTABLE_SQL)
            .execute(&self.inner.pool)
            .await
            .map_err(|error| {
                anyhow::anyhow!(
                    "TimescaleDB extension is installed but history hypertable migration failed: {error}"
                )
            })?;
        self.timescale_active.store(true, Ordering::Release);
        info!("TimescaleDB hypertable created (or already existed)");

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
        query: &HistoryRangeQuery,
    ) -> anyhow::Result<(Vec<HistoryRecord>, i64)> {
        self.inner.query_range(query).await
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
        if !self.timescale_is_active() {
            return self.inner.cleanup_old_data(older_than_days).await;
        }

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

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{DateTime, Utc};

    fn lazy_backend() -> TimescaleDbBackend {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .connect_lazy("postgres://history:history@127.0.0.1/history")
            .expect("test DSN is syntactically valid");
        TimescaleDbBackend::new(pool)
    }

    async fn private_timescale_pool(schema: &'static str) -> PgPool {
        let dsn = std::env::var("AETHER_TEST_TSDB_DSN")
            .expect("AETHER_TEST_TSDB_DSN must point at a disposable TimescaleDB server");
        let admin = sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .connect(&dsn)
            .await
            .expect("connect to TimescaleDB");
        for statement in [
            format!("DROP SCHEMA IF EXISTS {schema} CASCADE"),
            format!("CREATE SCHEMA {schema}"),
        ] {
            sqlx::query(&statement)
                .execute(&admin)
                .await
                .expect("prepare private TimescaleDB schema");
        }
        admin.close().await;

        sqlx::postgres::PgPoolOptions::new()
            .max_connections(2)
            .after_connect(move |connection, _meta| {
                Box::pin(async move {
                    sqlx::query(&format!("SET search_path TO {schema}, public"))
                        .execute(connection)
                        .await?;
                    Ok(())
                })
            })
            .connect(&dsn)
            .await
            .expect("connect to private TimescaleDB schema")
    }

    #[test]
    fn non_empty_table_conversion_enables_data_migration() {
        assert!(CREATE_HYPERTABLE_SQL.contains("migrate_data => TRUE"));
        assert!(CREATE_HYPERTABLE_SQL.contains("if_not_exists => TRUE"));
    }

    #[tokio::test]
    async fn adapter_reports_postgres_until_hypertable_conversion_succeeds() {
        let backend = lazy_backend();
        assert_eq!(backend.name(), "postgres");

        backend.timescale_active.store(true, Ordering::Release);
        assert_eq!(backend.name(), "timescaledb");
    }

    #[tokio::test]
    #[ignore = "requires TimescaleDB (AETHER_TEST_TSDB_DSN)"]
    async fn non_empty_legacy_table_becomes_a_hypertable_and_retries_exactly_once() {
        const SCHEMA: &str = "t_timescale_ingestion_migration";
        let pool = private_timescale_pool(SCHEMA).await;
        sqlx::query(
            "CREATE TABLE history (
                time TIMESTAMPTZ NOT NULL,
                series_key TEXT NOT NULL,
                point_id TEXT NOT NULL,
                value DOUBLE PRECISION,
                string_value TEXT
            )",
        )
        .execute(&pool)
        .await
        .expect("create non-empty legacy history table");
        sqlx::query(
            "INSERT INTO history (time, series_key, point_id, value, string_value)
             VALUES
                 (to_timestamp(1700000000), 'inst:legacy:M', '7', 9.5, NULL),
                 (to_timestamp(1700000000), 'inst:legacy:M', '8', 10.5, NULL)",
        )
        .execute(&pool)
        .await
        .expect("insert same-timestamp legacy history");

        let backend = TimescaleDbBackend::new(pool);
        backend
            .init_schema()
            .await
            .expect("migrate non-empty history to a hypertable");
        assert_eq!(backend.name(), "timescaledb");

        let is_hypertable: bool = sqlx::query_scalar(
            "SELECT EXISTS (
                 SELECT 1 FROM timescaledb_information.hypertables
                 WHERE hypertable_schema = current_schema()
                   AND hypertable_name = 'history'
             )",
        )
        .fetch_one(&backend.inner.pool)
        .await
        .expect("inspect TimescaleDB catalog");
        assert!(is_hypertable);

        let legacy_rows: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM history
             WHERE series_key = 'inst:legacy:M' AND ingestion_id IS NULL",
        )
        .fetch_one(&backend.inner.pool)
        .await
        .expect("count preserved legacy rows");
        assert_eq!(legacy_rows, 2);

        let fresh = DataPoint::new(
            DateTime::<Utc>::from_timestamp(1_700_000_001, 0).expect("representable"),
            "inst:fresh:M",
            "9",
            Some(11.5),
            None,
        );
        backend
            .write_batch(vec![fresh.clone()])
            .await
            .expect("write fresh history");
        backend
            .write_batch(vec![fresh])
            .await
            .expect("retry committed history");
        let total: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM history")
            .fetch_one(&backend.inner.pool)
            .await
            .expect("count migrated and retried rows");
        assert_eq!(total, 3);
    }

    #[tokio::test]
    #[ignore = "requires TimescaleDB (AETHER_TEST_TSDB_DSN)"]
    async fn installed_extension_does_not_hide_a_failed_hypertable_migration() {
        let pool = private_timescale_pool("t_timescale_failed_migration").await;
        sqlx::query(
            "CREATE TABLE history (
                time TIMESTAMPTZ NOT NULL,
                series_key TEXT NOT NULL,
                point_id TEXT NOT NULL,
                value DOUBLE PRECISION,
                string_value TEXT
            )",
        )
        .execute(&pool)
        .await
        .expect("create legacy history table");
        sqlx::query("CREATE UNIQUE INDEX incompatible_history_key ON history (series_key)")
            .execute(&pool)
            .await
            .expect("create an index incompatible with time partitioning");

        let backend = TimescaleDbBackend::new(pool);
        let error = backend
            .init_schema()
            .await
            .expect_err("an installed extension must fail closed on migration errors");

        assert!(error.to_string().contains("hypertable migration failed"));
        assert_eq!(backend.name(), "postgres");
    }
}
