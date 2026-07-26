//! Cancellation and concurrency coverage for the shared `HistoryQuery` adapter.
//!
//! `HistoryQuery` is `Send + Sync + 'static` and takes `&self`, so one adapter
//! is shared by every task in a composition root. These checks pin the two
//! properties that follow from that: dropping a query future must leave the
//! adapter usable, and concurrent queries must not observe each other.

mod support;

use std::sync::Arc;
use std::time::Duration;

use aether_http_history_query::{HttpHistoryQuery, HttpHistoryQueryConfig};
use aether_ports::{HistoryQuery, PortErrorKind};
use serde_json::Value;
use support::{
    BATCH_PATH, adapter_for, batch_data, batch_series, calendar_route, config_with_limits,
    envelope, load_feature, mount_json, point, quarter_hour_feature, stored_route, window,
};
use tokio::task::JoinSet;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn two_point_body() -> Value {
    envelope(batch_data(vec![batch_series(
        "inst:1:M",
        "1",
        vec![
            point("2026-07-11T11:00:00Z", Some(810.0)),
            point("2026-07-11T11:15:00Z", Some(818.0)),
        ],
    )]))
}

async fn mount_delayed(server: &MockServer, delay: Duration) {
    Mock::given(method("POST"))
        .and(path(BATCH_PATH))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(two_point_body())
                .set_delay(delay),
        )
        .mount(server)
        .await;
}

const fn assert_shared_port<T: HistoryQuery + ?Sized>() {}

#[test]
fn the_adapter_satisfies_the_shared_port_bounds() {
    assert_shared_port::<HttpHistoryQuery>();
    assert_shared_port::<dyn HistoryQuery>();
}

#[tokio::test]
async fn a_cancelled_query_leaves_the_adapter_usable() {
    let server = MockServer::start().await;
    mount_delayed(&server, Duration::from_secs(5)).await;
    let adapter = adapter_for(&server, vec![stored_route()]);

    let cancelled = tokio::time::timeout(
        Duration::from_millis(50),
        adapter.query(window(vec![load_feature()], 2)),
    )
    .await;
    assert!(
        cancelled.is_err(),
        "the fixture must actually cancel an in-flight query"
    );

    server.reset().await;
    mount_json(&server, 200, two_point_body()).await;

    let sourced = adapter
        .query(window(vec![load_feature()], 2))
        .await
        .expect("the adapter recovers after an abandoned query");
    assert_eq!(
        sourced.segment().series()[0].values()[1].as_number(),
        Some(818.0)
    );
}

#[tokio::test]
async fn cancelling_at_successive_deadlines_never_wedges_the_adapter() {
    let server = MockServer::start().await;
    mount_delayed(&server, Duration::from_millis(400)).await;
    let adapter = adapter_for(&server, vec![stored_route()]);

    for deadline_ms in [1, 5, 20, 60, 150] {
        let outcome = tokio::time::timeout(
            Duration::from_millis(deadline_ms),
            adapter.query(window(vec![load_feature()], 2)),
        )
        .await;
        if let Ok(Err(error)) = outcome {
            assert_eq!(
                error.kind(),
                PortErrorKind::Timeout,
                "an uncancelled failure at {deadline_ms}ms must still be a timeout"
            );
        }
    }

    server.reset().await;
    mount_json(&server, 200, two_point_body()).await;

    adapter
        .query(window(vec![load_feature()], 2))
        .await
        .expect("the adapter still serves requests after repeated cancellation");
}

#[tokio::test]
async fn a_cancelled_query_does_not_leak_a_response_into_the_next_one() {
    let server = MockServer::start().await;
    mount_delayed(&server, Duration::from_millis(300)).await;
    let adapter = adapter_for(&server, vec![stored_route()]);

    drop(
        tokio::time::timeout(
            Duration::from_millis(20),
            adapter.query(window(vec![load_feature()], 2)),
        )
        .await,
    );

    server.reset().await;
    mount_json(
        &server,
        200,
        envelope(batch_data(vec![batch_series(
            "inst:1:M",
            "1",
            vec![point("2026-07-11T11:15:00Z", Some(42.0))],
        )])),
    )
    .await;

    let sourced = adapter
        .query(window(vec![load_feature()], 2))
        .await
        .expect("the next query resolves");
    let load = &sourced.segment().series()[0];
    assert!(
        load.values()[0].is_missing(),
        "the abandoned response must not repopulate the first grid cell"
    );
    assert_eq!(load.values()[1].as_number(), Some(42.0));
}

