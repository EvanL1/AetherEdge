//! `aether-testkit` conformance suite for the `HistoryQuery` port.
//!
//! `HttpHistoryQuery` implements exactly one `aether-ports` trait,
//! [`aether_ports::HistoryQuery`], so the applicable checks are
//! `assert_history_query_bounded` and `assert_history_query_provenance`.
//! The remaining testkit assertions target ports this crate does not
//! implement (`DataProcessor`, `DelegatedDeviceProvider`, `LiveState`,
//! `DurableOutbox`, `IntegrationTopologyGenerationStore`).

mod support;

use aether_domain::{HistoryAggregation, HistoryDuplicatePolicy};
use aether_ports::HistoryQuery;
use aether_testkit::{assert_history_query_bounded, assert_history_query_provenance};
use support::{
    CADENCE_MS, WINDOW_START_MS, adapter_for, batch_data, batch_series, calendar_route, envelope,
    load_feature, mount_json, point, quarter_hour_feature, stored_route, window, window_spanning,
};
use wiremock::MockServer;

#[tokio::test]
async fn bounded_conformance_holds_for_a_single_sample_grid() {
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
    let requested = window_spanning(
        vec![load_feature(), quarter_hour_feature()],
        WINDOW_START_MS,
        WINDOW_START_MS + CADENCE_MS,
        1,
        HistoryAggregation::Last,
        HistoryDuplicatePolicy::Reject,
    );

    let expected = adapter
        .query(requested.clone())
        .await
        .expect("single-sample grid resolves");

    assert_history_query_bounded(&adapter, requested, expected)
        .await
        .expect("HTTP adapter satisfies bounded history conformance");
}

#[tokio::test]
async fn provenance_conformance_holds_for_stored_and_calendar_features() {
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
    let adapter = adapter_for(&server, vec![stored_route(), calendar_route()]);
    let requested = window(vec![load_feature(), quarter_hour_feature()], 2);

    let sourced = adapter
        .query(requested.clone())
        .await
        .expect("two-sample grid resolves");

    assert_history_query_provenance(&adapter, requested, sourced.provenance())
        .await
        .expect("HTTP adapter satisfies provenance conformance");
}

#[tokio::test]
async fn bounded_conformance_rejects_a_bound_below_the_commissioned_grid() {
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
    let adapter = adapter_for(&server, vec![stored_route(), calendar_route()]);
    let requested = window(vec![load_feature(), quarter_hour_feature()], 2);

    let expected = adapter
        .query(requested.clone())
        .await
        .expect("two-sample grid resolves");

    assert_history_query_bounded(&adapter, requested, expected)
        .await
        .expect("HTTP adapter satisfies bounded history conformance");
}
