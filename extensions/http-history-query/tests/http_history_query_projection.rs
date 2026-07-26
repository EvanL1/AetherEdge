//! Unit coverage for the logical grid, value projection, and provenance that
//! `HttpHistoryQuery::query` derives from a batch response.

mod support;

use aether_domain::{HistoryAggregation, HistoryDuplicatePolicy, SampleQuality, SourceKind};
use aether_http_history_query::HistoryFeatureRoute;
use aether_ports::{HistoryQuery, HistoryWindow, PortErrorKind};
use support::{
    CADENCE_MS, WINDOW_END_MS, WINDOW_START_MS, adapter_for, batch_data, batch_series, binding,
    calendar_route, envelope, load_feature, mount_json, numeric, point, quarter_hour_feature,
    stored_route, task, window, window_spanning, window_with_policy,
};
use wiremock::MockServer;

/// 2026-07-11T00:00:00Z, the start of the UTC day used by calendar checks.
const DAY_START_MS: u64 = 1_783_728_000_000;
const DAY_MS: u64 = 86_400_000;

async fn assert_query_error(
    server: &MockServer,
    routes: Vec<HistoryFeatureRoute>,
    requested: HistoryWindow,
    expected: PortErrorKind,
    context: &str,
) {
    let adapter = adapter_for(server, routes);
    match adapter.query(requested).await {
        Ok(_) => panic!("{context} must not produce a segment"),
        Err(error) => assert_eq!(error.kind(), expected, "{context} reported the wrong kind"),
    }
}

#[tokio::test]
async fn stored_values_land_on_the_derived_grid_and_gaps_stay_explicitly_missing() {
    let server = MockServer::start().await;
    mount_json(
        &server,
        200,
        envelope(batch_data(vec![batch_series(
            "inst:1:M",
            "1",
            vec![point("2026-07-11T11:00:00Z", Some(810.0))],
        )])),
    )
    .await;
    let adapter = adapter_for(&server, vec![stored_route()]);

    let sourced = adapter
        .query(window(vec![load_feature()], 2))
        .await
        .expect("a partially populated series still projects onto the grid");

    let load = &sourced.segment().series()[0];
    assert_eq!(sourced.segment().sample_count(), 2);
    assert_eq!(load.values()[0].as_number(), Some(810.0));
    assert_eq!(load.quality()[0], SampleQuality::Good);
    assert!(
        load.values()[1].is_missing(),
        "an absent grid cell must not be interpolated"
    );
    assert_eq!(load.quality()[1], SampleQuality::Missing);
}

#[tokio::test]
async fn an_explicit_null_is_missing_rather_than_zero() {
    let server = MockServer::start().await;
    mount_json(
        &server,
        200,
        envelope(batch_data(vec![batch_series(
            "inst:1:M",
            "1",
            vec![
                point("2026-07-11T11:00:00Z", Some(810.0)),
                point("2026-07-11T11:15:00Z", None),
            ],
        )])),
    )
    .await;
    let adapter = adapter_for(&server, vec![stored_route()]);

    let sourced = adapter
        .query(window(vec![load_feature()], 2))
        .await
        .expect("an explicit null is a valid observation gap");

    let load = &sourced.segment().series()[0];
    assert!(load.values()[1].is_missing());
    assert_ne!(
        load.values()[1].as_number(),
        Some(0.0),
        "a null observation must never be read as a real zero"
    );
    assert_eq!(
        sourced.provenance()[0].watermark().get(),
        WINDOW_START_MS,
        "the watermark tracks the newest usable observation, not the newest row"
    );
}

#[tokio::test]
async fn a_series_whose_every_observation_is_null_fails_closed() {
    let server = MockServer::start().await;
    mount_json(
        &server,
        200,
        envelope(batch_data(vec![batch_series(
            "inst:1:M",
            "1",
            vec![point("2026-07-11T11:00:00Z", None)],
        )])),
    )
    .await;

    assert_query_error(
        &server,
        vec![stored_route()],
        window(vec![load_feature()], 2),
        PortErrorKind::InvalidData,
        "a series with no usable observation",
    )
    .await;
}

