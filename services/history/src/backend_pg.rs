/// Plain PostgreSQL storage backend.
///
/// Uses a regular `history` table with B-tree indexes. No TimescaleDB
/// extension required. For deployments with TimescaleDB installed, prefer
/// `backend_tsdb.rs` which converts the table to a hypertable.
use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};
use sqlx::PgPool;
use tracing::{error, info};

use crate::models::{
    DataPoint, DataStats, HistoryRangeQuery, HistoryRecord, SeriesPoint, SeriesResult, fmt_ts,
    source_from_key,
};
use crate::storage::StorageBackend;

/// Fan fetched rows back out to the requested series, in request order.
///
/// Looked up rather than removed: a request may legitimately name the same
/// (`series_key`, `point_id`) twice, and consuming the entry left every repeat
/// with an empty series — indistinguishable from "this point has no history".
fn group_batch_rows(
    series: &[(String, String)],
    rows: &[(DateTime<Utc>, String, String, Option<f64>)],
) -> Vec<SeriesResult> {
    let mut grouped: std::collections::HashMap<(&str, &str), Vec<SeriesPoint>> =
        std::collections::HashMap::new();
    for (time, key, point_id, value) in rows {
        grouped
            .entry((key.as_str(), point_id.as_str()))
            .or_default()
            .push(SeriesPoint {
                time: fmt_ts(time),
                value: *value,
            });
    }

    series
        .iter()
        .map(|(key, point_id)| {
            let data = grouped
                .get(&(key.as_str(), point_id.as_str()))
                .cloned()
                .unwrap_or_default();
            SeriesResult {
                series_key: key.clone(),
                point_id: point_id.clone(),
                count: data.len(),
                data,
            }
        })
        .collect()
}

pub struct PostgresBackend {
    pub pool: PgPool,
}

impl PostgresBackend {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl StorageBackend for PostgresBackend {
    fn name(&self) -> &str {
        "postgres"
    }

