use aether_domain::{
    BindingIdentity, FeatureDefinition, FeatureRole, HistoryAggregation, HistoryDuplicatePolicy,
    SampleQuality, SourceKind, TaskIdentity, TimestampMs,
};
use aether_http_history_query::{
    CalendarFeature, HistoryFeatureRoute, HttpHistoryQuery, HttpHistoryQueryConfig,
};
use aether_ports::{HistoryQuery, HistoryWindow, PortErrorKind};
use serde_json::json;
use wiremock::matchers::{body_json, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const CADENCE_MS: u64 = 15 * 60 * 1_000;

fn binding() -> BindingIdentity {
    BindingIdentity::new("energy.site-a", 1).expect("binding is valid")
}

fn task() -> TaskIdentity {
    TaskIdentity::new("iot.prealigned-history", 1).expect("task is valid")
}

fn numeric(name: &str, unit: &str) -> FeatureDefinition {
    FeatureDefinition::numeric(name, FeatureRole::History, unit).expect("feature is valid")
}

fn routes() -> Vec<HistoryFeatureRoute> {
    vec![
        HistoryFeatureRoute::stored(
            task(),
            binding(),
            numeric("load", "kW"),
            CADENCE_MS,
            "inst:1:M",
            "1",
            "energy.site.load.active_power",
        )
        .expect("route is valid"),
        HistoryFeatureRoute::stored(
            task(),
            binding(),
            numeric("temp_avg", "Cel"),
            CADENCE_MS,
            "inst:2:M",
            "2",
            "weather.observed.air_temperature",
        )
        .expect("route is valid"),
        HistoryFeatureRoute::calendar(
            task(),
            binding(),
            numeric("quarter_hour", "1"),
            CADENCE_MS,
            CalendarFeature::QuarterHourOfDay,
            "calendar.quarter_hour",
        )
        .expect("route is valid"),
    ]
}

fn window() -> HistoryWindow {
    HistoryWindow::new(
        task(),
        binding(),
        vec![
            numeric("load", "kW"),
            numeric("temp_avg", "Cel"),
            numeric("quarter_hour", "1"),
        ],
        TimestampMs::new(1_783_767_600_000),
        TimestampMs::new(1_783_769_400_000),
        2,
        HistoryAggregation::Last,
        HistoryDuplicatePolicy::Reject,
    )
    .expect("window is valid")
}

#[test]
fn configuration_rejects_unsafe_endpoints_duplicate_routes_and_unbounded_limits() {
    for endpoint in [
        "http://example.com/hisApi/data/batch-query",
        "https://127.0.0.1/hisApi/data/batch-query",
        "http://user:secret@127.0.0.1/hisApi/data/batch-query",
        "http://127.0.0.1/other",
        "http://127.0.0.1/hisApi/data/batch-query?q=1",
    ] {
        assert!(HttpHistoryQueryConfig::new(endpoint, routes(), 1_000, 4096).is_err());
    }
    let mut duplicate = routes();
    duplicate.push(duplicate[0].clone());
    assert!(
        HttpHistoryQueryConfig::new(
            "http://127.0.0.1:6004/hisApi/data/batch-query",
            duplicate,
            1_000,
            4096,
        )
        .is_err()
    );
    let reused_series_in_one_task = vec![
        HistoryFeatureRoute::stored(
            task(),
            binding(),
            numeric("load", "kW"),
            CADENCE_MS,
            "inst:1:M",
            "1",
            "energy.site.load",
        )
        .expect("route is valid"),
        HistoryFeatureRoute::stored(
            task(),
            binding(),
            numeric("voltage", "V"),
            CADENCE_MS,
            "inst:1:M",
            "1",
            "energy.site.voltage",
        )
        .expect("route is structurally valid"),
    ];
    assert!(
        HttpHistoryQueryConfig::new(
            "http://127.0.0.1:6004/hisApi/data/batch-query",
            reused_series_in_one_task,
            1_000,
            4096,
        )
        .is_err()
    );

    let cross_task_reuse = vec![
        HistoryFeatureRoute::stored(
            task(),
            binding(),
            numeric("load", "kW"),
            CADENCE_MS,
            "inst:1:M",
            "1",
            "energy.site.load",
        )
        .expect("route is valid"),
        HistoryFeatureRoute::stored(
            TaskIdentity::new("iot.anomaly-detection", 1).expect("second task is valid"),
            binding(),
            numeric("load", "kW"),
            CADENCE_MS,
            "inst:1:M",
            "1",
            "energy.site.load",
        )
        .expect("cross-task route is valid"),
    ];
    assert!(
        HttpHistoryQueryConfig::new(
            "http://127.0.0.1:6004/hisApi/data/batch-query",
            cross_task_reuse,
            1_000,
            4096,
        )
        .is_ok()
    );
}

#[tokio::test]
async fn exact_history_and_calendar_are_projected_without_leaking_storage_coordinates() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/hisApi/data/batch-query"))
        .and(body_json(json!({
            "start_time": "2026-07-11T11:00:00Z",
            "end_time": "2026-07-11T11:30:00Z",
            "series": [
                {"series_key": "inst:1:M", "point_id": "1"},
                {"series_key": "inst:2:M", "point_id": "2"}
            ],
            "limit_per_series": 2
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "success": true,
            "message": "OK",
            "data": {
                "start_time": "2026-07-11T11:00:00Z",
                "end_time": "2026-07-11T11:30:00Z",
                "series": [
                    {
                        "series_key": "inst:1:M",
                        "point_id": "1",
                        "count": 2,
                        "data": [
                            {"time": "2026-07-11T11:00:00Z", "value": 810.0},
                            {"time": "2026-07-11T11:15:00Z", "value": 818.0}
                        ]
                    },
                    {
                        "series_key": "inst:2:M",
                        "point_id": "2",
                        "count": 1,
                        "data": [
                            {"time": "2026-07-11T11:00:00Z", "value": 30.8}
                        ]
                    }
                ]
            }
        })))
        .expect(1)
        .mount(&server)
        .await;
    let config = HttpHistoryQueryConfig::new(
        &format!("{}/hisApi/data/batch-query", server.uri()),
        routes(),
        2_000,
        64 * 1024,
    )
    .expect("configuration is safe");
    let adapter = HttpHistoryQuery::new(config).expect("adapter is valid");

    let sourced = adapter.query(window()).await.expect("history is resolved");

    assert_eq!(sourced.segment().sample_count(), 2);
    assert_eq!(
        sourced.segment().series()[0].values()[1].as_number(),
        Some(818.0)
    );
    assert_eq!(
        sourced.segment().series()[1].quality()[1],
        SampleQuality::Missing
    );
    assert_eq!(
        sourced.segment().series()[2].values()[0].as_number(),
        Some(44.0)
    );
    assert_eq!(
        sourced.segment().series()[2].values()[1].as_number(),
        Some(45.0)
    );
    assert_eq!(sourced.provenance()[0].source_kind(), SourceKind::History);
    assert_eq!(sourced.provenance()[2].source_kind(), SourceKind::Calendar);
    assert!(sourced.provenance().iter().all(|item| {
        item.source_ref()
            .is_some_and(|value| !value.contains("inst:"))
    }));
}