#[tokio::test]
async fn provenance_reports_the_semantic_ref_and_never_the_storage_coordinates() {
    let server = MockServer::start().await;
    mount_json(
        &server,
        200,
        envelope(batch_data(vec![batch_series(
            "inst:1:M",
            "1",
            vec![point("2026-07-11T11:00:00Z", Some(810.0))],
        )])),
    )
    .await;
    let adapter = adapter_for(&server, vec![stored_route(), calendar_route()]);

    let sourced = adapter
        .query(window(vec![load_feature(), quarter_hour_feature()], 2))
        .await
        .expect("mixed stored and calendar features resolve");

    assert_eq!(sourced.provenance().len(), 2);
    assert_eq!(sourced.provenance()[0].source_kind(), SourceKind::History);
    assert_eq!(sourced.provenance()[1].source_kind(), SourceKind::Calendar);
    assert_eq!(
        sourced.provenance()[0].source_ref(),
        Some("energy.site.load.active_power")
    );
    for source in sourced.provenance() {
        let reference = source
            .source_ref()
            .expect("every feature declares a source");
        assert!(
            !reference.contains("inst:") && !reference.contains(':'),
            "storage coordinates must not reach a processor, found {reference}"
        );
        assert!(
            source.issued_at().is_none(),
            "history provenance carries no forecast issue cut"
        );
        assert!(
            source.watermark().get() < WINDOW_END_MS,
            "a watermark must stay inside the half-open window"
        );
    }
}

#[tokio::test]
async fn a_calendar_only_window_is_generated_locally_without_any_history_request() {
    let server = MockServer::start().await;
    let adapter = adapter_for(&server, vec![calendar_route()]);

    let sourced = adapter
        .query(window(vec![quarter_hour_feature()], 2))
        .await
        .expect("calendar features need no stored series");

    assert_eq!(
        sourced.segment().series()[0].values()[0].as_number(),
        Some(44.0)
    );
    assert_eq!(
        sourced.segment().series()[0].values()[1].as_number(),
        Some(45.0)
    );
    let requests = server
        .received_requests()
        .await
        .expect("the mock server records requests");
    assert!(
        requests.is_empty(),
        "a calendar-only window must not contact the history service"
    );
}

#[tokio::test]
async fn the_quarter_hour_transform_spans_the_whole_utc_day_inclusively() {
    let server = MockServer::start().await;
    let adapter = adapter_for(&server, vec![calendar_route()]);
    let full_day = window_spanning(
        vec![quarter_hour_feature()],
        DAY_START_MS,
        DAY_START_MS + DAY_MS,
        96,
        HistoryAggregation::Last,
        HistoryDuplicatePolicy::Reject,
    );

    let sourced = adapter
        .query(full_day)
        .await
        .expect("a full day of quarter hours resolves");

    let values = sourced.segment().series()[0]
        .values()
        .iter()
        .map(aether_domain::FeatureValue::as_number)
        .collect::<Vec<_>>();
    assert_eq!(values.len(), 96);
    assert_eq!(values[0], Some(0.0), "midnight UTC is interval index 0");
    assert_eq!(values[95], Some(95.0), "23:45 UTC is interval index 95");
    assert!(
        values
            .iter()
            .enumerate()
            .all(|(index, value)| *value == Some(index as f64)),
        "the transform must be a dense zero-based index"
    );
    assert!(
        sourced.segment().series()[0]
            .quality()
            .iter()
            .all(|quality| *quality == SampleQuality::Good),
        "a deterministic local transform has no missing cells"
    );
}

#[tokio::test]
async fn a_window_not_divisible_by_its_commissioned_cadence_cannot_form_a_grid() {
    let server = MockServer::start().await;

    assert_query_error(
        &server,
        vec![calendar_route()],
        window_spanning(
            vec![quarter_hour_feature()],
            WINDOW_START_MS,
            WINDOW_START_MS + CADENCE_MS + 1,
            2,
            HistoryAggregation::Last,
            HistoryDuplicatePolicy::Reject,
        ),
        PortErrorKind::InvalidData,
        "a span one millisecond beyond the commissioned cadence",
    )
    .await;
}

#[tokio::test]
async fn a_loose_sample_bound_does_not_change_the_commissioned_grid() {
    let server = MockServer::start().await;
    let adapter = adapter_for(&server, vec![calendar_route()]);

    let sourced = adapter
        .query(window(vec![quarter_hour_feature()], 7))
        .await
        .expect("a bound above the two-sample grid is accepted");

    assert_eq!(sourced.segment().sample_count(), 2);
    assert_eq!(
        sourced.segment().series()[0].values()[1].as_number(),
        Some(45.0)
    );
}