#[tokio::test]
async fn concurrent_queries_on_one_adapter_all_observe_the_same_committed_response() {
    let server = MockServer::start().await;
    mount_json(&server, 200, two_point_body()).await;
    let adapter = Arc::new(adapter_for(&server, vec![stored_route()]));

    let mut tasks = JoinSet::new();
    for _ in 0..32 {
        let adapter = Arc::clone(&adapter);
        tasks.spawn(async move { adapter.query(window(vec![load_feature()], 2)).await });
    }

    let mut completed = 0;
    while let Some(joined) = tasks.join_next().await {
        let sourced = joined
            .expect("no query task panics")
            .expect("every concurrent query resolves");
        assert_eq!(sourced.segment().sample_count(), 2);
        assert_eq!(
            sourced.segment().series()[0].values()[1].as_number(),
            Some(818.0)
        );
        completed += 1;
    }
    assert_eq!(completed, 32);
}

#[tokio::test]
async fn concurrent_stored_and_calendar_windows_do_not_observe_each_other() {
    let server = MockServer::start().await;
    mount_json(&server, 200, two_point_body()).await;
    let adapter = Arc::new(adapter_for(&server, vec![stored_route(), calendar_route()]));

    let mut tasks = JoinSet::new();
    for index in 0..24 {
        let adapter = Arc::clone(&adapter);
        tasks.spawn(async move {
            if index % 2 == 0 {
                let sourced = adapter
                    .query(window(vec![load_feature()], 2))
                    .await
                    .expect("the stored window resolves");
                (
                    "stored",
                    sourced.segment().series()[0].values()[1].as_number(),
                )
            } else {
                let sourced = adapter
                    .query(window(vec![quarter_hour_feature()], 2))
                    .await
                    .expect("the calendar window resolves");
                (
                    "calendar",
                    sourced.segment().series()[0].values()[1].as_number(),
                )
            }
        });
    }

    let mut stored = 0;
    let mut calendar = 0;
    while let Some(joined) = tasks.join_next().await {
        match joined.expect("no query task panics") {
            ("stored", value) => {
                assert_eq!(value, Some(818.0));
                stored += 1;
            },
            ("calendar", value) => {
                assert_eq!(value, Some(45.0));
                calendar += 1;
            },
            (kind, _) => panic!("unexpected window kind {kind}"),
        }
    }
    assert_eq!((stored, calendar), (12, 12));

    let requests = server
        .received_requests()
        .await
        .expect("the mock server records requests");
    assert_eq!(
        requests.len(),
        12,
        "only the stored windows may reach the history service"
    );
}

#[tokio::test]
async fn a_shared_adapter_keeps_classifying_failures_per_call() {
    let server = MockServer::start().await;
    mount_json(&server, 503, Value::Null).await;
    let adapter = Arc::new(adapter_for(&server, vec![stored_route()]));

    let mut tasks = JoinSet::new();
    for _ in 0..8 {
        let adapter = Arc::clone(&adapter);
        tasks.spawn(async move {
            adapter
                .query(window(vec![load_feature()], 2))
                .await
                .err()
                .map(|error| error.kind())
        });
    }
    while let Some(joined) = tasks.join_next().await {
        assert_eq!(
            joined.expect("no query task panics"),
            Some(PortErrorKind::Unavailable),
            "a shared adapter must not collapse per-call error kinds"
        );
    }

    server.reset().await;
    mount_json(&server, 200, two_point_body()).await;
    adapter
        .query(window(vec![load_feature()], 2))
        .await
        .expect("the adapter recovers once the service returns");
}

#[tokio::test]
async fn a_per_call_timeout_does_not_shorten_a_later_call() {
    let server = MockServer::start().await;
    mount_delayed(&server, Duration::from_millis(500)).await;
    let config = config_with_limits(&server, vec![stored_route()], 150, 64 * 1024);
    let adapter = HttpHistoryQuery::new(config).expect("adapter builds");

    let timed_out = adapter
        .query(window(vec![load_feature()], 2))
        .await
        .expect_err("the first call exceeds its own timeout");
    assert_eq!(timed_out.kind(), PortErrorKind::Timeout);

    server.reset().await;
    mount_json(&server, 200, two_point_body()).await;

    let sourced = adapter
        .query(window(vec![load_feature()], 2))
        .await
        .expect("the configured timeout applies per call, not per adapter");
    assert_eq!(sourced.segment().sample_count(), 2);
}

#[tokio::test]
async fn an_adapter_survives_being_moved_across_tasks() {
    let server = MockServer::start().await;
    mount_json(&server, 200, two_point_body()).await;
    let endpoint = format!("{}{BATCH_PATH}", server.uri());

    let handle = tokio::spawn(async move {
        let config = HttpHistoryQueryConfig::new(&endpoint, vec![stored_route()], 2_000, 64 * 1024)
            .expect("configuration is safe");
        let adapter: Arc<dyn HistoryQuery> =
            Arc::new(HttpHistoryQuery::new(config).expect("adapter builds"));
        adapter.query(window(vec![load_feature()], 2)).await
    });

    let sourced = handle
        .await
        .expect("the spawned task completes")
        .expect("a moved adapter still resolves its window");
    assert_eq!(sourced.segment().sample_count(), 2);
}