#[tokio::test]
async fn duplicate_or_off_grid_history_fails_closed() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "success": true,
            "message": "OK",
            "data": {
                "start_time": "2026-07-11T11:00:00Z",
                "end_time": "2026-07-11T11:30:00Z",
                "series": [{
                    "series_key": "inst:1:M",
                    "point_id": "1",
                    "count": 2,
                    "data": [
                        {"time": "2026-07-11T11:05:00Z", "value": 1.0},
                        {"time": "2026-07-11T11:05:00Z", "value": 2.0}
                    ]
                }]
            }
        })))
        .mount(&server)
        .await;
    let mut configured_routes = routes();
    let one_route = vec![configured_routes.remove(0)];
    let config = HttpHistoryQueryConfig::new(
        &format!("{}/hisApi/data/batch-query", server.uri()),
        one_route,
        2_000,
        64 * 1024,
    )
    .expect("configuration is safe");
    let adapter = HttpHistoryQuery::new(config).expect("adapter is valid");
    let one_feature = HistoryWindow::new(
        task(),
        binding(),
        vec![numeric("load", "kW")],
        TimestampMs::new(1_783_767_600_000),
        TimestampMs::new(1_783_769_400_000),
        2,
        HistoryAggregation::Last,
        HistoryDuplicatePolicy::Reject,
    )
    .expect("window is valid");

    let error = adapter
        .query(one_feature)
        .await
        .expect_err("bad history is rejected");
    assert_eq!(error.kind(), PortErrorKind::InvalidData);
}
