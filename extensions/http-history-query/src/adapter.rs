use std::collections::BTreeMap;

use aether_domain::{
    FeatureValue, HistoryAggregation, HistoryDuplicatePolicy, SampleQuality, Segment, SegmentKind,
    Series, SourceKind, SourceProvenance, TimestampMs,
};
use aether_ports::{
    HistoryQuery, HistoryWindow, PortError, PortErrorKind, PortResult, SourcedSegment,
};
use async_trait::async_trait;
use chrono::{DateTime, Timelike, Utc};
use serde::{Deserialize, Serialize};

use crate::{CalendarFeature, HistoryFeatureRoute, HistoryFeatureSource, HttpHistoryQueryConfig};

const MAX_UPSTREAM_SAMPLES: usize = 5_000;

/// Bounded adapter for the existing aether-history batch query endpoint.
pub struct HttpHistoryQuery {
    config: HttpHistoryQueryConfig,
    client: reqwest::Client,
}

impl HttpHistoryQuery {
    /// Creates an adapter from already validated transport and feature policy.
    pub fn new(config: HttpHistoryQueryConfig) -> PortResult<Self> {
        let client = config.build_client()?;
        Ok(Self { config, client })
    }

    fn routes<'a>(&'a self, window: &HistoryWindow) -> PortResult<Vec<&'a HistoryFeatureRoute>> {
        window
            .features()
            .iter()
            .map(|feature| {
                let route = self
                    .config
                    .routes()
                    .iter()
                    .find(|route| {
                        route.task() == window.task()
                            && route.binding() == window.binding()
                            && route.definition().name() == feature.name()
                    })
                    .ok_or_else(|| permanent("history feature mapping is incomplete"))?;
                if route.definition() != feature {
                    return Err(permanent(
                        "history feature type, role, unit, or constraints mismatch its mapping",
                    ));
                }
                Ok(route)
            })
            .collect()
    }

    async fn fetch(&self, request: &BatchQueryRequest) -> PortResult<BatchQueryResponse> {
        let mut response = self
            .client
            .post(self.config.endpoint().clone())
            .json(request)
            .send()
            .await
            .map_err(map_transport)?;
        if response.status() != reqwest::StatusCode::OK {
            return Err(PortError::new(
                if response.status().is_server_error() {
                    PortErrorKind::Unavailable
                } else {
                    PortErrorKind::Rejected
                },
                "history service rejected the bounded query",
            ));
        }
        if response
            .content_length()
            .is_some_and(|length| length > self.config.max_response_bytes() as u64)
        {
            return Err(PortError::new(
                PortErrorKind::InvalidData,
                "history response exceeds the configured limit",
            ));
        }
        let mut body = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(map_transport)? {
            if body
                .len()
                .checked_add(chunk.len())
                .is_none_or(|size| size > self.config.max_response_bytes())
            {
                return Err(PortError::new(
                    PortErrorKind::InvalidData,
                    "history response exceeds the configured limit",
                ));
            }
            body.extend_from_slice(&chunk);
        }
        let envelope: BatchEnvelope = serde_json::from_slice(&body)
            .map_err(|_| invalid("history response is not the expected JSON contract"))?;
        if !envelope.success || envelope.message.trim().is_empty() {
            return Err(invalid("history response reports failure"));
        }
        Ok(envelope.data)
    }
}

struct RegularGrid {
    timestamps: Vec<TimestampMs>,
}

impl RegularGrid {
    fn new(window: &HistoryWindow, cadence_ms: u64) -> PortResult<Self> {
        let span = window
            .end()
            .get()
            .checked_sub(window.start().get())
            .ok_or_else(|| invalid("history window span is invalid"))?;
        if cadence_ms == 0 || span == 0 || !span.is_multiple_of(cadence_ms) {
            return Err(invalid(
                "history window does not match its commissioned cadence",
            ));
        }
        let sample_count = usize::try_from(span / cadence_ms)
            .map_err(|_| rejected("history logical sample count exceeds usize"))?;
        if sample_count == 0 || sample_count > window.max_samples() {
            return Err(rejected(
                "history logical sample count exceeds the requested bound",
            ));
        }
        if sample_count > MAX_UPSTREAM_SAMPLES {
            return Err(permanent(
                "history window exceeds the aether-history per-series limit",
            ));
        }
        let timestamps = (0..sample_count)
            .map(|index| {
                u64::try_from(index)
                    .ok()
                    .and_then(|index| cadence_ms.checked_mul(index))
                    .and_then(|offset| window.start().get().checked_add(offset))
                    .map(TimestampMs::new)
                    .ok_or_else(|| invalid("history timestamp grid overflowed"))
            })
            .collect::<PortResult<Vec<_>>>()?;
        Ok(Self { timestamps })
    }
}

