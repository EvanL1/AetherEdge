use async_trait::async_trait;
use chrono::{DateTime, Utc};

use crate::models::{DataPoint, DataStats, HistoryRecord, QueryRangeParams, SeriesResult};

/// Uniform interface for all historical-data storage backends.
///
/// Implement this trait to add a new backend (TimescaleDB, PostgreSQL,
/// InfluxDB, etc.) without touching the rest of the service.
#[async_trait]
pub trait StorageBackend: Send + Sync + 'static {
    /// Short identifier string, e.g. `"sqlite"` or `"postgres"`.
    fn name(&self) -> &str;

    /// Initialize the schema (create tables, hypertables, indexes).
    /// Called once at startup.
    async fn init_schema(&self) -> anyhow::Result<()>;

    /// Persist a batch of data points. Returns the number of rows written.
    async fn write_batch(&self, points: Vec<DataPoint>) -> anyhow::Result<usize>;

    /// Paginated range query. Returns `(records, total_count)`.
    async fn query_range(
        &self,
        params: &QueryRangeParams,
        default_page_size: i64,
        max_page_size: i64,
        max_time_range_days: i64,
    ) -> anyhow::Result<(Vec<HistoryRecord>, i64)>;

    /// Fetch the single most-recent record for a key/point pair.
    async fn query_latest(
        &self,
        series_key: &str,
        point_id: &str,
    ) -> anyhow::Result<Option<HistoryRecord>>;

    /// Global stats (row count, channel list, time range).
    async fn get_stats(&self) -> anyhow::Result<DataStats>;

    /// Return distinct logical series keys that have data in storage.
    async fn list_channels(&self) -> anyhow::Result<Vec<String>>;

    /// Batch range query: fetch multiple (`series_key`, `point_id`) series in one call.
    ///
    /// Returns one `SeriesResult` per requested series (in the same order).
    /// Each series contains at most `limit_per_series` data points ordered by
    /// time ascending.  Series with no data are included with an empty `data`
    /// vec so the caller always gets a result for every requested key/point.
    async fn query_batch(
        &self,
        series: &[(String, String)],
        start_time: DateTime<Utc>,
        end_time: DateTime<Utc>,
        limit_per_series: i64,
    ) -> anyhow::Result<Vec<SeriesResult>>;

    /// Delete rows older than `older_than_days`. Returns deleted row count.
    async fn cleanup_old_data(&self, older_than_days: i32) -> anyhow::Result<u64>;

    /// Lightweight connectivity check. Returns `true` if healthy.
    async fn health_check(&self) -> bool;
}
