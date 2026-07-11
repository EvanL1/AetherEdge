//! Embedded SQLite historical storage used by the default edge profile.

use std::collections::HashSet;

use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};
use sqlx::SqlitePool;
use tracing::error;

use crate::models::{
    DataPoint, DataStats, HistoryRecord, QueryRangeParams, SeriesPoint, SeriesResult, fmt_ts,
    parse_time, source_from_key,
};
use crate::storage::StorageBackend;

pub struct SqliteHistoryBackend {
    pool: SqlitePool,
}

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
                 series_key   TEXT NOT NULL,\
                 point_id     TEXT NOT NULL,\
                 value        REAL,\
                 string_value TEXT\
             )",
        )
        .execute(&self.pool)
        .await?;
        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_history_key_point_time \
             ON history (series_key, point_id, time_ms DESC)",
        )
        .execute(&self.pool)
        .await?;
        sqlx::query("CREATE INDEX IF NOT EXISTS idx_history_time ON history (time_ms DESC)")
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
        for point in points {
            sqlx::query(
                "INSERT INTO history \
                 (time_ms, series_key, point_id, value, string_value) VALUES (?, ?, ?, ?, ?)",
            )
            .bind(point.time.timestamp_millis())
            .bind(point.series_key)
            .bind(point.point_id)
            .bind(point.value)
            .bind(point.string_value)
            .execute(&mut *transaction)
            .await?;
        }
        transaction.commit().await?;
        Ok(count)
    }

    async fn query_range(
        &self,
        params: &QueryRangeParams,
        default_page_size: i64,
        max_page_size: i64,
        max_time_range_days: i64,
    ) -> anyhow::Result<(Vec<HistoryRecord>, i64)> {
        let page = params.page.unwrap_or(1).max(1);
        let page_size = params
            .page_size
            .unwrap_or(default_page_size)
            .clamp(1, max_page_size.max(1));
        let offset = (page - 1) * page_size;
        let end = params
            .end_time
            .as_deref()
            .map(parse_time)
            .transpose()?
            .unwrap_or_else(Utc::now);
        let requested_start = params
            .start_time
            .as_deref()
            .map(parse_time)
            .transpose()?
            .unwrap_or_else(|| end - Duration::hours(24));
        let start = requested_start.max(end - Duration::days(max_time_range_days));

        let rows: Vec<(i64, String, String, Option<f64>)> = sqlx::query_as(
            "SELECT time_ms, series_key, point_id, value FROM history \
             WHERE series_key = ? AND point_id = ? AND time_ms >= ? AND time_ms <= ? \
             ORDER BY time_ms DESC LIMIT ? OFFSET ?",
        )
        .bind(&params.series_key)
        .bind(&params.point_id)
        .bind(start.timestamp_millis())
        .bind(end.timestamp_millis())
        .bind(page_size)
        .bind(offset)
        .fetch_all(&self.pool)
        .await?;
        let total: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM history \
             WHERE series_key = ? AND point_id = ? AND time_ms >= ? AND time_ms <= ?",
        )
        .bind(&params.series_key)
        .bind(&params.point_id)
        .bind(start.timestamp_millis())
        .bind(end.timestamp_millis())
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
        let data_types = channels
            .iter()
            .filter_map(|key| key.rsplit(':').next().map(String::from))
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();
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
        let mut result = Vec::with_capacity(series.len());
        for (key, point_id) in series {
            let rows: Vec<(i64, Option<f64>)> = sqlx::query_as(
                "SELECT time_ms, value FROM history \
                 WHERE series_key = ? AND point_id = ? AND time_ms >= ? AND time_ms <= ? \
                 ORDER BY time_ms ASC LIMIT ?",
            )
            .bind(key)
            .bind(point_id)
            .bind(start_time.timestamp_millis())
            .bind(end_time.timestamp_millis())
            .bind(limit_per_series.max(1))
            .fetch_all(&self.pool)
            .await?;
            let data = rows
                .into_iter()
                .filter_map(|(time_ms, value)| {
                    DateTime::<Utc>::from_timestamp_millis(time_ms).map(|time| SeriesPoint {
                        time: fmt_ts(&time),
                        value,
                    })
                })
                .collect::<Vec<_>>();
            result.push(SeriesResult {
                series_key: key.clone(),
                point_id: point_id.clone(),
                count: data.len(),
                data,
            });
        }
        Ok(result)
    }

    async fn cleanup_old_data(&self, older_than_days: i32) -> anyhow::Result<u64> {
        let cutoff = Utc::now() - Duration::days(i64::from(older_than_days));
        Ok(sqlx::query("DELETE FROM history WHERE time_ms < ?")
            .bind(cutoff.timestamp_millis())
            .execute(&self.pool)
            .await?
            .rows_affected())
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
    use crate::models::{DataPoint, QueryRangeParams};
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
            .write_batch(vec![DataPoint {
                time: Utc
                    .timestamp_millis_opt(1_720_000_000_123)
                    .single()
                    .expect("valid time"),
                series_key: "inst:1:M".to_string(),
                point_id: "7".to_string(),
                value: Some(42.5),
                string_value: None,
            }])
            .await
            .expect("write history");

        let latest = backend
            .query_latest("inst:1:M", "7")
            .await
            .expect("query latest")
            .expect("stored sample");
        let (range, total) = backend
            .query_range(
                &QueryRangeParams {
                    series_key: "inst:1:M".to_string(),
                    point_id: "7".to_string(),
                    start_time: Some("2024-07-03T00:00:00Z".to_string()),
                    end_time: Some("2024-07-04T00:00:00Z".to_string()),
                    page: None,
                    page_size: None,
                },
                100,
                1_000,
                365,
            )
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
}