#[async_trait]
impl HistoryQuery for HttpHistoryQuery {
    async fn query(&self, window: HistoryWindow) -> PortResult<SourcedSegment> {
        if window.policies().iter().any(|policy| {
            policy.aggregation() != HistoryAggregation::Last
                || policy.duplicate_policy() != HistoryDuplicatePolicy::Reject
        }) {
            return Err(permanent(
                "HTTP history requires a pre-aligned last-value series with duplicate rejection",
            ));
        }
        let routes = self.routes(&window)?;
        let cadence_ms = routes
            .first()
            .map(|route| route.cadence_ms())
            .ok_or_else(|| permanent("HTTP history query has no routes"))?;
        if routes.iter().any(|route| route.cadence_ms() != cadence_ms) {
            return Err(permanent(
                "one HTTP history window cannot mix commissioned cadences",
            ));
        }
        let timestamps = RegularGrid::new(&window, cadence_ms)?.timestamps;
        let sample_count = timestamps.len();

        let stored = routes
            .iter()
            .filter_map(|route| match route.source() {
                HistoryFeatureSource::Stored {
                    series_key,
                    point_id,
                    ..
                } => Some(BatchSeriesRequest {
                    series_key: series_key.clone(),
                    point_id: point_id.clone(),
                }),
                HistoryFeatureSource::Calendar { .. } => None,
            })
            .collect::<Vec<_>>();
        let response = if stored.is_empty() {
            BatchQueryResponse {
                _start_time: String::new(),
                _end_time: String::new(),
                series: Vec::new(),
            }
        } else {
            self.fetch(&BatchQueryRequest {
                start_time: format_timestamp(window.start())?,
                end_time: format_timestamp(window.cutoff())?,
                series: stored,
                limit_per_series: sample_count,
            })
            .await?
        };
        if response.series.len()
            != routes
                .iter()
                .filter(|route| matches!(route.source(), HistoryFeatureSource::Stored { .. }))
                .count()
        {
            return Err(invalid(
                "history response omitted or added a requested series",
            ));
        }

        let mut series = Vec::with_capacity(routes.len());
        let mut provenance = Vec::with_capacity(routes.len());
        for (definition, route) in window.features().iter().zip(routes) {
            match route.source() {
                HistoryFeatureSource::Stored {
                    series_key,
                    point_id,
                    source_ref,
                } => {
                    let returned = response
                        .series
                        .iter()
                        .find(|item| item.series_key == *series_key && item.point_id == *point_id)
                        .ok_or_else(|| invalid("history response series identity mismatched"))?;
                    if returned.count != returned.data.len() || returned.data.len() > sample_count {
                        return Err(invalid("history response count exceeds the request"));
                    }
                    let mut values_by_time = BTreeMap::new();
                    for point in &returned.data {
                        let timestamp = parse_timestamp(&point.time)?;
                        if timestamp < window.start()
                            || timestamp >= window.end()
                            || timestamp > window.cutoff()
                            || values_by_time.insert(timestamp, point.value).is_some()
                        {
                            return Err(invalid(
                                "history response contains duplicate or out-of-window points",
                            ));
                        }
                    }
                    if values_by_time
                        .keys()
                        .any(|timestamp| !timestamps.contains(timestamp))
                    {
                        return Err(invalid("history response is not aligned to the task grid"));
                    }
                    let newest_actual = values_by_time
                        .iter()
                        .rev()
                        .find_map(|(timestamp, value)| value.map(|_| *timestamp))
                        .ok_or_else(|| invalid("history series has no usable observation"))?;
                    let (values, quality): (Vec<_>, Vec<_>) = timestamps
                        .iter()
                        .map(
                            |timestamp| match values_by_time.get(timestamp).copied().flatten() {
                                Some(value) if value.is_finite() => (
                                    FeatureValue::number(value)
                                        .map_err(|_| invalid("history value is not finite")),
                                    SampleQuality::Good,
                                ),
                                Some(_) => (
                                    Err(invalid("history value is not finite")),
                                    SampleQuality::Missing,
                                ),
                                None => (Ok(FeatureValue::missing()), SampleQuality::Missing),
                            },
                        )
                        .map(|(value, quality)| value.map(|value| (value, quality)))
                        .collect::<PortResult<Vec<_>>>()?
                        .into_iter()
                        .unzip();
                    series.push(
                        Series::new(definition.clone(), values, quality)
                            .map_err(|_| invalid("history series violates its feature type"))?,
                    );
                    provenance.push(
                        SourceProvenance::new(
                            SegmentKind::History,
                            definition.name(),
                            SourceKind::History,
                            Some(source_ref),
                            newest_actual,
                        )
                        .map_err(|_| invalid("history provenance is invalid"))?,
                    );
                },
                HistoryFeatureSource::Calendar {
                    transform,
                    source_ref,
                } => {
                    let values = timestamps
                        .iter()
                        .map(|timestamp| calendar_value(*transform, *timestamp))
                        .map(FeatureValue::number)
                        .collect::<Result<Vec<_>, _>>()
                        .map_err(|_| invalid("calendar value is invalid"))?;
                    series.push(
                        Series::new(
                            definition.clone(),
                            values,
                            vec![SampleQuality::Good; sample_count],
                        )
                        .map_err(|_| invalid("calendar series violates its feature type"))?,
                    );
                    provenance.push(
                        SourceProvenance::new(
                            SegmentKind::History,
                            definition.name(),
                            SourceKind::Calendar,
                            Some(source_ref),
                            *timestamps
                                .last()
                                .ok_or_else(|| invalid("calendar grid is empty"))?,
                        )
                        .map_err(|_| invalid("calendar provenance is invalid"))?,
                    );
                },
            }
        }
        let segment = Segment::new(timestamps, series)
            .map_err(|_| invalid("history segment is structurally invalid"))?;
        SourcedSegment::new(segment, provenance)
    }
}

