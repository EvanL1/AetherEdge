//! Unit coverage for the transport boundary: status mapping, response bounds,
//! envelope validation, and timeout classification.
//!
//! The extension contract requires transient transport failure to stay
//! distinguishable from a refused request and from an unusable payload, so
//! every case here asserts an exact `PortErrorKind`.

mod support;

use std::time::Duration;

use aether_http_history_query::{HttpHistoryQuery, HttpHistoryQueryConfig};
use aether_ports::{HistoryQuery, PortErrorKind};
use serde_json::{Value, json};
use support::{
    BATCH_PATH, adapter_for, batch_data, batch_series, config_with_limits, envelope, load_feature,
    mount_json, point, stored_route, window,
};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn one_point_body() -> Value {
    envelope(batch_data(vec![batch_series(
        "inst:1:M",
        "1",
        vec![point("2026-07-11T11:00:00Z", Some(810.0))],
    )]))
}

async fn assert_kind(server: &MockServer, expected: PortErrorKind, context: &str) {
    let adapter = adapter_for(server, vec![stored_route()]);
    match adapter.query(window(vec![load_feature()], 2)).await {
        Ok(_) => panic!("{context} must not produce a segment"),
        Err(error) => assert_eq!(error.kind(), expected, "{context} reported the wrong kind"),
    }
}

#[tokio::test]
async fn client_errors_are_rejected_and_server_errors_stay_transient() {
    for status in [400, 401, 403, 404, 409, 422, 429] {
        let server = MockServer::start().await;
        mount_json(&server, status, json!({})).await;
        assert_kind(
            &server,
            PortErrorKind::Rejected,
            &format!("a {status} from the history service"),
        )
        .await;
    }

    for status in [500, 502, 503, 504] {
        let server = MockServer::start().await;
        mount_json(&server, status, json!({})).await;
        assert_kind(
            &server,
            PortErrorKind::Unavailable,
            &format!("a {status} from the history service"),
        )
        .await;
    }
}

#[tokio::test]
async fn a_non_ok_success_status_is_not_treated_as_a_usable_response() {
    for status in [201, 202, 204] {
        let server = MockServer::start().await;
        mount_json(&server, status, one_point_body()).await;
        assert_kind(
            &server,
            PortErrorKind::Rejected,
            &format!("a {status} carrying an otherwise valid body"),
        )
        .await;
    }
}

#[tokio::test]
async fn redirects_are_never_followed() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(BATCH_PATH))
        .respond_with(
            ResponseTemplate::new(302).insert_header("location", "http://127.0.0.1:1/elsewhere"),
        )
        .mount(&server)
        .await;

    assert_kind(
        &server,
        PortErrorKind::Rejected,
        "a redirect away from the commissioned endpoint",
    )
    .await;

    let requests = server
        .received_requests()
        .await
        .expect("the mock server records requests");
    assert_eq!(
        requests.len(),
        1,
        "the adapter must not chase the redirect target"
    );
}

/// Holds a loopback port bound so the address cannot be reused by a parallel
/// test, and drops every accepted connection before any response is written.
async fn spawn_connection_dropper() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("a loopback port is available");
    let address = listener
        .local_addr()
        .expect("the listener reports its address");
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            drop(stream);
        }
    });
    format!("http://{address}{BATCH_PATH}")
}

#[tokio::test]
async fn a_history_service_that_drops_the_connection_is_transient_rather_than_permanent() {
    let endpoint = spawn_connection_dropper().await;
    let config = HttpHistoryQueryConfig::new(&endpoint, vec![stored_route()], 1_000, 4_096)
        .expect("the endpoint is a valid loopback address");
    let adapter = HttpHistoryQuery::new(config).expect("adapter builds without a live service");

    match adapter.query(window(vec![load_feature()], 2)).await {
        Ok(_) => panic!("a dropped connection must not produce a segment"),
        Err(error) => assert_eq!(
            error.kind(),
            PortErrorKind::Unavailable,
            "connection loss is transient, not permanent"
        ),
    }
}

#[tokio::test]
async fn a_slow_history_service_is_reported_as_a_timeout() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(BATCH_PATH))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(one_point_body())
                .set_delay(Duration::from_secs(5)),
        )
        .mount(&server)
        .await;
    let config = config_with_limits(&server, vec![stored_route()], 200, 64 * 1024);
    let adapter = HttpHistoryQuery::new(config).expect("adapter builds");

    match adapter.query(window(vec![load_feature()], 2)).await {
        Ok(_) => panic!("a stalled service must not produce a segment"),
        Err(error) => assert_eq!(
            error.kind(),
            PortErrorKind::Timeout,
            "a stalled service is a timeout, not a generic outage"
        ),
    }
}

