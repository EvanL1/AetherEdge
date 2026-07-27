use std::time::Duration;

use aether_domain::{
    FeatureValue, HistoryAggregation, HistoryDuplicatePolicy, SampleQuality, Segment, SegmentKind,
    Series, SourceKind, SourceProvenance, TimestampMs,
};
use aether_ports::{
    HistoryQuery, HistoryWindow, PortError, PortErrorKind, PortResult, SourcedSegment,
};
use async_trait::async_trait;
use chrono::{DateTime, Timelike, Utc};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::{Sqlite, SqlitePool, Transaction};

use crate::{
    CalendarFeature, SqliteHistoryFeatureRoute, SqliteHistoryFeatureSource,
    SqliteHistoryQueryConfig,
};

const READ_BUSY_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_READ_CONNECTIONS: u32 = 4;
const REQUIRED_HISTORY_COLUMNS: [&str; 4] = ["time_ms", "series_key", "point_id", "value"];

/// Read-only embedded adapter over the existing `aether-history` SQLite file.
pub struct SqliteHistoryQuery {
    config: SqliteHistoryQueryConfig,
    pool: SqlitePool,
}

impl SqliteHistoryQuery {
    /// Configures a lazy SQLite database in read-only/query-only mode.
    ///
    /// The database may be absent while the composition root starts. Each
    /// query later returns [`PortErrorKind::Unavailable`] while the file is
    /// inaccessible. This function never creates a database or schema object.
    pub async fn open(config: SqliteHistoryQueryConfig) -> PortResult<Self> {
        let options = SqliteConnectOptions::new()
            .filename(config.database_path())
            .create_if_missing(false)
            .read_only(true)
            .busy_timeout(READ_BUSY_TIMEOUT)
            .pragma("query_only", "ON");
        let pool = SqlitePoolOptions::new()
            .max_connections(MAX_READ_CONNECTIONS)
            .acquire_timeout(READ_BUSY_TIMEOUT)
            .connect_lazy_with(options);
        Ok(Self { config, pool })
    }

    async fn validate_schema(transaction: &mut Transaction<'_, Sqlite>) -> PortResult<()> {
        let columns: Vec<String> =
            sqlx::query_scalar("SELECT name FROM pragma_table_info('history') ORDER BY cid ASC")
                .fetch_all(&mut **transaction)
                .await
                .map_err(|_| unavailable("SQLite history schema cannot be inspected"))?;
        if REQUIRED_HISTORY_COLUMNS
            .iter()
            .any(|required| !columns.iter().any(|column| column == required))
        {
            return Err(unavailable(
                "SQLite history table is not ready with the required read contract",
            ));
        }
        Ok(())
    }