    async fn init_schema(&self) -> anyhow::Result<()> {
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS history (
                time         TIMESTAMPTZ NOT NULL,
                series_key   TEXT NOT NULL,
                point_id     TEXT NOT NULL,
                value        DOUBLE PRECISION,
                string_value TEXT
            )",
        )
        .execute(&self.pool)
        .await?;

        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_history_key_point_time
             ON history (series_key, point_id, time DESC)",
        )
        .execute(&self.pool)
        .await?;

        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_history_time
             ON history (time DESC)",
        )
        .execute(&self.pool)
        .await?;

        info!("PostgreSQL backend schema initialized");
        Ok(())
    }

    async fn write_batch(&self, points: Vec<DataPoint>) -> anyhow::Result<usize> {
        if points.is_empty() {
            return Ok(0);
        }
        let len = points.len();

        let times: Vec<DateTime<Utc>> = points.iter().map(|p| p.time).collect();
        let keys: Vec<&str> = points.iter().map(|p| p.series_key.as_str()).collect();
        let pids: Vec<&str> = points.iter().map(|p| p.point_id.as_str()).collect();
        let values: Vec<Option<f64>> = points.iter().map(|p| p.value).collect();
        let svalues: Vec<Option<&str>> = points.iter().map(|p| p.string_value.as_deref()).collect();

        sqlx::query(
            "INSERT INTO history (time, series_key, point_id, value, string_value)
             SELECT * FROM UNNEST(
                 $1::TIMESTAMPTZ[],
                 $2::TEXT[],
                 $3::TEXT[],
                 $4::FLOAT8[],
                 $5::TEXT[]
             )",
        )
        .bind(times)
        .bind(keys)
        .bind(pids)
        .bind(values)
        .bind(svalues)
        .execute(&self.pool)
        .await?;

        Ok(len)
    }

    async fn query_range(
        &self,
        query: &HistoryRangeQuery,
    ) -> anyhow::Result<(Vec<HistoryRecord>, i64)> {
        let offset = (query.page - 1) * query.page_size;

        struct Row {
            time: DateTime<Utc>,
            series_key: String,
            point_id: String,
            value: Option<f64>,
        }

        let rows = sqlx::query_as::<_, (DateTime<Utc>, String, String, Option<f64>)>(
            "SELECT time, series_key, point_id, value
             FROM history
             WHERE series_key = $1 AND point_id = $2
               AND time >= $3 AND time <= $4
             ORDER BY time DESC
             LIMIT $5 OFFSET $6",
        )
        .bind(&query.series_key)
        .bind(&query.point_id)
        .bind(query.start_time)
        .bind(query.end_time)
        .bind(query.page_size)
        .bind(offset)
        .fetch_all(&self.pool)
        .await?
        .into_iter()
        .map(|(time, series_key, point_id, value)| Row {
            time,
            series_key,
            point_id,
            value,
        })
        .collect::<Vec<_>>();

        let total: i64 = sqlx::query_as::<_, (i64,)>(
            "SELECT COUNT(*)
             FROM history
             WHERE series_key = $1 AND point_id = $2
               AND time >= $3 AND time <= $4",
        )
        .bind(&query.series_key)
        .bind(&query.point_id)
        .bind(query.start_time)
        .bind(query.end_time)
        .fetch_one(&self.pool)
        .await
        .map(|(n,)| n)?;

        let records = rows
            .into_iter()
            .map(|r| HistoryRecord {
                timestamp: fmt_ts(&r.time),
                source: source_from_key(&r.series_key),
                series_key: r.series_key,
                point_id: r.point_id,
                value: r.value,
            })
            .collect();

        Ok((records, total))
    }

    async fn query_latest(
        &self,
        series_key: &str,
        point_id: &str,
    ) -> anyhow::Result<Option<HistoryRecord>> {
        let row = sqlx::query_as::<_, (DateTime<Utc>, String, String, Option<f64>)>(
            "SELECT time, series_key, point_id, value
             FROM history
             WHERE series_key = $1 AND point_id = $2
             ORDER BY time DESC
             LIMIT 1",
        )
        .bind(series_key)
        .bind(point_id)
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.map(|(time, rk, pid, value)| HistoryRecord {
            timestamp: fmt_ts(&time),
            source: source_from_key(&rk),
            series_key: rk,
            point_id: pid,
            value,
        }))
    }

    async fn get_stats(&self) -> anyhow::Result<DataStats> {
        let (earliest, latest, total): (Option<DateTime<Utc>>, Option<DateTime<Utc>>, Option<i64>) =
            sqlx::query_as(
                "SELECT MIN(time), MAX(time), COUNT(*)
                 FROM history",
            )
            .fetch_one(&self.pool)
            .await?;

        let channels = self.list_channels().await?;
        let data_types = crate::models::data_types_from_keys(&channels);

        Ok(DataStats {
            earliest_timestamp: earliest.as_ref().map(fmt_ts),
            latest_timestamp: latest.as_ref().map(fmt_ts),
            total_points: total.unwrap_or(0),
            channels,
            data_types,
        })
    }

    async fn list_channels(&self) -> anyhow::Result<Vec<String>> {
        let rows: Vec<(String,)> =
            sqlx::query_as("SELECT DISTINCT series_key FROM history ORDER BY series_key")
                .fetch_all(&self.pool)
                .await?;
        Ok(rows.into_iter().map(|(k,)| k).collect())
    }

    async fn query_batch(
        &self,
        series: &[(String, String)],
        start_time: DateTime<Utc>,
        end_time: DateTime<Utc>,
        limit_per_series: i64,
    ) -> anyhow::Result<Vec<SeriesResult>> {
        if series.is_empty() {
            return Ok(vec![]);
        }

        let keys: Vec<&str> = series.iter().map(|(k, _)| k.as_str()).collect();
        let pids: Vec<&str> = series.iter().map(|(_, p)| p.as_str()).collect();

        // Single query with ROW_NUMBER() window function to enforce per-series limit.
        // UNNEST($3, $4) produces a set of (series_key, point_id) pairs that act as
        // an IN-filter, avoiding N round-trips while still bounding result size.
        let rows = sqlx::query_as::<_, (DateTime<Utc>, String, String, Option<f64>)>(
            "SELECT time, series_key, point_id, value
             FROM (
                 SELECT time, series_key, point_id, value,
                        ROW_NUMBER() OVER (
                            PARTITION BY series_key, point_id
                            ORDER BY time ASC
                        ) AS rn
                 FROM history
                 WHERE time >= $1 AND time <= $2
                   AND (series_key, point_id) IN (
                       SELECT * FROM UNNEST($3::TEXT[], $4::TEXT[])
                   )
             ) sub
             WHERE rn <= $5
             ORDER BY series_key, point_id, time ASC",
        )
        .bind(start_time)
        .bind(end_time)
        .bind(&keys)
        .bind(&pids)
        .bind(limit_per_series)
        .fetch_all(&self.pool)
        .await?;

        Ok(group_batch_rows(series, &rows))
    }

    async fn cleanup_old_data(&self, older_than_days: i32) -> anyhow::Result<u64> {
        let cutoff = Utc::now() - Duration::days(older_than_days as i64);
        let result = sqlx::query("DELETE FROM history WHERE time < $1")
            .bind(cutoff)
            .execute(&self.pool)
            .await?;
        let deleted = result.rows_affected();
        info!(
            "Cleanup: deleted {} rows older than {} days",
            deleted, older_than_days
        );
        Ok(deleted)
    }

    async fn health_check(&self) -> bool {
        sqlx::query("SELECT 1")
            .execute(&self.pool)
            .await
            .map(|_| true)
            .unwrap_or_else(|e| {
                error!("PostgreSQL health check failed: {}", e);
                false
            })
    }
}

