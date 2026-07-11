/// NullBackend – placeholder used when no storage backend is configured.
///
/// All write/query operations return an explanatory error rather than
/// silently discarding data.  Background tasks check `storage_enabled`
/// before touching the storage backend, so in normal operation this
/// backend is never called.
use async_trait::async_trait;

use chrono::{DateTime, Utc};

use crate::models::{DataPoint, DataStats, HistoryRecord, QueryRangeParams, SeriesResult};
use crate::storage::StorageBackend;

pub struct NullBackend;

#[async_trait]
impl StorageBackend for NullBackend {
    fn name(&self) -> &str {
        "disabled"
    }

    async fn init_schema(&self) -> anyhow::Result<()> {
        Ok(())
    }

    async fn write_batch(&self, _points: Vec<DataPoint>) -> anyhow::Result<usize> {
        anyhow::bail!(
            "Storage backend not configured. Use PUT /hisApi/storage to configure and enable storage first"
        )
    }

    async fn query_range(
        &self,
        _params: &QueryRangeParams,
        _default_page_size: i64,
        _max_page_size: i64,
        _max_time_range_days: i64,
    ) -> anyhow::Result<(Vec<HistoryRecord>, i64)> {
        anyhow::bail!("Storage backend not configured")
    }

    async fn query_latest(
        &self,
        _series_key: &str,
        _point_id: &str,
    ) -> anyhow::Result<Option<HistoryRecord>> {
        anyhow::bail!("Storage backend not configured")
    }

    async fn get_stats(&self) -> anyhow::Result<DataStats> {
        anyhow::bail!("Storage backend not configured")
    }

    async fn list_channels(&self) -> anyhow::Result<Vec<String>> {
        anyhow::bail!("Storage backend not configured")
    }

    async fn query_batch(
        &self,
        _series: &[(String, String)],
        _start_time: DateTime<Utc>,
        _end_time: DateTime<Utc>,
        _limit_per_series: i64,
    ) -> anyhow::Result<Vec<SeriesResult>> {
        anyhow::bail!("存储后端未配置")
    }

    async fn cleanup_old_data(&self, _older_than_days: i32) -> anyhow::Result<u64> {
        anyhow::bail!("Storage backend not configured")
    }

    async fn health_check(&self) -> bool {
        false
    }
}
