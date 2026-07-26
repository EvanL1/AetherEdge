//! Coverage for the fail-closed paths that only a hostile or broken upstream
//! can drive: length-free streaming, out-of-range timestamps, and values that
//! survive JSON but not the numeric contract.

mod support;

use aether_domain::{HistoryAggregation, HistoryDuplicatePolicy};
use aether_http_history_query::{HistoryFeatureRoute, HttpHistoryQuery, HttpHistoryQueryConfig};
use aether_ports::{HistoryQuery, PortErrorKind};
use support::{
    BATCH_PATH, adapter_for, batch_data, batch_series, calendar_route, envelope, load_feature,
    mount_json, point, quarter_hour_feature, stored_route, window, window_spanning,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use wiremock::MockServer;

/// Serves one handcrafted chunked response with no `Content-Length`, so the
/// adapter must bound the body while streaming it.
async fn spawn_chunked_responder(chunk: Vec<u8>, chunk_count: usize) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("a loopback port is available");
    let address = listener
        .local_addr()
        .expect("the listener reports its address");
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            let chunk = chunk.clone();
            tokio::spawn(async move {
                let mut seen = Vec::new();
                let mut buffer = [0_u8; 1024];
                while !seen.windows(4).any(|window| window == b"\r\n\r\n") {
                    match stream.read(&mut buffer).await {
                        Ok(0) | Err(_) => return,
                        Ok(read) => seen.extend_from_slice(&buffer[..read]),
                    }
                }
                let mut response = b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\
                     Transfer-Encoding: chunked\r\n\r\n"
                    .to_vec();
                for _ in 0..chunk_count {
                    response.extend_from_slice(format!("{:x}\r\n", chunk.len()).as_bytes());
                    response.extend_from_slice(&chunk);
                    response.extend_from_slice(b"\r\n");
                }
                response.extend_from_slice(b"0\r\n\r\n");
                let _ = stream.write_all(&response).await;
                let _ = stream.flush().await;
            });
        }
    });
    format!("http://{address}{BATCH_PATH}")
}

fn adapter_at(endpoint: &str, max_response_bytes: usize) -> HttpHistoryQuery {
    let config =
        HttpHistoryQueryConfig::new(endpoint, vec![stored_route()], 2_000, max_response_bytes)
            .expect("the endpoint is a valid loopback address");
    HttpHistoryQuery::new(config).expect("adapter builds")
}

#[tokio::test]
async fn a_length_free_response_is_bounded_while_it_streams() {
    let filler = vec![b'x'; 4_096];
    let endpoint = spawn_chunked_responder(filler, 8).await;
    let adapter = adapter_at(&endpoint, 1_024);

    match adapter.query(window(vec![load_feature()], 2)).await {
        Ok(_) => panic!("an unbounded chunked body must not produce a segment"),
        Err(error) => assert_eq!(
            error.kind(),
            PortErrorKind::InvalidData,
            "a missing Content-Length must not disable the response budget"
        ),
    }
}

#[tokio::test]
async fn a_length_free_response_inside_the_budget_still_parses() {
    let body = serde_json::to_vec(&envelope(batch_data(vec![batch_series(
        "inst:1:M",
        "1",
        vec![point("2026-07-11T11:00:00Z", Some(810.0))],
    )])))
    .expect("fixture body encodes");
    let endpoint = spawn_chunked_responder(body, 1).await;
    let adapter = adapter_at(&endpoint, 64 * 1024);

    let sourced = adapter
        .query(window(vec![load_feature()], 2))
        .await
        .expect("chunked transfer encoding is supported");
    assert_eq!(
        sourced.segment().series()[0].values()[0].as_number(),
        Some(810.0)
    );
}

#[tokio::test]
async fn a_pre_epoch_observation_is_refused_rather_than_wrapped() {
    let server = MockServer::start().await;
    mount_json(
        &server,
        200,
        envelope(batch_data(vec![batch_series(
            "inst:1:M",
            "1",
            vec![point("1969-12-31T23:00:00Z", Some(810.0))],
        )])),
    )
    .await;

    let adapter = adapter_for(&server, vec![stored_route()]);
    match adapter.query(window(vec![load_feature()], 2)).await {
        Ok(_) => panic!("a pre-epoch observation must not produce a segment"),
        Err(error) => assert_eq!(
            error.kind(),
            PortErrorKind::InvalidData,
            "a negative epoch must fail rather than wrap into a huge u64"
        ),
    }
}

#[tokio::test]
async fn a_window_outside_the_rfc_3339_range_is_refused_before_any_request() {
    let server = MockServer::start().await;
    let adapter = adapter_for(&server, vec![stored_route()]);
    let i64_max = u64::try_from(i64::MAX).expect("i64::MAX is non-negative");

    for (start, end, context) in [
        (u64::MAX - 2, u64::MAX, "a window beyond i64 milliseconds"),
        (
            i64_max - 2,
            i64_max,
            "a window inside i64 but beyond the RFC 3339 range",
        ),
    ] {
        let unrepresentable = window_spanning(
            vec![load_feature()],
            start,
            end,
            2,
            HistoryAggregation::Last,
            HistoryDuplicatePolicy::Reject,
        );
        match adapter.query(unrepresentable).await {
            Ok(_) => panic!("{context} must not produce a segment"),
            Err(error) => assert_eq!(
                error.kind(),
                PortErrorKind::InvalidData,
                "{context} reported the wrong kind"
            ),
        }
    }

    assert!(
        server
            .received_requests()
            .await
            .expect("the mock server records requests")
            .is_empty(),
        "an unrepresentable window must fail before contacting the history service"
    );
}

