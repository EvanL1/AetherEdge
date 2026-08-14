//! Embedded SQLite historical storage used by the default edge profile.

use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};
use sqlx::{QueryBuilder, Sqlite, SqlitePool};
use tracing::error;

use crate::models::{
    DataPoint, DataStats, HistoryRangeQuery, HistoryRecord, SeriesPoint, SeriesResult, fmt_ts,
    source_from_key,
};
use crate::storage::StorageBackend;

pub struct SqliteHistoryBackend {
    pool: SqlitePool,
}

const SQLITE_INSERT_ROWS_PER_STATEMENT: usize = 5_000;
const SQLITE_CLEANUP_ROWS_PER_STATEMENT: u64 = 10_000;

impl SqliteHistoryBackend {
    #[must_use]
    pub const fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    pub async fn connect(path: &str) -> anyhow::Result<Self> {
        if let Some(parent) = std::path::Path::new(path).parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent)?;
        }
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(5)
            .connect_with(common::bootstrap_database::sqlite_connect_options(path))
            .await?;
        Ok(Self::new(pool))
    }

    fn record(
        time_ms: i64,
        series_key: String,
        point_id: String,
        value: Option<f64>,
    ) -> anyhow::Result<HistoryRecord> {
        let time = DateTime::<Utc>::from_timestamp_millis(time_ms)
            .ok_or_else(|| anyhow::anyhow!("invalid stored history timestamp {time_ms}"))?;
        Ok(HistoryRecord {
            timestamp: fmt_ts(&time),
            source: source_from_key(&series_key),
            series_key,
            point_id,
            value,
        })
    }
}

#[async_trait]
impl StorageBackend for SqliteHistoryBackend {
    fn name(&self) -> &str {
        "sqlite"
    }