#[tokio::test]
async fn the_per_series_sample_limit_is_inclusive_at_five_thousand() {
    let server = MockServer::start().await;
    let one_millisecond_route = HistoryFeatureRoute::calendar(
        task(),
        binding(),
        quarter_hour_feature(),
        1,
        aether_http_history_query::CalendarFeature::QuarterHourOfDay,
        "calendar.quarter_hour",
    )
    .expect("one-millisecond calendar route is valid for the boundary fixture");
    let adapter = adapter_for(&server, vec![one_millisecond_route.clone()]);

    let at_limit = adapter
        .query(window_spanning(
            vec![quarter_hour_feature()],
            WINDOW_START_MS,
            WINDOW_START_MS + 5_000,
            5_000,
            HistoryAggregation::Last,
            HistoryDuplicatePolicy::Reject,
        ))
        .await
        .expect("5000 samples is the inclusive aether-history per-series limit");
    assert_eq!(at_limit.segment().sample_count(), 5_000);

    assert_query_error(
        &server,
        vec![one_millisecond_route],
        window_spanning(
            vec![quarter_hour_feature()],
            WINDOW_START_MS,
            WINDOW_START_MS + 5_001,
            5_001,
            HistoryAggregation::Last,
            HistoryDuplicatePolicy::Reject,
        ),
        PortErrorKind::Permanent,
        "one sample over the aether-history per-series limit",
    )
    .await;
}

#[tokio::test]
async fn only_a_pre_aligned_last_value_series_with_duplicate_rejection_is_accepted() {
    let server = MockServer::start().await;

    for (aggregation, duplicate_policy, context) in [
        (
            HistoryAggregation::Mean,
            HistoryDuplicatePolicy::Reject,
            "an aggregation this adapter cannot perform",
        ),
        (
            HistoryAggregation::Sum,
            HistoryDuplicatePolicy::Reject,
            "a summing aggregation",
        ),
        (
            HistoryAggregation::Last,
            HistoryDuplicatePolicy::Latest,
            "a duplicate policy that silently resolves collisions",
        ),
    ] {
        assert_query_error(
            &server,
            vec![calendar_route()],
            window_with_policy(
                vec![quarter_hour_feature()],
                2,
                aggregation,
                duplicate_policy,
            ),
            PortErrorKind::Permanent,
            context,
        )
        .await;
    }
}

#[tokio::test]
async fn a_feature_task_or_binding_outside_the_commissioned_mapping_is_permanent() {
    let server = MockServer::start().await;

    assert_query_error(
        &server,
        vec![stored_route()],
        window(vec![numeric("unmapped", "kW")], 2),
        PortErrorKind::Permanent,
        "a feature with no commissioned route",
    )
    .await;

    let other_binding = HistoryWindow::new(
        task(),
        aether_domain::BindingIdentity::new("energy.site-a", 2).expect("binding is valid"),
        vec![load_feature()],
        aether_domain::TimestampMs::new(WINDOW_START_MS),
        aether_domain::TimestampMs::new(WINDOW_END_MS),
        2,
        HistoryAggregation::Last,
        HistoryDuplicatePolicy::Reject,
    )
    .expect("window is valid");
    assert_query_error(
        &server,
        vec![stored_route()],
        other_binding,
        PortErrorKind::Permanent,
        "a binding revision the route was not commissioned for",
    )
    .await;

    let other_task = HistoryWindow::new(
        aether_domain::TaskIdentity::new("iot.anomaly-detection", 1).expect("task is valid"),
        binding(),
        vec![load_feature()],
        aether_domain::TimestampMs::new(WINDOW_START_MS),
        aether_domain::TimestampMs::new(WINDOW_END_MS),
        2,
        HistoryAggregation::Last,
        HistoryDuplicatePolicy::Reject,
    )
    .expect("window is valid");
    assert_query_error(
        &server,
        vec![stored_route()],
        other_task,
        PortErrorKind::Permanent,
        "a task the route was not commissioned for",
    )
    .await;
}

