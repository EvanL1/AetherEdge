//! HTTP/OpenAPI DTOs for the history service.

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};

use crate::models::{self, HistoryRangeQuery, PatternEntry, ServiceConfig as RuntimeServiceConfig};

#[derive(Debug, Serialize, ToSchema)]
pub struct HistoryRecord {
    pub timestamp: String,
    pub series_key: String,
    pub point_id: String,
    pub value: Option<f64>,
    pub source: String,
}

impl From<models::HistoryRecord> for HistoryRecord {
    fn from(value: models::HistoryRecord) -> Self {
        Self {
            timestamp: value.timestamp,
            series_key: value.series_key,
            point_id: value.point_id,
            value: value.value,
            source: value.source,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, ToSchema)]
pub struct BatchSeriesItem {
    pub series_key: String,
    pub point_id: String,
}

#[derive(Debug, Deserialize, ToSchema)]
#[schema(example = json!({
    "start_time": "2026-05-11T02:14:34.712Z",
    "end_time": "2026-05-11T08:14:34.712Z",
    "series": [{"series_key": "inst:9:M", "point_id": "101"}],
    "limit_per_series": 500
}))]
pub struct BatchQueryRequest {
    pub start_time: String,
    pub end_time: String,
    pub series: Vec<BatchSeriesItem>,
    pub limit_per_series: Option<i64>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct SeriesPoint {
    pub time: String,
    pub value: Option<f64>,
}

impl From<models::SeriesPoint> for SeriesPoint {
    fn from(value: models::SeriesPoint) -> Self {
        Self {
            time: value.time,
            value: value.value,
        }
    }
}

#[derive(Debug, Serialize, ToSchema)]
pub struct SeriesResult {
    pub series_key: String,
    pub point_id: String,
    pub count: usize,
    pub data: Vec<SeriesPoint>,
}

impl From<models::SeriesResult> for SeriesResult {
    fn from(value: models::SeriesResult) -> Self {
        Self {
            series_key: value.series_key,
            point_id: value.point_id,
            count: value.count,
            data: value.data.into_iter().map(Into::into).collect(),
        }
    }
}

#[derive(Debug, Serialize, ToSchema)]
pub struct BatchQueryResponse {
    pub start_time: String,
    pub end_time: String,
    pub series: Vec<SeriesResult>,
}

#[derive(Debug, Deserialize, IntoParams)]
pub struct QueryRangeParams {
    pub series_key: String,
    pub point_id: String,
    pub start_time: Option<String>,
    pub end_time: Option<String>,
    pub page: Option<i64>,
    pub page_size: Option<i64>,
}

/// Subtract without panicking: `chrono` aborts the process (release profile sets
/// `panic = "abort"`) when the result leaves the representable range, and both
/// operands here are caller-controlled.
fn saturating_sub(time: DateTime<Utc>, delta: Duration) -> DateTime<Utc> {
    time.checked_sub_signed(delta)
        .unwrap_or(DateTime::<Utc>::MIN_UTC)
}

impl QueryRangeParams {
    pub fn into_query(self, config: &RuntimeServiceConfig) -> anyhow::Result<HistoryRangeQuery> {
        let end_time = self
            .end_time
            .as_deref()
            .map(models::parse_time)
            .transpose()?
            .unwrap_or_else(Utc::now);
        let requested_start = self
            .start_time
            .as_deref()
            .map(models::parse_time)
            .transpose()?
            .unwrap_or_else(|| saturating_sub(end_time, Duration::hours(24)));
        let start_time = requested_start.max(saturating_sub(
            end_time,
            Duration::days(config.max_time_range_days),
        ));

        // Clamp against the hard limit as well as the configured one: a config row
        // written before the limit existed may still hold an unbounded page size.
        let page_size_limit = config.max_page_size.clamp(1, models::MAX_PAGE_SIZE_LIMIT);

        Ok(HistoryRangeQuery {
            series_key: self.series_key,
            point_id: self.point_id,
            start_time,
            end_time,
            page: self.page.unwrap_or(1).clamp(1, models::MAX_PAGE_SIZE_LIMIT),
            page_size: self
                .page_size
                .unwrap_or(config.default_page_size)
                .clamp(1, page_size_limit),
        })
    }
}

#[derive(Debug, Deserialize, IntoParams)]
pub struct LatestParams {
    pub series_key: String,
    pub point_id: String,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct DataStats {
    pub earliest_timestamp: Option<String>,
    pub latest_timestamp: Option<String>,
    pub total_points: i64,
    pub channels: Vec<String>,
    pub data_types: Vec<String>,
}

impl From<models::DataStats> for DataStats {
    fn from(value: models::DataStats) -> Self {
        Self {
            earliest_timestamp: value.earliest_timestamp,
            latest_timestamp: value.latest_timestamp,
            total_points: value.total_points,
            channels: value.channels,
            data_types: value.data_types,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[schema(example = json!({
    "collection_interval_secs": 30,
    "flush_interval_secs": 60,
    "batch_size": 1000,
    "cleanup_enabled": true,
    "cleanup_older_than_days": 30,
    "default_page_size": 100,
    "max_page_size": 1000,
    "max_time_range_days": 365,
    "subscribe_patterns": {"inst:*:M": null, "inst:4:M": 60},
    "exclude_patterns": []
}))]
pub struct ServiceConfig {
    pub collection_interval_secs: u64,
    pub flush_interval_secs: u64,
    pub batch_size: usize,
    pub cleanup_enabled: bool,
    pub cleanup_older_than_days: i32,
    pub default_page_size: i64,
    pub max_page_size: i64,
    pub max_time_range_days: i64,
    #[serde(with = "crate::models::pattern_serde")]
    #[schema(value_type = Object)]
    pub subscribe_patterns: Vec<PatternEntry>,
    pub exclude_patterns: Vec<String>,
}

impl From<&RuntimeServiceConfig> for ServiceConfig {
    fn from(value: &RuntimeServiceConfig) -> Self {
        Self {
            collection_interval_secs: value.collection_interval_secs,
            flush_interval_secs: value.flush_interval_secs,
            batch_size: value.batch_size,
            cleanup_enabled: value.cleanup_enabled,
            cleanup_older_than_days: value.cleanup_older_than_days,
            default_page_size: value.default_page_size,
            max_page_size: value.max_page_size,
            max_time_range_days: value.max_time_range_days,
            subscribe_patterns: value.subscribe_patterns.clone(),
            exclude_patterns: value.exclude_patterns.clone(),
        }
    }
}

impl From<ServiceConfig> for RuntimeServiceConfig {
    fn from(value: ServiceConfig) -> Self {
        let mut config = Self {
            collection_interval_secs: value.collection_interval_secs,
            flush_interval_secs: value.flush_interval_secs,
            batch_size: value.batch_size,
            cleanup_enabled: value.cleanup_enabled,
            cleanup_older_than_days: value.cleanup_older_than_days,
            default_page_size: value.default_page_size,
            max_page_size: value.max_page_size,
            max_time_range_days: value.max_time_range_days,
            subscribe_patterns: value.subscribe_patterns,
            exclude_patterns: value.exclude_patterns,
        };
        config.normalize();
        config
    }
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct StorageTestRequest {
    pub backend: String,
    pub host: String,
    pub port: Option<u16>,
    pub username: String,
    pub password: String,
}

impl StorageTestRequest {
    pub fn addr(&self) -> String {
        format!("{}:{}", self.host, self.port.unwrap_or(5432))
    }

    #[cfg(feature = "postgres-storage")]
    pub fn pg_probe_dsn(&self) -> String {
        models::build_dsn(
            &self.host,
            self.port,
            "postgres",
            &self.username,
            &self.password,
        )
    }
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct StorageConfigRequest {
    pub enabled: bool,
    pub backend: String,
    pub host: String,
    pub port: Option<u16>,
    pub database: String,
    pub username: String,
    pub password: String,
}

impl StorageConfigRequest {
    pub fn into_settings(self) -> models::StorageSettings {
        let url = if self.backend == "sqlite" {
            self.database
        } else {
            models::build_dsn(
                &self.host,
                self.port,
                &self.database,
                &self.username,
                &self.password,
            )
        };
        models::StorageSettings {
            enabled: self.enabled,
            backend: self.backend,
            url,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn range_params(end_time: Option<&str>, page: Option<i64>) -> QueryRangeParams {
        QueryRangeParams {
            series_key: "inst:1:M".to_string(),
            point_id: "42".to_string(),
            start_time: None,
            end_time: end_time.map(str::to_string),
            page,
            page_size: None,
        }
    }

    #[test]
    fn end_time_near_the_representable_floor_does_not_panic() {
        // `parse_time` accepts any timestamp chrono can represent, including ones
        // that cannot absorb another `max_time_range_days` subtraction. Before the
        // fix this panicked, and `panic = "abort"` turned it into a process kill.
        let floor = DateTime::<Utc>::MIN_UTC.timestamp().to_string();

        let query = range_params(Some(&floor), None)
            .into_query(&RuntimeServiceConfig::default())
            .expect("an extreme but representable end_time is a valid request");

        assert!(query.start_time <= query.end_time);
    }

    #[test]
    fn default_start_time_near_the_representable_floor_does_not_panic() {
        // The `unwrap_or_else` default subtracts 24h from end_time on the same path.
        let floor = DateTime::<Utc>::MIN_UTC.timestamp().to_string();

        let query = range_params(Some(&floor), None)
            .into_query(&RuntimeServiceConfig::default())
            .expect("omitting start_time must not panic either");

        assert_eq!(query.start_time, DateTime::<Utc>::MIN_UTC);
    }

    #[test]
    fn page_is_clamped_so_offset_arithmetic_cannot_overflow() {
        let query = range_params(None, Some(i64::MAX))
            .into_query(&RuntimeServiceConfig::default())
            .expect("a huge page number is clamped, not an error");

        // This is exactly what the storage backends compute for OFFSET.
        assert!(
            query
                .page
                .checked_sub(1)
                .and_then(|p| p.checked_mul(query.page_size))
                .is_some()
        );
        // ...and what routes.rs computes for `has_more`.
        assert!(query.page.checked_mul(query.page_size).is_some());
    }

    #[test]
    fn batch_series_json_uses_domain_series_key() {
        let item = BatchSeriesItem {
            series_key: "inst:9:M".to_string(),
            point_id: "101".to_string(),
        };
        let value = serde_json::to_value(item).expect("serialize batch series item");
        assert_eq!(value["series_key"], "inst:9:M");
    }

    #[test]
    fn range_query_parses_time_and_normalizes_limits_once() {
        let config = RuntimeServiceConfig {
            default_page_size: 50,
            max_page_size: 200,
            max_time_range_days: 7,
            ..RuntimeServiceConfig::default()
        };
        let query = QueryRangeParams {
            series_key: "inst:9:M".to_string(),
            point_id: "101".to_string(),
            start_time: Some("2026-01-01T00:00:00Z".to_string()),
            end_time: Some("2026-02-01T00:00:00Z".to_string()),
            page: Some(0),
            page_size: Some(10_000),
        }
        .into_query(&config)
        .expect("valid range query");

        assert_eq!(query.page, 1);
        assert_eq!(query.page_size, 200);
        assert_eq!(query.start_time, query.end_time - chrono::Duration::days(7));
    }
}