    fn routes<'a>(
        &'a self,
        window: &HistoryWindow,
    ) -> PortResult<Vec<&'a SqliteHistoryFeatureRoute>> {
        window
            .features()
            .iter()
            .map(|requested| {
                let route = self
                    .config
                    .routes()
                    .iter()
                    .find(|route| {
                        route.task() == window.task()
                            && route.binding() == window.binding()
                            && route.definition().name() == requested.name()
                    })
                    .ok_or_else(|| permanent("SQLite history feature mapping is incomplete"))?;
                if route.definition() != requested {
                    return Err(permanent(
                        "SQLite history feature type, role, or unit mismatches its mapping",
                    ));
                }
                let requested_policy = window
                    .policy(requested.name())
                    .ok_or_else(|| permanent("SQLite history feature policy is incomplete"))?;
                if route
                    .aggregation()
                    .is_some_and(|aggregation| aggregation != requested_policy.aggregation())
                {
                    return Err(permanent(
                        "SQLite history aggregation mismatches its commissioned route",
                    ));
                }
                if route
                    .duplicate_policy()
                    .is_some_and(|policy| policy != requested_policy.duplicate_policy())
                {
                    return Err(permanent(
                        "SQLite history duplicate policy mismatches its commissioned route",
                    ));
                }
                Ok(route)
            })
            .collect()
    }

    async fn read_stored_series(
        &self,
        route: &SqliteHistoryFeatureRoute,
        grid: &IntervalEndGrid,
        transaction: &mut Transaction<'_, Sqlite>,
    ) -> PortResult<(Series, SourceProvenance)> {
        let SqliteHistoryFeatureSource::Stored {
            series_key,
            point_id,
            source_ref,
            aggregation,
            duplicate_policy,
        } = route.source()
        else {
            return Err(permanent("SQLite history route is not a stored series"));
        };
        let raw_limit = self
            .config
            .max_raw_samples_per_feature()
            .checked_add(1)
            .and_then(|limit| i64::try_from(limit).ok())
            .ok_or_else(|| permanent("SQLite raw sample limit is invalid"))?;
        let raw_lower = i64::try_from(grid.raw_lower_exclusive.get())
            .map_err(|_| invalid("SQLite raw lower bound is outside i64"))?;
        let raw_upper = i64::try_from(grid.raw_upper_inclusive.get())
            .map_err(|_| invalid("SQLite raw upper bound is outside i64"))?;
        let rows: Vec<(i64, i64, Option<f64>)> = sqlx::query_as(
            "SELECT rowid, time_ms, value FROM history \
             WHERE series_key = ? AND point_id = ? AND time_ms > ? AND time_ms <= ? \
             ORDER BY time_ms ASC, rowid ASC LIMIT ?",
        )
        .bind(series_key)
        .bind(point_id)
        .bind(raw_lower)
        .bind(raw_upper)
        .bind(raw_limit)
        .fetch_all(&mut **transaction)
        .await
        .map_err(|_| unavailable("SQLite bounded history read failed"))?;
        if rows.len() > self.config.max_raw_samples_per_feature() {
            return Err(rejected(
                "SQLite history exceeds the per-feature raw sample bound",
            ));
        }
        let rows = resolve_duplicate_rows(rows, *duplicate_policy)?;

        let mut buckets = vec![AggregateBucket::default(); grid.timestamps.len()];
        let mut watermark = None;
        for (time_ms, value) in rows {
            let timestamp = u64::try_from(time_ms)
                .map(TimestampMs::new)
                .map_err(|_| invalid("SQLite history contains a negative timestamp"))?;
            if timestamp <= grid.raw_lower_exclusive
                || timestamp > grid.raw_upper_inclusive
                || timestamp > grid.cutoff
            {
                return Err(invalid("SQLite history escaped the bounded raw window"));
            }
            let Some(value) = value else {
                continue;
            };
            if !value.is_finite() {
                return Err(invalid("SQLite history contains a non-finite value"));
            }
            if route
                .definition()
                .numeric_constraints()
                .is_some_and(|constraints| !constraints.accepts(value))
            {
                return Err(invalid(
                    "SQLite raw history violates task-owned numeric limits",
                ));
            }
            let offset = timestamp
                .get()
                .checked_sub(grid.raw_lower_exclusive.get())
                .and_then(|value| value.checked_sub(1))
                .ok_or_else(|| invalid("SQLite history bucket offset is invalid"))?;
            let bucket_index = usize::try_from(offset / grid.cadence_ms)
                .map_err(|_| invalid("SQLite history bucket index exceeds usize"))?;
            let bucket = buckets
                .get_mut(bucket_index)
                .ok_or_else(|| invalid("SQLite history row has no interval-end bucket"))?;
            bucket.add(*aggregation, timestamp, value)?;
            watermark =
                Some(watermark.map_or(timestamp, |current: TimestampMs| current.max(timestamp)));
        }
        let watermark = watermark
            .ok_or_else(|| invalid("SQLite history series has no usable raw observation"))?;
        let (values, quality) = buckets
            .into_iter()
            .map(|bucket| bucket.finish(*aggregation))
            .collect::<PortResult<Vec<_>>>()?
            .into_iter()
            .unzip();
        let series = Series::new(route.definition().clone(), values, quality)
            .map_err(|_| invalid("SQLite aggregate violates its feature definition"))?;
        let provenance = SourceProvenance::new(
            SegmentKind::History,
            route.definition().name(),
            SourceKind::History,
            Some(source_ref),
            watermark,
        )
        .map_err(|_| invalid("SQLite history provenance is invalid"))?;
        Ok((series, provenance))
    }

    fn calendar_series(
        route: &SqliteHistoryFeatureRoute,
        grid: &IntervalEndGrid,
    ) -> PortResult<(Series, SourceProvenance)> {
        let SqliteHistoryFeatureSource::Calendar {
            transform,
            source_ref,
        } = route.source()
        else {
            return Err(permanent("SQLite history route is not a calendar feature"));
        };
        let values = grid
            .timestamps
            .iter()
            .copied()
            .map(|timestamp| {
                let value = calendar_value(*transform, timestamp)?;
                FeatureValue::number(value).map_err(|_| invalid("calendar value is invalid"))
            })
            .collect::<PortResult<Vec<_>>>()?;
        let series = Series::new(
            route.definition().clone(),
            values,
            vec![SampleQuality::Good; grid.timestamps.len()],
        )
        .map_err(|_| invalid("calendar aggregate violates its feature definition"))?;
        let watermark = grid
            .timestamps
            .last()
            .copied()
            .ok_or_else(|| invalid("calendar grid is empty"))?;
        let provenance = SourceProvenance::new(
            SegmentKind::History,
            route.definition().name(),
            SourceKind::Calendar,
            Some(source_ref),
            watermark,
        )
        .map_err(|_| invalid("calendar provenance is invalid"))?;
        Ok((series, provenance))
    }
}