fn calendar_value(transform: CalendarFeature, timestamp: TimestampMs) -> f64 {
    let Some(milliseconds) = i64::try_from(timestamp.get()).ok() else {
        return f64::NAN;
    };
    let Some(value) = DateTime::<Utc>::from_timestamp_millis(milliseconds) else {
        return f64::NAN;
    };
    match transform {
        CalendarFeature::QuarterHourOfDay => f64::from(value.hour() * 4 + value.minute() / 15),
    }
}

fn format_timestamp(timestamp: TimestampMs) -> PortResult<String> {
    let milliseconds = i64::try_from(timestamp.get())
        .map_err(|_| invalid("history timestamp is outside RFC 3339 range"))?;
    DateTime::<Utc>::from_timestamp_millis(milliseconds)
        .map(|value| value.to_rfc3339_opts(chrono::SecondsFormat::AutoSi, true))
        .ok_or_else(|| invalid("history timestamp is outside RFC 3339 range"))
}

fn parse_timestamp(value: &str) -> PortResult<TimestampMs> {
    let parsed = DateTime::parse_from_rfc3339(value)
        .map_err(|_| invalid("history timestamp is not RFC 3339"))?;
    let milliseconds = u64::try_from(parsed.timestamp_millis())
        .map_err(|_| invalid("history timestamp is outside supported range"))?;
    Ok(TimestampMs::new(milliseconds))
}

fn map_transport(error: reqwest::Error) -> PortError {
    PortError::new(
        if error.is_timeout() {
            PortErrorKind::Timeout
        } else {
            PortErrorKind::Unavailable
        },
        "history transport failed",
    )
}

fn invalid(message: &'static str) -> PortError {
    PortError::new(PortErrorKind::InvalidData, message)
}

fn permanent(message: &'static str) -> PortError {
    PortError::new(PortErrorKind::Permanent, message)
}

fn rejected(message: &'static str) -> PortError {
    PortError::new(PortErrorKind::Rejected, message)
}

#[derive(Debug, Serialize)]
struct BatchQueryRequest {
    start_time: String,
    end_time: String,
    series: Vec<BatchSeriesRequest>,
    limit_per_series: usize,
}

#[derive(Debug, Serialize)]
struct BatchSeriesRequest {
    series_key: String,
    point_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct BatchEnvelope {
    success: bool,
    message: String,
    data: BatchQueryResponse,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct BatchQueryResponse {
    #[serde(rename = "start_time")]
    _start_time: String,
    #[serde(rename = "end_time")]
    _end_time: String,
    #[serde(default)]
    series: Vec<BatchSeriesResponse>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct BatchSeriesResponse {
    series_key: String,
    point_id: String,
    count: usize,
    data: Vec<BatchPoint>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct BatchPoint {
    time: String,
    value: Option<f64>,
}