#[cfg(test)]
mod batch_grouping_tests {
    use super::*;

    fn row(point_id: &str, value: f64) -> (DateTime<Utc>, String, String, Option<f64>) {
        (
            DateTime::<Utc>::from_timestamp(1_700_000_000, 0).expect("representable"),
            "inst:1:M".to_string(),
            point_id.to_string(),
            Some(value),
        )
    }

    fn requested(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(k, p)| ((*k).to_string(), (*p).to_string()))
            .collect()
    }

    #[test]
    fn each_requested_series_receives_its_rows() {
        let series = requested(&[("inst:1:M", "7"), ("inst:1:M", "8")]);
        let rows = vec![row("7", 1.0), row("8", 2.0)];

        let results = group_batch_rows(&series, &rows);

        assert_eq!(results.len(), 2);
        assert_eq!(results[0].count, 1);
        assert_eq!(results[1].count, 1);
    }

    #[test]
    fn a_repeated_series_item_is_answered_twice_not_emptied() {
        // `map.remove` handed the rows to the first occurrence and left duplicates
        // with an empty vector, so the same point looked both present and absent
        // in one response. SQLite queries each item independently and does not.
        let series = requested(&[("inst:1:M", "7"), ("inst:1:M", "7")]);
        let rows = vec![row("7", 1.0)];

        let results = group_batch_rows(&series, &rows);

        assert_eq!(results.len(), 2);
        assert_eq!(results[0].count, 1);
        assert_eq!(
            results[1].count, 1,
            "a duplicated request item must not read as 'no history for this point'"
        );
    }

    #[test]
    fn a_series_with_no_rows_is_reported_as_empty() {
        let series = requested(&[("inst:1:M", "9")]);

        let results = group_batch_rows(&series, &[]);

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].count, 0);
    }
}

/// Integration coverage for the PostgreSQL backend.
///
/// Opt-in: these need a live server, so they are `#[ignore]`d and read their
/// DSN from `AETHER_TEST_PG_DSN`. Run them with a throwaway server:
///
/// ```sh
/// docker run -d --rm --name aether-pg-test -e POSTGRES_PASSWORD=aether-test \
///     -e POSTGRES_DB=history -p 5432:5432 postgres:17-alpine
/// AETHER_TEST_PG_DSN=postgres://postgres:aether-test@127.0.0.1:5432/history \
///     cargo test -p aether-history --features postgres-storage --bins -- --ignored
/// ```
#[cfg(test)]
mod postgres_integration_tests {
    use super::*;
    use crate::models::DataPoint;

    /// Connect and hand back a backend owning a private, empty schema.
    ///
    /// One schema per test: these run concurrently and one of them drops its
    /// table, which would otherwise pull the rest down with it.
    async fn backend(schema: &'static str) -> PostgresBackend {
        let dsn = std::env::var("AETHER_TEST_PG_DSN")
            .expect("AETHER_TEST_PG_DSN must point at a disposable PostgreSQL server");

        let admin = sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .connect(&dsn)
            .await
            .expect("connect to the test server");
        for statement in [
            format!("DROP SCHEMA IF EXISTS {schema} CASCADE"),
            format!("CREATE SCHEMA {schema}"),
        ] {
            sqlx::query(&statement)
                .execute(&admin)
                .await
                .expect("prepare a private schema");
        }
        admin.close().await;

        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(2)
            .after_connect(move |conn, _meta| {
                Box::pin(async move {
                    sqlx::query(&format!("SET search_path TO {schema}"))
                        .execute(conn)
                        .await?;
                    Ok(())
                })
            })
            .connect(&dsn)
            .await
            .expect("connect to the test server");

        let backend = PostgresBackend::new(pool);
        backend.init_schema().await.expect("schema");
        backend
    }

