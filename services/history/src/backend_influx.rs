/// InfluxDB storage backend – **stub / reserved interface**.
///
/// This backend reserves the `StorageBackend` trait slot for a future InfluxDB
/// 3.x implementation.  All methods return an error until the backend is
/// implemented.
///
/// # How to implement
/// 1. Add `influxdb3-rust` (or `influxdb3-client`) to `Cargo.toml`.
/// 2. Replace the `todo!` bodies below with real InfluxDB Line Protocol writes
///    and Flight SQL queries.
/// 3. Wire the backend in `main.rs` when `STORAGE_BACKEND=influxdb`.
use async_trait::async_trait;

use chrono::{DateTime, Utc};

use crate::models::{DataPoint, DataStats, HistoryRecord, QueryRangeParams, SeriesResult};
use crate::storage::StorageBackend;

pub struct InfluxDbBackend;

#[async_trait]
impl StorageBackend for InfluxDbBackend {
    fn name(&self) -> &str {
        "influxdb"
    }

    async fn init_schema(&self) -> anyhow::Result<()> {
        anyhow::bail!("InfluxDB backend is not yet implemented")
    }

    async fn write_batch(&self, _points: Vec<DataPoint>) -> anyhow::Result<usize> {
        anyhow::bail!("InfluxDB backend is not yet implemented")
    }

    async fn query_range(
        &self,
        _params: &QueryRangeParams,
        _default_page_size: i64,
        _max_page_size: i64,
        _max_time_range_days: i64,
    ) -> anyhow::Result<(Vec<HistoryRecord>, i64)> {
        anyhow::bail!("InfluxDB backend is not yet implemented")
    }

    async fn query_latest(
        &self,
        _series_key: &str,
        _point_id: &str,
    ) -> anyhow::Result<Option<HistoryRecord>> {
        anyhow::bail!("InfluxDB backend is not yet implemented")
    }

    async fn get_stats(&self) -> anyhow::Result<DataStats> {
        anyhow::bail!("InfluxDB backend is not yet implemented")
    }

    async fn list_channels(&self) -> anyhow::Result<Vec<String>> {
        anyhow::bail!("InfluxDB backend is not yet implemented")
    }

    async fn query_batch(
        &self,
        _series: &[(String, String)],
        _start_time: DateTime<Utc>,
        _end_time: DateTime<Utc>,
        _limit_per_series: i64,
    ) -> anyhow::Result<Vec<SeriesResult>> {
        anyhow::bail!("InfluxDB backend is not yet implemented")
    }

    async fn cleanup_old_data(&self, _older_than_days: i32) -> anyhow::Result<u64> {
        anyhow::bail!("InfluxDB backend is not yet implemented")
    }

    async fn health_check(&self) -> bool {
        false
    }
}