#[async_trait]
impl HistoryQuery for SqliteHistoryQuery {
    async fn query(&self, window: HistoryWindow) -> PortResult<SourcedSegment> {
        let routes = self.routes(&window)?;
        let cadence_ms = routes
            .first()
            .map(|route| route.cadence_ms())
            .ok_or_else(|| permanent("SQLite history query has no routes"))?;
        if routes.iter().any(|route| route.cadence_ms() != cadence_ms) {
            return Err(permanent(
                "one SQLite history window cannot mix commissioned cadences",
            ));
        }
        let grid = IntervalEndGrid::new(&window, cadence_ms)?;
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|_| unavailable("SQLite history database cannot be opened read-only"))?;
        Self::validate_schema(&mut transaction).await?;
        let mut series = Vec::with_capacity(routes.len());
        let mut provenance = Vec::with_capacity(routes.len());
        for route in routes {
            let (resolved, source) = match route.source() {
                SqliteHistoryFeatureSource::Stored { .. } => {
                    self.read_stored_series(route, &grid, &mut transaction)
                        .await?
                },
                SqliteHistoryFeatureSource::Calendar { .. } => Self::calendar_series(route, &grid)?,
            };
            series.push(resolved);
            provenance.push(source);
        }
        transaction
            .commit()
            .await
            .map_err(|_| unavailable("SQLite history read transaction could not commit"))?;
        let segment = Segment::new(grid.timestamps, series)
            .map_err(|_| invalid("SQLite history segment is structurally invalid"))?;
        SourcedSegment::new(segment, provenance)
    }
}

struct IntervalEndGrid {
    cadence_ms: u64,
    timestamps: Vec<TimestampMs>,
    raw_lower_exclusive: TimestampMs,
    raw_upper_inclusive: TimestampMs,
    cutoff: TimestampMs,
}

impl IntervalEndGrid {
    fn new(window: &HistoryWindow, cadence_ms: u64) -> PortResult<Self> {
        let span = window
            .end()
            .get()
            .checked_sub(window.start().get())
            .ok_or_else(|| invalid("SQLite history window span is invalid"))?;
        if cadence_ms == 0 || span == 0 || !span.is_multiple_of(cadence_ms) {
            return Err(invalid(
                "SQLite history window does not match its commissioned cadence",
            ));
        }
        let sample_count = usize::try_from(span / cadence_ms)
            .map_err(|_| rejected("SQLite logical sample count exceeds usize"))?;
        if sample_count == 0 || sample_count > window.max_samples() {
            return Err(rejected(
                "SQLite logical history exceeds the requested sample bound",
            ));
        }
        let timestamps = (0..sample_count)
            .map(|index| {
                u64::try_from(index)
                    .ok()
                    .and_then(|index| cadence_ms.checked_mul(index))
                    .and_then(|offset| window.start().get().checked_add(offset))
                    .map(TimestampMs::new)
                    .ok_or_else(|| invalid("SQLite interval-end grid overflowed"))
            })
            .collect::<PortResult<Vec<_>>>()?;
        let raw_upper_inclusive = timestamps
            .last()
            .copied()
            .ok_or_else(|| invalid("SQLite interval-end grid is empty"))?;
        let cutoff = window.cutoff();
        if raw_upper_inclusive > cutoff {
            return Err(invalid(
                "SQLite interval-end grid advances beyond the observation cutoff",
            ));
        }
        let raw_lower_exclusive = window
            .start()
            .get()
            .checked_sub(cadence_ms)
            .map(TimestampMs::new)
            .ok_or_else(|| invalid("SQLite first interval has no bounded lower edge"))?;
        Ok(Self {
            cadence_ms,
            timestamps,
            raw_lower_exclusive,
            raw_upper_inclusive,
            cutoff,
        })
    }
}