    async fn init_schema(&self) -> anyhow::Result<()> {
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS history (\
                 time_ms      INTEGER NOT NULL,\
                 ingestion_id TEXT,\
                 series_key   TEXT NOT NULL,\
                 point_id     TEXT NOT NULL,\
                 value        REAL,\
                 string_value TEXT\
             )",
        )
        .execute(&self.pool)
        .await?;
        // Existing edge databases predate retry identities. Keep every legacy
        // row (NULL identities are distinct in SQLite) and migrate in place.
        let has_ingestion_id: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM pragma_table_info('history') WHERE name = 'ingestion_id'",
        )
        .fetch_one(&self.pool)
        .await?;
        if has_ingestion_id == 0 {
            sqlx::query("ALTER TABLE history ADD COLUMN ingestion_id TEXT")
                .execute(&self.pool)
                .await?;
        }
        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_history_key_point_time \
             ON history (series_key, point_id, time_ms DESC)",
        )
        .execute(&self.pool)
        .await?;
        sqlx::query("CREATE INDEX IF NOT EXISTS idx_history_time ON history (time_ms DESC)")
            .execute(&self.pool)
            .await?;
        sqlx::query(
            "CREATE UNIQUE INDEX IF NOT EXISTS idx_history_ingestion_identity \
             ON history (time_ms, ingestion_id)",
        )
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn write_batch(&self, points: Vec<DataPoint>) -> anyhow::Result<usize> {
        if points.is_empty() {
            return Ok(0);
        }
        let count = points.len();
        let mut transaction = self.pool.begin().await?;
        for chunk in points.chunks(SQLITE_INSERT_ROWS_PER_STATEMENT) {
            let mut query = QueryBuilder::<Sqlite>::new(
                "INSERT INTO history \
                 (time_ms, ingestion_id, series_key, point_id, value, string_value) ",
            );
            query.push_values(chunk, |mut values, point| {
                values
                    .push_bind(point.time.timestamp_millis())
                    .push_bind(point.ingestion_id().to_string())
                    .push_bind(&point.series_key)
                    .push_bind(&point.point_id)
                    .push_bind(point.value)
                    .push_bind(&point.string_value);
            });
            // The count returned to the scheduler is an acknowledgement count,
            // so a retry of an already committed identity is successful.
            query.push(" ON CONFLICT(time_ms, ingestion_id) DO NOTHING");
            query.build().execute(&mut *transaction).await?;
        }
        transaction.commit().await?;
        Ok(count)
    }

    async fn query_range(
        &self,
        query: &HistoryRangeQuery,
    ) -> anyhow::Result<(Vec<HistoryRecord>, i64)> {
        let offset = (query.page - 1) * query.page_size;

        let rows: Vec<(i64, String, String, Option<f64>)> = sqlx::query_as(
            "SELECT time_ms, series_key, point_id, value FROM history \
             WHERE series_key = ? AND point_id = ? AND time_ms >= ? AND time_ms <= ? \
             ORDER BY time_ms DESC LIMIT ? OFFSET ?",
        )
        .bind(&query.series_key)
        .bind(&query.point_id)
        .bind(query.start_time.timestamp_millis())
        .bind(query.end_time.timestamp_millis())
        .bind(query.page_size)
        .bind(offset)
        .fetch_all(&self.pool)
        .await?;
        let total: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM history \
             WHERE series_key = ? AND point_id = ? AND time_ms >= ? AND time_ms <= ?",
        )
        .bind(&query.series_key)
        .bind(&query.point_id)
        .bind(query.start_time.timestamp_millis())
        .bind(query.end_time.timestamp_millis())
        .fetch_one(&self.pool)
        .await?;
        let records = rows
            .into_iter()
            .map(|(time, key, point, value)| Self::record(time, key, point, value))
            .collect::<anyhow::Result<Vec<_>>>()?;
        Ok((records, total))
    }

    async fn query_latest(
        &self,
        series_key: &str,
        point_id: &str,
    ) -> anyhow::Result<Option<HistoryRecord>> {
        let row: Option<(i64, String, String, Option<f64>)> = sqlx::query_as(
            "SELECT time_ms, series_key, point_id, value FROM history \
             WHERE series_key = ? AND point_id = ? ORDER BY time_ms DESC LIMIT 1",
        )
        .bind(series_key)
        .bind(point_id)
        .fetch_optional(&self.pool)
        .await?;
        row.map(|(time, key, point, value)| Self::record(time, key, point, value))
            .transpose()
    }

    async fn get_stats(&self) -> anyhow::Result<DataStats> {
        let (earliest, latest, total): (Option<i64>, Option<i64>, i64) =
            sqlx::query_as("SELECT MIN(time_ms), MAX(time_ms), COUNT(*) FROM history")
                .fetch_one(&self.pool)
                .await?;
        let channels = self.list_channels().await?;
        let data_types = crate::models::data_types_from_keys(&channels);
        Ok(DataStats {
            earliest_timestamp: earliest
                .and_then(DateTime::<Utc>::from_timestamp_millis)
                .as_ref()
                .map(fmt_ts),
            latest_timestamp: latest
                .and_then(DateTime::<Utc>::from_timestamp_millis)
                .as_ref()
                .map(fmt_ts),
            total_points: total,
            channels,
            data_types,
        })
    }

    async fn list_channels(&self) -> anyhow::Result<Vec<String>> {
        sqlx::query_scalar("SELECT DISTINCT series_key FROM history ORDER BY series_key")
            .fetch_all(&self.pool)
            .await
            .map_err(Into::into)
    }

    async fn query_batch(
        &self,
        series: &[(String, String)],
        start_time: DateTime<Utc>,
        end_time: DateTime<Utc>,
        limit_per_series: i64,
    ) -> anyhow::Result<Vec<SeriesResult>> {
        if series.is_empty() {
            return Ok(Vec::new());
        }
        let mut query =
            QueryBuilder::<Sqlite>::new("WITH requested(series_order, series_key, point_id) AS (");
        query.push_values(
            series.iter().enumerate(),
            |mut values, (series_order, (series_key, point_id))| {
                values
                    .push_bind(i64::try_from(series_order).unwrap_or(i64::MAX))
                    .push_bind(series_key)
                    .push_bind(point_id);
            },
        );
        query.push(
            "), ranked AS (\
                 SELECT requested.series_order, history.time_ms, history.value, \
                        ROW_NUMBER() OVER (\
                            PARTITION BY requested.series_order ORDER BY history.time_ms ASC\
                        ) AS row_number \
                 FROM requested \
                 JOIN history ON history.series_key = requested.series_key \
                             AND history.point_id = requested.point_id \
                 WHERE history.time_ms >= ",
        );
        query.push_bind(start_time.timestamp_millis());
        query.push(" AND history.time_ms <= ");
        query.push_bind(end_time.timestamp_millis());
        query.push(") SELECT series_order, time_ms, value FROM ranked WHERE row_number <= ");
        query.push_bind(limit_per_series.max(1));
        query.push(" ORDER BY series_order, time_ms ASC");

        let rows: Vec<(i64, i64, Option<f64>)> =
            query.build_query_as().fetch_all(&self.pool).await?;
        let mut result = series
            .iter()
            .map(|(series_key, point_id)| SeriesResult {
                series_key: series_key.clone(),
                point_id: point_id.clone(),
                count: 0,
                data: Vec::new(),
            })
            .collect::<Vec<_>>();
        for (series_order, time_ms, value) in rows {
            let Ok(series_order) = usize::try_from(series_order) else {
                continue;
            };
            let Some(target) = result.get_mut(series_order) else {
                continue;
            };
            if let Some(time) = DateTime::<Utc>::from_timestamp_millis(time_ms) {
                target.data.push(SeriesPoint {
                    time: fmt_ts(&time),
                    value,
                });
                target.count += 1;
            }
        }
        Ok(result)
    }

    async fn cleanup_old_data(&self, older_than_days: i32) -> anyhow::Result<u64> {
        let cutoff = Utc::now() - Duration::days(i64::from(older_than_days));
        let mut deleted = 0;
        loop {
            let affected = sqlx::query(
                "DELETE FROM history WHERE rowid IN (\
                     SELECT rowid FROM history WHERE time_ms < ? LIMIT ?\
                 )",
            )
            .bind(cutoff.timestamp_millis())
            .bind(i64::try_from(SQLITE_CLEANUP_ROWS_PER_STATEMENT).unwrap_or(i64::MAX))
            .execute(&self.pool)
            .await?
            .rows_affected();
            deleted += affected;
            if affected < SQLITE_CLEANUP_ROWS_PER_STATEMENT {
                return Ok(deleted);
            }
            tokio::task::yield_now().await;
        }
    }

    async fn health_check(&self) -> bool {
        sqlx::query("SELECT 1")
            .execute(&self.pool)
            .await
            .map(|_| true)
            .unwrap_or_else(|error| {
                error!("SQLite history health check failed: {error}");
                false
            })
    }
}