#[tokio::test]
async fn off_grid_duplicate_and_out_of_window_points_all_fail_closed() {
    for (points, context) in [
        (
            vec![point("2026-07-11T11:05:00Z", Some(1.0))],
            "a point between two grid labels",
        ),
        (
            vec![
                point("2026-07-11T11:00:00Z", Some(1.0)),
                point("2026-07-11T11:00:00Z", Some(2.0)),
            ],
            "a duplicated grid label",
        ),
        (
            vec![point("2026-07-11T10:45:00Z", Some(1.0))],
            "a point before the window start",
        ),
        (
            vec![point("2026-07-11T11:30:00Z", Some(1.0))],
            "a point at the exclusive window end",
        ),
        (
            vec![point("not-a-timestamp", Some(1.0))],
            "a timestamp that is not RFC 3339",
        ),
        (
            vec![point("2026-07-11T11:00:00", Some(1.0))],
            "an RFC 3339 timestamp with no offset",
        ),
    ] {
        let server = MockServer::start().await;
        mount_json(
            &server,
            200,
            envelope(batch_data(vec![batch_series("inst:1:M", "1", points)])),
        )
        .await;

        assert_query_error(
            &server,
            vec![stored_route()],
            window(vec![load_feature()], 2),
            PortErrorKind::InvalidData,
            context,
        )
        .await;
    }
}

#[tokio::test]
async fn an_authoritative_cutoff_is_forwarded_and_later_observations_are_refused() {
    let server = MockServer::start().await;
    mount_json(
        &server,
        200,
        envelope(batch_data(vec![batch_series(
            "inst:1:M",
            "1",
            vec![
                point("2026-07-11T11:00:00Z", Some(810.0)),
                point("2026-07-11T11:15:00Z", Some(818.0)),
            ],
        )])),
    )
    .await;
    let cutoff_window = window(vec![load_feature()], 2)
        .with_cutoff(aether_domain::TimestampMs::new(WINDOW_START_MS + 450_000))
        .expect("a cutoff inside the window is valid");

    assert_query_error(
        &server,
        vec![stored_route()],
        cutoff_window,
        PortErrorKind::InvalidData,
        "an observation after the authoritative cutoff",
    )
    .await;

    let requests = server
        .received_requests()
        .await
        .expect("the mock server records requests");
    let body = requests
        .first()
        .and_then(|request| serde_json::from_slice::<serde_json::Value>(&request.body).ok())
        .expect("the adapter sent a JSON batch request");
    assert_eq!(
        body["end_time"], "2026-07-11T11:07:30Z",
        "the cutoff, not the window end, bounds the upstream request"
    );
    assert_eq!(body["limit_per_series"], 2);
}

#[tokio::test]
async fn a_response_that_adds_omits_or_renames_a_series_is_refused() {
    for (series, context) in [
        (Vec::new(), "a response omitting the requested series"),
        (
            vec![
                batch_series(
                    "inst:1:M",
                    "1",
                    vec![point("2026-07-11T11:00:00Z", Some(1.0))],
                ),
                batch_series(
                    "inst:9:M",
                    "9",
                    vec![point("2026-07-11T11:00:00Z", Some(2.0))],
                ),
            ],
            "a response adding an unrequested series",
        ),
        (
            vec![batch_series(
                "inst:9:M",
                "9",
                vec![point("2026-07-11T11:00:00Z", Some(1.0))],
            )],
            "a response renaming the requested series",
        ),
    ] {
        let server = MockServer::start().await;
        mount_json(&server, 200, envelope(batch_data(series))).await;

        assert_query_error(
            &server,
            vec![stored_route()],
            window(vec![load_feature()], 2),
            PortErrorKind::InvalidData,
            context,
        )
        .await;
    }
}

#[tokio::test]
async fn a_declared_count_that_disagrees_with_the_payload_is_refused() {
    let server = MockServer::start().await;
    mount_json(
        &server,
        200,
        envelope(batch_data(vec![serde_json::json!({
            "series_key": "inst:1:M",
            "point_id": "1",
            "count": 5,
            "data": [{"time": "2026-07-11T11:00:00Z", "value": 810.0}],
        })])),
    )
    .await;

    assert_query_error(
        &server,
        vec![stored_route()],
        window(vec![load_feature()], 2),
        PortErrorKind::InvalidData,
        "a declared count that disagrees with the returned rows",
    )
    .await;
}

#[tokio::test]
async fn more_rows_than_the_requested_bound_are_refused() {
    let server = MockServer::start().await;
    mount_json(
        &server,
        200,
        envelope(batch_data(vec![batch_series(
            "inst:1:M",
            "1",
            vec![
                point("2026-07-11T11:00:00Z", Some(1.0)),
                point("2026-07-11T11:10:00Z", Some(2.0)),
                point("2026-07-11T11:20:00Z", Some(3.0)),
            ],
        )])),
    )
    .await;

    assert_query_error(
        &server,
        vec![stored_route()],
        window(vec![load_feature()], 2),
        PortErrorKind::InvalidData,
        "a response exceeding the requested per-series bound",
    )
    .await;
}
