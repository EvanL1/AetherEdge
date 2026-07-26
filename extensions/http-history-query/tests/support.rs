//! Shared loopback fixtures for the HTTP `HistoryQuery` adapter tests.
//!
//! Cargo compiles this module into every integration target that declares it,
//! and each target uses only the fixtures it needs.
#![allow(dead_code)]

use aether_domain::{
    BindingIdentity, FeatureDefinition, FeatureRole, HistoryAggregation, HistoryDuplicatePolicy,
    TaskIdentity, TimestampMs,
};
use aether_http_history_query::{
    CalendarFeature, HistoryFeatureRoute, HttpHistoryQuery, HttpHistoryQueryConfig,
};
use aether_ports::HistoryWindow;
use serde_json::{Value, json};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// The only path `HttpHistoryQueryConfig` accepts.
pub const BATCH_PATH: &str = "/hisApi/data/batch-query";

/// 2026-07-11T11:00:00Z.
pub const WINDOW_START_MS: u64 = 1_783_767_600_000;
/// 2026-07-11T11:30:00Z, giving a 30-minute span.
pub const WINDOW_END_MS: u64 = 1_783_769_400_000;
/// Fifteen-minute cadence commissioned by the shared fixture routes.
pub const CADENCE_MS: u64 = 15 * 60 * 1_000;

pub fn binding() -> BindingIdentity {
    BindingIdentity::new("energy.site-a", 1).expect("fixture binding is valid")
}

pub fn task() -> TaskIdentity {
    TaskIdentity::new("iot.prealigned-history", 1).expect("fixture task is valid")
}

pub fn numeric(name: &str, unit: &str) -> FeatureDefinition {
    FeatureDefinition::numeric(name, FeatureRole::History, unit).expect("fixture feature is valid")
}

pub fn load_feature() -> FeatureDefinition {
    numeric("load", "kW")
}

pub fn quarter_hour_feature() -> FeatureDefinition {
    numeric("quarter_hour", "1")
}

pub fn stored_route() -> HistoryFeatureRoute {
    HistoryFeatureRoute::stored(
        task(),
        binding(),
        load_feature(),
        CADENCE_MS,
        "inst:1:M",
        "1",
        "energy.site.load.active_power",
    )
    .expect("fixture stored route is valid")
}

pub fn calendar_route() -> HistoryFeatureRoute {
    HistoryFeatureRoute::calendar(
        task(),
        binding(),
        quarter_hour_feature(),
        CADENCE_MS,
        CalendarFeature::QuarterHourOfDay,
        "calendar.quarter_hour",
    )
    .expect("fixture calendar route is valid")
}

/// Builds the aggregation and duplicate policy pair this adapter accepts.
pub fn window(features: Vec<FeatureDefinition>, max_samples: usize) -> HistoryWindow {
    window_with_policy(
        features,
        max_samples,
        HistoryAggregation::Last,
        HistoryDuplicatePolicy::Reject,
    )
}

pub fn window_with_policy(
    features: Vec<FeatureDefinition>,
    max_samples: usize,
    aggregation: HistoryAggregation,
    duplicate_policy: HistoryDuplicatePolicy,
) -> HistoryWindow {
    window_spanning(
        features,
        WINDOW_START_MS,
        WINDOW_END_MS,
        max_samples,
        aggregation,
        duplicate_policy,
    )
}

pub fn window_spanning(
    features: Vec<FeatureDefinition>,
    start_ms: u64,
    end_ms: u64,
    max_samples: usize,
    aggregation: HistoryAggregation,
    duplicate_policy: HistoryDuplicatePolicy,
) -> HistoryWindow {
    HistoryWindow::new(
        task(),
        binding(),
        features,
        TimestampMs::new(start_ms),
        TimestampMs::new(end_ms),
        max_samples,
        aggregation,
        duplicate_policy,
    )
    .expect("fixture window is valid")
}

/// Wraps a `data` object in the batch envelope the adapter expects.
pub fn envelope(data: Value) -> Value {
    json!({"success": true, "message": "OK", "data": data})
}

/// Builds a `data` object from `(series_key, point_id, points)` triples.
pub fn batch_data(series: Vec<Value>) -> Value {
    json!({
        "start_time": "2026-07-11T11:00:00Z",
        "end_time": "2026-07-11T11:30:00Z",
        "series": series,
    })
}

/// Builds one response series whose `count` matches its point list.
pub fn batch_series(series_key: &str, point_id: &str, points: Vec<Value>) -> Value {
    json!({
        "series_key": series_key,
        "point_id": point_id,
        "count": points.len(),
        "data": points,
    })
}

pub fn point(time: &str, value: Option<f64>) -> Value {
    json!({"time": time, "value": value})
}

/// Mounts an unbounded responder so a test may issue repeated queries.
pub async fn mount_json(server: &MockServer, status: u16, body: Value) {
    Mock::given(method("POST"))
        .and(path(BATCH_PATH))
        .respond_with(ResponseTemplate::new(status).set_body_json(body))
        .mount(server)
        .await;
}

pub fn config_for(server: &MockServer, routes: Vec<HistoryFeatureRoute>) -> HttpHistoryQueryConfig {
    config_with_limits(server, routes, 2_000, 64 * 1024)
}

pub fn config_with_limits(
    server: &MockServer,
    routes: Vec<HistoryFeatureRoute>,
    timeout_ms: u64,
    max_response_bytes: usize,
) -> HttpHistoryQueryConfig {
    HttpHistoryQueryConfig::new(
        &format!("{}{BATCH_PATH}", server.uri()),
        routes,
        timeout_ms,
        max_response_bytes,
    )
    .expect("fixture configuration is safe")
}

pub fn adapter_for(server: &MockServer, routes: Vec<HistoryFeatureRoute>) -> HttpHistoryQuery {
    HttpHistoryQuery::new(config_for(server, routes)).expect("fixture adapter builds")
}