#[cfg(test)]
mod tests {
    use chrono::{TimeZone, Utc};

    use super::SqliteHistoryBackend;
    use crate::models::{DataPoint, HistoryRangeQuery};
    use crate::storage::StorageBackend;

    #[tokio::test]
    async fn embedded_backend_roundtrips_history_without_external_service() {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("open embedded history database");
        let backend = SqliteHistoryBackend::new(pool);
        backend.init_schema().await.expect("initialize schema");
        backend
            .write_batch(vec![DataPoint::new(
                Utc.timestamp_millis_opt(1_720_000_000_123)
                    .single()
                    .expect("valid time"),
                "inst:1:M",
                "7",
                Some(42.5),
                None,
            )])
            .await
            .expect("write history");

        let latest = backend
            .query_latest("inst:1:M", "7")
            .await
            .expect("query latest")
            .expect("stored sample");
        let (range, total) = backend
            .query_range(&HistoryRangeQuery {
                series_key: "inst:1:M".to_string(),
                point_id: "7".to_string(),
                start_time: Utc
                    .with_ymd_and_hms(2024, 7, 3, 0, 0, 0)
                    .single()
                    .expect("valid start"),
                end_time: Utc
                    .with_ymd_and_hms(2024, 7, 4, 0, 0, 0)
                    .single()
                    .expect("valid end"),
                page: 1,
                page_size: 100,
            })
            .await
            .expect("query range");

        assert_eq!(latest.value, Some(42.5));
        assert_eq!(range.len(), 1);
        assert_eq!(total, 1);
        assert_eq!(
            backend.list_channels().await.expect("list keys"),
            ["inst:1:M"]
        );
    }

    #[tokio::test]
    async fn batch_query_uses_one_result_group_per_requested_series_and_applies_each_limit() {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("open embedded history database");
        let backend = SqliteHistoryBackend::new(pool);
        backend.init_schema().await.expect("initialize schema");
        let base = Utc
            .timestamp_millis_opt(1_720_000_000_000)
            .single()
            .expect("valid time");
        let mut points = Vec::new();
        for offset in 0..3 {
            points.push(DataPoint::new(
                base + chrono::Duration::milliseconds(offset),
                "inst:1:M",
                "7",
                Some(offset as f64),
                None,
            ));
        }
        points.push(DataPoint::new(base, "inst:2:M", "8", Some(8.0), None));
        backend.write_batch(points).await.expect("write batch");

        let result = backend
            .query_batch(
                &[
                    ("inst:1:M".to_owned(), "7".to_owned()),
                    ("inst:2:M".to_owned(), "8".to_owned()),
                    ("inst:missing:M".to_owned(), "9".to_owned()),
                ],
                base - chrono::Duration::seconds(1),
                base + chrono::Duration::seconds(1),
                2,
            )
            .await
            .expect("query batches");

        assert_eq!(result.len(), 3);
        assert_eq!(result[0].count, 2);
        assert_eq!(result[1].count, 1);
        assert_eq!(result[2].count, 0);
        assert!(result[2].data.is_empty());
    }