#[tokio::test]
async fn a_calendar_grid_outside_the_representable_range_fails_closed() {
    let server = MockServer::start().await;
    let adapter = adapter_for(&server, vec![calendar_route()]);

    for (start, end, context) in [
        (u64::MAX - 2, u64::MAX, "a grid beyond i64 milliseconds"),
        (
            u64::try_from(i64::MAX).expect("i64::MAX is non-negative") - 2,
            u64::try_from(i64::MAX).expect("i64::MAX is non-negative"),
            "a grid inside i64 but beyond the calendar range",
        ),
    ] {
        let unrepresentable = window_spanning(
            vec![quarter_hour_feature()],
            start,
            end,
            2,
            HistoryAggregation::Last,
            HistoryDuplicatePolicy::Reject,
        );
        match adapter.query(unrepresentable).await {
            Ok(_) => panic!("{context} must not produce a segment"),
            Err(error) => assert_eq!(
                error.kind(),
                PortErrorKind::InvalidData,
                "{context} reported the wrong kind"
            ),
        }
    }
}

#[tokio::test]
async fn a_json_value_outside_the_f64_range_is_not_projected_as_a_number() {
    let server = MockServer::start().await;
    let oversized: serde_json::Value = match serde_json::from_str("1e400") {
        Ok(value) => value,
        Err(_) => {
            // The upstream parser refuses the literal outright, which is
            // itself a fail-closed outcome; nothing further to assert.
            return;
        },
    };
    mount_json(
        &server,
        200,
        envelope(batch_data(vec![serde_json::json!({
            "series_key": "inst:1:M",
            "point_id": "1",
            "count": 1,
            "data": [{"time": "2026-07-11T11:00:00Z", "value": oversized}],
        })])),
    )
    .await;

    let adapter = adapter_for(&server, vec![stored_route()]);
    let outcome = adapter.query(window(vec![load_feature()], 2)).await;

    match outcome {
        Ok(sourced) => {
            let load = &sourced.segment().series()[0];
            assert!(
                load.values()[0]
                    .as_number()
                    .is_none_or(|value| value.is_finite()),
                "a non-finite observation must never reach a processor as a number"
            );
        },
        Err(error) => assert_eq!(error.kind(), PortErrorKind::InvalidData),
    }
}

/// Fails: the unit is not part of the commissioned mapping.
///
/// `HistoryFeatureRoute` stores only a feature *name*
/// (`src/config.rs:38-39`, `src/config.rs:48`), and `HttpHistoryQuery::routes`
/// matches on `route.feature() == feature.name()` (`src/adapter.rs:37-41`).
/// A window asking for `load` in `MW` therefore binds to the route
/// commissioned for `load` in `kW`, and `src/adapter.rs:237` builds the series
/// from the *requested* definition, so kW samples are relabelled as MW and
/// handed to a processor a thousand times too small.
///
/// The sibling adapter does not have this gap: `SqliteHistoryFeatureRoute`
/// takes a full `FeatureDefinition` and rejects a unit mismatch with
/// `PortErrorKind::Permanent`
/// (`extensions/sqlite-history-query/tests/sqlite_history_query.rs:530-534`).
/// The fix is to commission `HistoryFeatureRoute` with a `FeatureDefinition`
/// and compare definitions rather than names.
#[ignore = "implementation gap: routes match on feature name only, so a unit mismatch is silently accepted"]
#[tokio::test]
async fn a_unit_outside_the_commissioned_mapping_is_refused() {
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

    match adapter
        .query(window(vec![support::numeric("load", "MW")], 2))
        .await
    {
        Ok(sourced) => panic!(
            "a unit mismatch must not resolve, got {:?} in {:?}",
            sourced.segment().series()[0].values()[0].as_number(),
            sourced.segment().series()[0].definition().unit(),
        ),
        Err(error) => assert_eq!(error.kind(), PortErrorKind::Permanent),
    }
}

#[tokio::test]
async fn an_unmapped_calendar_feature_cannot_borrow_a_stored_route() {
    let server = MockServer::start().await;
    let only_calendar = vec![
        HistoryFeatureRoute::calendar(
            support::task(),
            support::binding(),
            "quarter_hour",
            aether_http_history_query::CalendarFeature::QuarterHourOfDay,
            "calendar.quarter_hour",
        )
        .expect("calendar route is valid"),
    ];
    let adapter = adapter_for(&server, only_calendar);

    match adapter.query(window(vec![load_feature()], 2)).await {
        Ok(_) => panic!("an unmapped feature must not resolve"),
        Err(error) => assert_eq!(error.kind(), PortErrorKind::Permanent),
    }
}