    fn point(series_key: &str, point_id: &str, value: f64, offset_secs: i64) -> DataPoint {
        DataPoint {
            time: DateTime::<Utc>::from_timestamp(1_700_000_000 + offset_secs, 0)
                .expect("representable"),
            series_key: series_key.to_string(),
            point_id: point_id.to_string(),
            value: Some(value),
            string_value: None,
        }
    }

    fn window() -> (DateTime<Utc>, DateTime<Utc>) {
        (
            DateTime::<Utc>::from_timestamp(1_600_000_000, 0).expect("representable"),
            DateTime::<Utc>::from_timestamp(1_800_000_000, 0).expect("representable"),
        )
    }

    #[tokio::test]
    #[ignore = "requires a PostgreSQL server (AETHER_TEST_PG_DSN)"]
    async fn history_survives_a_write_and_paged_read() {
        let backend = backend("t_roundtrip").await;
        let written = backend
            .write_batch(vec![
                point("inst:1:M", "7", 1.0, 0),
                point("inst:1:M", "7", 2.0, 1),
                point("inst:1:M", "7", 3.0, 2),
            ])
            .await
            .expect("write");
        assert_eq!(written, 3);

        let (start_time, end_time) = window();
        let (records, total) = backend
            .query_range(&HistoryRangeQuery {
                series_key: "inst:1:M".to_string(),
                point_id: "7".to_string(),
                start_time,
                end_time,
                page: 1,
                page_size: 2,
            })
            .await
            .expect("query");

        assert_eq!(records.len(), 2, "page_size bounds the page");
        assert_eq!(
            total, 3,
            "total counts every matching row, not just the page"
        );
        assert_eq!(records[0].source, "inst");
    }

    #[tokio::test]
    #[ignore = "requires a PostgreSQL server (AETHER_TEST_PG_DSN)"]
    async fn stats_report_type_codes_not_source_prefixes() {
        // The PostgreSQL implementation used to take the *first* key segment here
        // while SQLite took the last, so `data_types` returned sources on one
        // backend and types on the other for the same request.
        let backend = backend("t_stats").await;
        backend
            .write_batch(vec![
                point("inst:1:M", "7", 1.0, 0),
                point("inst:2:A", "8", 2.0, 1),
                point("io:3:M", "9", 3.0, 2),
            ])
            .await
            .expect("write");

        let stats = backend.get_stats().await.expect("stats");

        let mut data_types = stats.data_types;
        data_types.sort();
        assert_eq!(data_types, vec!["A".to_string(), "M".to_string()]);
        assert_eq!(stats.total_points, 3);
        assert_eq!(stats.channels.len(), 3);
    }

    #[tokio::test]
    #[ignore = "requires a PostgreSQL server (AETHER_TEST_PG_DSN)"]
    async fn a_repeated_batch_item_is_answered_twice() {
        let backend = backend("t_batch_dup").await;
        backend
            .write_batch(vec![point("inst:1:M", "7", 1.0, 0)])
            .await
            .expect("write");

        let (start_time, end_time) = window();
        let series = vec![
            ("inst:1:M".to_string(), "7".to_string()),
            ("inst:1:M".to_string(), "7".to_string()),
        ];
        let results = backend
            .query_batch(&series, start_time, end_time, 100)
            .await
            .expect("batch query");

        assert_eq!(results.len(), 2);
        assert_eq!(results[0].count, 1);
        assert_eq!(
            results[1].count, 1,
            "a duplicated request item must not read as 'no history for this point'"
        );
    }

    #[tokio::test]
    #[ignore = "requires a PostgreSQL server (AETHER_TEST_PG_DSN)"]
    async fn an_unreadable_table_is_an_error_not_an_empty_history() {
        // `query_range` swallowed its COUNT error into `total = 0`, which the
        // handler turned into `has_more: false` — paginating clients stopped early.
        let backend = backend("t_unreadable").await;
        sqlx::query("DROP TABLE history")
            .execute(&backend.pool)
            .await
            .expect("simulate an unreadable table");

        let (start_time, end_time) = window();
        let result = backend
            .query_range(&HistoryRangeQuery {
                series_key: "inst:1:M".to_string(),
                point_id: "7".to_string(),
                start_time,
                end_time,
                page: 1,
                page_size: 10,
            })
            .await;
        assert!(result.is_err(), "a failed read must propagate");

        assert!(
            backend.get_stats().await.is_err(),
            "a failed stats read must propagate, not report an empty historian"
        );
    }
}