    #[tokio::test]
    async fn retry_after_a_lost_commit_response_is_idempotent() {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("open embedded history database");
        let backend = SqliteHistoryBackend::new(pool);
        backend.init_schema().await.expect("initialize schema");
        let time = Utc
            .timestamp_millis_opt(1_720_000_000_123)
            .single()
            .expect("valid time");
        let admitted = DataPoint::new(time, "inst:1:M", "7", Some(42.5), None);

        // Model the ambiguous outcome directly: the first transaction committed,
        // but its acknowledgement was lost, so the scheduler retries the clone.
        assert_eq!(
            backend
                .write_batch(vec![admitted.clone()])
                .await
                .expect("first commit"),
            1
        );
        assert_eq!(
            backend
                .write_batch(vec![admitted])
                .await
                .expect("retry is acknowledged"),
            1
        );

        let stored: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM history")
            .fetch_one(&backend.pool)
            .await
            .expect("count stored rows");
        assert_eq!(stored, 1, "one ingestion identity is stored exactly once");

        // Equal business data admitted twice is not a retry and must not be
        // collapsed merely because its timestamp and value match.
        backend
            .write_batch(vec![
                DataPoint::new(time, "inst:1:M", "7", Some(42.5), None),
                DataPoint::new(time, "inst:1:M", "7", Some(42.5), None),
            ])
            .await
            .expect("write independent admissions");
        let stored: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM history")
            .fetch_one(&backend.pool)
            .await
            .expect("count stored rows");
        assert_eq!(stored, 3, "independent admissions remain independent");
    }

    #[tokio::test]
    async fn an_unrelated_unique_violation_is_not_mistaken_for_an_idempotent_retry() {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("open embedded history database");
        let backend = SqliteHistoryBackend::new(pool);
        backend.init_schema().await.expect("initialize schema");
        sqlx::query(
            "CREATE UNIQUE INDEX test_history_business_constraint \
             ON history (series_key, point_id)",
        )
        .execute(&backend.pool)
        .await
        .expect("add unrelated integrity constraint");
        let time = Utc
            .timestamp_millis_opt(1_720_000_000_123)
            .single()
            .expect("valid time");
        backend
            .write_batch(vec![DataPoint::new(time, "inst:1:M", "7", Some(1.0), None)])
            .await
            .expect("write first row");

        let error = backend
            .write_batch(vec![DataPoint::new(
                time + chrono::Duration::milliseconds(1),
                "inst:1:M",
                "7",
                Some(2.0),
                None,
            )])
            .await
            .expect_err("a non-ingestion uniqueness violation must fail the batch");
        assert!(error.to_string().contains("UNIQUE constraint failed"));
        let stored: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM history")
            .fetch_one(&backend.pool)
            .await
            .expect("count rows after rejected conflict");
        assert_eq!(stored, 1);
    }

    #[tokio::test]
    async fn legacy_schema_is_migrated_without_rewriting_existing_history() {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("open embedded history database");
        sqlx::query(
            "CREATE TABLE history (\
                 time_ms INTEGER NOT NULL,\
                 series_key TEXT NOT NULL,\
                 point_id TEXT NOT NULL,\
                 value REAL,\
                 string_value TEXT\
             )",
        )
        .execute(&pool)
        .await
        .expect("create legacy schema");
        sqlx::query(
            "INSERT INTO history (time_ms, series_key, point_id, value, string_value) \
             VALUES (1720000000123, 'inst:legacy:M', '7', 9.5, NULL)",
        )
        .execute(&pool)
        .await
        .expect("insert legacy row");

        let backend = SqliteHistoryBackend::new(pool);
        backend.init_schema().await.expect("migrate schema");

        let ingestion_id: Option<String> =
            sqlx::query_scalar("SELECT ingestion_id FROM history WHERE series_key = ?")
                .bind("inst:legacy:M")
                .fetch_one(&backend.pool)
                .await
                .expect("read migrated legacy row");
        assert_eq!(
            ingestion_id, None,
            "legacy rows remain present and unmodified"
        );
        let unique_index_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM pragma_index_list('history') \
             WHERE name = 'idx_history_ingestion_identity' AND \"unique\" = 1",
        )
        .fetch_one(&backend.pool)
        .await
        .expect("inspect migrated index");
        assert_eq!(unique_index_count, 1);

        let fresh = DataPoint::new(
            Utc.timestamp_millis_opt(1_720_000_000_124)
                .single()
                .expect("valid time"),
            "inst:fresh:M",
            "8",
            Some(10.5),
            None,
        );
        backend
            .write_batch(vec![fresh.clone()])
            .await
            .expect("write migrated schema");
        backend
            .write_batch(vec![fresh])
            .await
            .expect("retry migrated schema");
        let stored: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM history")
            .fetch_one(&backend.pool)
            .await
            .expect("count retained and new rows");
        assert_eq!(stored, 2, "one legacy row and one fresh identity remain");
    }
}