#[tokio::test]
async fn a_response_over_the_configured_limit_is_refused_before_it_is_parsed() {
    let server = MockServer::start().await;
    mount_json(&server, 200, one_point_body()).await;
    let adapter =
        HttpHistoryQuery::new(config_with_limits(&server, vec![stored_route()], 2_000, 16))
            .expect("a 16-byte response budget is a valid configuration");

    match adapter.query(window(vec![load_feature()], 2)).await {
        Ok(_) => panic!("an oversized response must not produce a segment"),
        Err(error) => assert_eq!(error.kind(), PortErrorKind::InvalidData),
    }
}

#[tokio::test]
async fn a_response_inside_the_configured_limit_is_accepted() {
    let server = MockServer::start().await;
    let body = one_point_body();
    let encoded = serde_json::to_vec(&body).expect("fixture body encodes");
    mount_json(&server, 200, body).await;
    let adapter = HttpHistoryQuery::new(config_with_limits(
        &server,
        vec![stored_route()],
        2_000,
        encoded.len(),
    ))
    .expect("a budget equal to the payload is valid");

    let sourced = adapter
        .query(window(vec![load_feature()], 2))
        .await
        .expect("a response exactly at the budget is accepted");
    assert_eq!(sourced.segment().sample_count(), 2);
}

#[tokio::test]
async fn a_payload_that_is_not_the_expected_contract_is_invalid_data() {
    for (body, context) in [
        (json!("not an object"), "a JSON scalar"),
        (json!({"success": true}), "an envelope missing its data"),
        (
            json!({"success": true, "message": "OK", "data": {}}),
            "a data object missing its time bounds",
        ),
        (
            json!({
                "success": true,
                "message": "OK",
                "data": {"start_time": "", "end_time": "", "series": []},
                "extra": 1,
            }),
            "an envelope carrying an undeclared field",
        ),
        (
            json!({
                "success": true,
                "message": "OK",
                "data": {
                    "start_time": "",
                    "end_time": "",
                    "series": [{
                        "series_key": "inst:1:M",
                        "point_id": "1",
                        "count": 1,
                        "data": [{"time": "2026-07-11T11:00:00Z", "value": 1.0, "extra": 2}],
                    }],
                },
            }),
            "a point carrying an undeclared field",
        ),
    ] {
        let server = MockServer::start().await;
        mount_json(&server, 200, body).await;
        assert_kind(&server, PortErrorKind::InvalidData, context).await;
    }
}

#[tokio::test]
async fn malformed_json_is_invalid_data_rather_than_a_transport_failure() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(BATCH_PATH))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_raw(b"{\"success\": true, ".to_vec(), "application/json"),
        )
        .mount(&server)
        .await;

    assert_kind(&server, PortErrorKind::InvalidData, "a truncated JSON body").await;
}

#[tokio::test]
async fn an_envelope_reporting_failure_or_no_diagnostic_is_refused() {
    for (body, context) in [
        (
            json!({
                "success": false,
                "message": "history is rebuilding",
                "data": {"start_time": "", "end_time": "", "series": []},
            }),
            "an envelope that reports failure",
        ),
        (
            json!({
                "success": true,
                "message": "   ",
                "data": {"start_time": "", "end_time": "", "series": []},
            }),
            "a successful envelope with no diagnostic message",
        ),
    ] {
        let server = MockServer::start().await;
        mount_json(&server, 200, body).await;
        assert_kind(&server, PortErrorKind::InvalidData, context).await;
    }
}

#[tokio::test]
async fn the_upstream_request_carries_only_commissioned_coordinates() {
    let server = MockServer::start().await;
    mount_json(&server, 200, one_point_body()).await;
    let adapter = adapter_for(&server, vec![stored_route()]);

    adapter
        .query(window(vec![load_feature()], 2))
        .await
        .expect("the query succeeds");

    let requests = server
        .received_requests()
        .await
        .expect("the mock server records requests");
    let body: Value =
        serde_json::from_slice(&requests[0].body).expect("the adapter sends a JSON body");
    assert_eq!(
        body,
        json!({
            "start_time": "2026-07-11T11:00:00Z",
            "end_time": "2026-07-11T11:30:00Z",
            "series": [{"series_key": "inst:1:M", "point_id": "1"}],
            "limit_per_series": 2,
        }),
        "the batch request must contain no task, binding, or semantic metadata"
    );
}