fn resolve_duplicate_rows(
    rows: Vec<(i64, i64, Option<f64>)>,
    policy: HistoryDuplicatePolicy,
) -> PortResult<Vec<(i64, Option<f64>)>> {
    let mut resolved: Vec<(i64, i64, Option<f64>)> = Vec::with_capacity(rows.len());
    for (row_id, time_ms, value) in rows {
        if let Some((seen_row_id, seen_time_ms, seen_value)) = resolved.last_mut() {
            if time_ms < *seen_time_ms || (time_ms == *seen_time_ms && row_id <= *seen_row_id) {
                return Err(invalid(
                    "SQLite raw history is not ordered by timestamp and rowid",
                ));
            }
            if time_ms == *seen_time_ms {
                match policy {
                    HistoryDuplicatePolicy::Latest => {
                        *seen_row_id = row_id;
                        *seen_time_ms = time_ms;
                        *seen_value = value;
                        continue;
                    },
                    HistoryDuplicatePolicy::Reject => {
                        return Err(invalid("SQLite raw history contains a duplicate timestamp"));
                    },
                }
            }
        }
        resolved.push((row_id, time_ms, value));
    }
    Ok(resolved
        .into_iter()
        .map(|(_, time_ms, value)| (time_ms, value))
        .collect())
}

#[derive(Debug, Clone, Copy, Default)]
struct AggregateBucket {
    sum: f64,
    count: u64,
    minimum: Option<f64>,
    maximum: Option<f64>,
    last: Option<(TimestampMs, f64)>,
}

impl AggregateBucket {
    fn add(
        &mut self,
        aggregation: HistoryAggregation,
        timestamp: TimestampMs,
        value: f64,
    ) -> PortResult<()> {
        self.count = self
            .count
            .checked_add(1)
            .ok_or_else(|| invalid("SQLite aggregate count overflowed"))?;
        match aggregation {
            HistoryAggregation::Mean | HistoryAggregation::Sum => {
                self.sum += value;
                if !self.sum.is_finite() {
                    return Err(invalid("SQLite aggregate sum overflowed"));
                }
            },
            HistoryAggregation::Min => {
                self.minimum = Some(self.minimum.map_or(value, |current| current.min(value)));
            },
            HistoryAggregation::Max => {
                self.maximum = Some(self.maximum.map_or(value, |current| current.max(value)));
            },
            HistoryAggregation::Last => {
                if self
                    .last
                    .is_some_and(|(seen_timestamp, _)| seen_timestamp >= timestamp)
                {
                    return Err(invalid(
                        "SQLite last aggregation has duplicate or unordered timestamps",
                    ));
                }
                self.last = Some((timestamp, value));
            },
        }
        Ok(())
    }

    fn finish(self, aggregation: HistoryAggregation) -> PortResult<(FeatureValue, SampleQuality)> {
        if self.count == 0 {
            return Ok((FeatureValue::missing(), SampleQuality::Missing));
        }
        let value = match aggregation {
            HistoryAggregation::Mean => self.sum / self.count as f64,
            HistoryAggregation::Last => self
                .last
                .map(|(_, value)| value)
                .ok_or_else(|| invalid("SQLite last aggregate is empty"))?,
            HistoryAggregation::Sum => self.sum,
            HistoryAggregation::Min => self
                .minimum
                .ok_or_else(|| invalid("SQLite minimum aggregate is empty"))?,
            HistoryAggregation::Max => self
                .maximum
                .ok_or_else(|| invalid("SQLite maximum aggregate is empty"))?,
        };
        FeatureValue::number(value)
            .map(|value| (value, SampleQuality::Good))
            .map_err(|_| invalid("SQLite aggregate value is invalid"))
    }
}

fn calendar_value(transform: CalendarFeature, timestamp: TimestampMs) -> PortResult<f64> {
    let milliseconds = i64::try_from(timestamp.get())
        .map_err(|_| invalid("calendar timestamp is outside UTC range"))?;
    let value = DateTime::<Utc>::from_timestamp_millis(milliseconds)
        .ok_or_else(|| invalid("calendar timestamp is outside UTC range"))?;
    Ok(match transform {
        CalendarFeature::QuarterHourOfDay => f64::from(value.hour() * 4 + value.minute() / 15),
    })
}

fn unavailable(message: &'static str) -> PortError {
    PortError::new(PortErrorKind::Unavailable, message)
}

fn rejected(message: &'static str) -> PortError {
    PortError::new(PortErrorKind::Rejected, message)
}

fn invalid(message: &'static str) -> PortError {
    PortError::new(PortErrorKind::InvalidData, message)
}

fn permanent(message: &'static str) -> PortError {
    PortError::new(PortErrorKind::Permanent, message)
}
