//! Unit coverage for `HistoryFeatureRoute` and `HttpHistoryQueryConfig`.
//!
//! Configuration is validated eagerly, so every rejection here must surface
//! `PortErrorKind::InvalidData` rather than a late failure at query time.

mod support;

use aether_domain::{FeatureDefinition, FeatureRole, FeatureValueType, TaskIdentity};
use aether_http_history_query::{
    CalendarFeature, HistoryFeatureRoute, HttpHistoryQuery, HttpHistoryQueryConfig,
};
use aether_ports::PortErrorKind;
use support::{
    BATCH_PATH, CADENCE_MS, binding, calendar_route, load_feature, numeric, quarter_hour_feature,
    stored_route, task,
};

const MAX_TIMEOUT_MS: u64 = 30_000;
const MAX_RESPONSE_BYTES: usize = 16 * 1024 * 1024;
const MAX_STORED_SERIES: usize = 20;

fn endpoint(host: &str) -> String {
    format!("http://{host}:6004{BATCH_PATH}")
}

fn config(
    endpoint: &str,
    routes: Vec<HistoryFeatureRoute>,
    timeout_ms: u64,
    max_response_bytes: usize,
) -> Result<HttpHistoryQueryConfig, aether_ports::PortError> {
    HttpHistoryQueryConfig::new(endpoint, routes, timeout_ms, max_response_bytes)
}

fn assert_invalid(result: Result<HttpHistoryQueryConfig, aether_ports::PortError>, context: &str) {
    match result {
        Ok(_) => panic!("{context} must not produce a usable configuration"),
        Err(error) => assert_eq!(
            error.kind(),
            PortErrorKind::InvalidData,
            "{context} must be rejected as invalid configuration"
        ),
    }
}

fn stored_defined(
    definition: FeatureDefinition,
    cadence_ms: u64,
    series_key: &str,
    point_id: &str,
) -> HistoryFeatureRoute {
    HistoryFeatureRoute::stored(
        task(),
        binding(),
        definition,
        cadence_ms,
        series_key,
        point_id,
        "energy.site.load.active_power",
    )
    .expect("distinct stored route is valid")
}

fn stored_named(feature: &str, series_key: &str, point_id: &str) -> HistoryFeatureRoute {
    stored_defined(numeric(feature, "kW"), CADENCE_MS, series_key, point_id)
}

/// `HistoryFeatureRoute::source` is `pub(crate)`, so a route's commissioned
/// coordinates are only observable through equality here. The guarantee that
/// storage coordinates never reach a processor is asserted end to end in
/// `http_history_query_projection.rs`.
#[test]
fn routes_compare_by_every_commissioned_coordinate() {
    assert_eq!(stored_route(), stored_route());
    assert_eq!(calendar_route(), calendar_route());
    assert_ne!(stored_route(), calendar_route());

    assert_ne!(
        stored_named("load", "inst:1:M", "1"),
        stored_named("load", "inst:1:M", "2"),
        "the point id is part of route identity"
    );
    assert_ne!(
        stored_named("load", "inst:1:M", "1"),
        stored_named("load", "inst:2:M", "1"),
        "the series key is part of route identity"
    );
    assert_ne!(
        stored_named("load", "inst:1:M", "1"),
        stored_named("voltage", "inst:1:M", "1"),
        "the semantic feature name is part of route identity"
    );
    assert_ne!(
        stored_route(),
        stored_defined(numeric("load", "MW"), CADENCE_MS, "inst:1:M", "1"),
        "the commissioned unit is part of route identity"
    );
    assert_ne!(
        stored_route(),
        stored_defined(load_feature(), CADENCE_MS * 2, "inst:1:M", "1"),
        "the commissioned cadence is part of route identity"
    );
}

#[test]
fn the_only_calendar_transform_is_comparable_and_copyable() {
    let transform = CalendarFeature::QuarterHourOfDay;
    let copied = transform;

    assert_eq!(transform, copied);
    assert_eq!(
        format!("{transform:?}"),
        "QuarterHourOfDay",
        "the transform name appears in commissioning diagnostics"
    );
}

#[test]
fn empty_control_and_non_semantic_route_fields_are_rejected() {
    for (series_key, point_id, source_ref, context) in [
        ("", "1", "energy.site.load", "an empty series key"),
        ("inst:1:M", "", "energy.site.load", "an empty point id"),
        (
            "inst:\u{7}1:M",
            "1",
            "energy.site.load",
            "a control character in the series key",
        ),
        ("inst:1:M", "1", "", "an empty source ref"),
        (
            "inst:1:M",
            "1",
            "inst:1:M",
            "a storage coordinate used as a source ref",
        ),
        (
            "inst:1:M",
            "1",
            ".leading-dot",
            "a source ref that does not start alphanumerically",
        ),
        (
            "inst:1:M",
            "1",
            "energy site load",
            "a source ref containing a space",
        ),
    ] {
        let error = HistoryFeatureRoute::stored(
            task(),
            binding(),
            load_feature(),
            CADENCE_MS,
            series_key,
            point_id,
            source_ref,
        )
        .expect_err(context);
        assert_eq!(
            error.kind(),
            PortErrorKind::InvalidData,
            "{context} must be invalid configuration"
        );
    }
}

#[test]
fn routes_require_an_exact_numeric_history_definition_and_positive_cadence() {
    let invalid_definitions = [
        FeatureDefinition::numeric("load", FeatureRole::FutureCovariate, "kW")
            .expect("future feature is structurally valid"),
        FeatureDefinition::new("load", FeatureRole::History, FeatureValueType::Text)
            .expect("text feature is structurally valid"),
        FeatureDefinition::new("load", FeatureRole::History, FeatureValueType::Number)
            .expect("unitless number is structurally valid"),
    ];
    for definition in invalid_definitions {
        let error = HistoryFeatureRoute::stored(
            task(),
            binding(),
            definition,
            CADENCE_MS,
            "inst:1:M",
            "1",
            "energy.site.load",
        )
        .expect_err("a non-history, non-numeric, or unitless definition must fail");
        assert_eq!(error.kind(), PortErrorKind::InvalidData);
    }

    let zero_cadence = HistoryFeatureRoute::stored(
        task(),
        binding(),
        load_feature(),
        0,
        "inst:1:M",
        "1",
        "energy.site.load",
    )
    .expect_err("a zero cadence must fail");
    assert_eq!(zero_cadence.kind(), PortErrorKind::InvalidData);

    let dimensional_calendar = HistoryFeatureRoute::calendar(
        task(),
        binding(),
        load_feature(),
        CADENCE_MS,
        CalendarFeature::QuarterHourOfDay,
        "calendar.quarter_hour",
    )
    .expect_err("a calendar feature must use the exact unit '1'");
    assert_eq!(dimensional_calendar.kind(), PortErrorKind::InvalidData);
}

#[test]
fn a_source_ref_at_the_length_limit_is_accepted_and_one_byte_over_is_not() {
    let at_limit = "a".repeat(2_048);
    assert!(
        HistoryFeatureRoute::calendar(
            task(),
            binding(),
            quarter_hour_feature(),
            CADENCE_MS,
            CalendarFeature::QuarterHourOfDay,
            at_limit,
        )
        .is_ok(),
        "2048 bytes is the inclusive semantic source-ref limit"
    );

    let over_limit = "a".repeat(2_049);
    let error = HistoryFeatureRoute::calendar(
        task(),
        binding(),
        quarter_hour_feature(),
        CADENCE_MS,
        CalendarFeature::QuarterHourOfDay,
        over_limit,
    )
    .expect_err("2049 bytes exceeds the semantic source-ref limit");
    assert_eq!(error.kind(), PortErrorKind::InvalidData);
}

#[test]
fn only_loopback_http_endpoints_on_the_batch_path_are_accepted() {
    for host in ["127.0.0.1", "127.0.0.2", "localhost", "LOCALHOST"] {
        assert!(
            config(&endpoint(host), vec![stored_route()], 1_000, 4_096).is_ok(),
            "{host} is a loopback history endpoint"
        );
    }

    for (candidate, context) in [
        (endpoint("0.0.0.0"), "an unspecified IPv4 address"),
        (
            format!("http://[::]:6004{BATCH_PATH}"),
            "an unspecified IPv6 address",
        ),
        (
            format!("http://[2001:db8::1]:6004{BATCH_PATH}"),
            "a non-loopback IPv6 address",
        ),
        (endpoint("10.0.0.1"), "a routable private address"),
        (endpoint("example.com"), "a public hostname"),
        (
            format!("https://127.0.0.1:6004{BATCH_PATH}"),
            "a TLS endpoint the adapter cannot verify",
        ),
        (
            format!("http://user@127.0.0.1:6004{BATCH_PATH}"),
            "an endpoint carrying a username",
        ),
        (
            format!("http://user:secret@127.0.0.1:6004{BATCH_PATH}"),
            "an endpoint carrying credentials",
        ),
        (
            format!("http://127.0.0.1:6004{BATCH_PATH}?limit=1"),
            "an endpoint carrying a query string",
        ),
        (
            format!("http://127.0.0.1:6004{BATCH_PATH}#fragment"),
            "an endpoint carrying a fragment",
        ),
        (
            "http://127.0.0.1:6004/hisApi/data/batch-query/".to_string(),
            "a trailing-slash path that is not the batch route",
        ),
        (
            "http://127.0.0.1:6004/other".to_string(),
            "an unrelated path",
        ),
        ("not-a-url".to_string(), "an unparsable endpoint"),
    ] {
        assert_invalid(
            config(&candidate, vec![stored_route()], 1_000, 4_096),
            context,
        );
    }
}

#[test]
fn ipv6_loopback_is_a_valid_history_endpoint() {
    assert!(
        config(
            &format!("http://[::1]:6004{BATCH_PATH}"),
            vec![stored_route()],
            1_000,
            4_096,
        )
        .is_ok(),
        "::1 is a loopback history endpoint"
    );
}

#[test]
fn timeout_and_response_limits_are_inclusive_at_their_boundaries() {
    for timeout_ms in [1, MAX_TIMEOUT_MS] {
        assert!(
            config(
                &endpoint("127.0.0.1"),
                vec![stored_route()],
                timeout_ms,
                4_096
            )
            .is_ok(),
            "{timeout_ms}ms is inside the accepted timeout range"
        );
    }
    assert_invalid(
        config(&endpoint("127.0.0.1"), vec![stored_route()], 0, 4_096),
        "a zero timeout",
    );
    assert_invalid(
        config(
            &endpoint("127.0.0.1"),
            vec![stored_route()],
            MAX_TIMEOUT_MS + 1,
            4_096,
        ),
        "a timeout one millisecond over the limit",
    );

    for max_response_bytes in [1, MAX_RESPONSE_BYTES] {
        assert!(
            config(
                &endpoint("127.0.0.1"),
                vec![stored_route()],
                1_000,
                max_response_bytes,
            )
            .is_ok(),
            "{max_response_bytes} bytes is inside the accepted response range"
        );
    }
    assert_invalid(
        config(&endpoint("127.0.0.1"), vec![stored_route()], 1_000, 0),
        "a zero response limit",
    );
    assert_invalid(
        config(
            &endpoint("127.0.0.1"),
            vec![stored_route()],
            1_000,
            MAX_RESPONSE_BYTES + 1,
        ),
        "a response limit one byte over the cap",
    );
}

#[test]
fn the_stored_series_fan_out_is_capped_but_calendar_routes_are_not_counted() {
    let at_limit = (0..MAX_STORED_SERIES)
        .map(|index| stored_named(&format!("load_{index}"), &format!("inst:{index}:M"), "1"))
        .collect::<Vec<_>>();
    assert!(
        config(&endpoint("127.0.0.1"), at_limit.clone(), 1_000, 4_096).is_ok(),
        "{MAX_STORED_SERIES} stored series is the inclusive fan-out limit"
    );

    let mut with_calendar = at_limit.clone();
    with_calendar.push(calendar_route());
    assert!(
        config(&endpoint("127.0.0.1"), with_calendar, 1_000, 4_096).is_ok(),
        "calendar routes are generated locally and do not consume the fan-out budget"
    );

    let mut over_limit = at_limit;
    over_limit.push(stored_named("load_extra", "inst:extra:M", "1"));
    assert_invalid(
        config(&endpoint("127.0.0.1"), over_limit, 1_000, 4_096),
        "one stored series over the fan-out limit",
    );
}

#[test]
fn a_configuration_without_routes_is_rejected() {
    assert_invalid(
        config(&endpoint("127.0.0.1"), Vec::new(), 1_000, 4_096),
        "a configuration with no feature routes",
    );
}

#[test]
fn one_task_may_not_reuse_a_feature_name_or_a_physical_series() {
    let duplicate_feature = vec![
        stored_named("load", "inst:1:M", "1"),
        stored_named("load", "inst:2:M", "2"),
    ];
    assert_invalid(
        config(&endpoint("127.0.0.1"), duplicate_feature, 1_000, 4_096),
        "two routes claiming the same feature name in one task",
    );

    let duplicate_series = vec![
        stored_named("load", "inst:1:M", "1"),
        stored_named("voltage", "inst:1:M", "1"),
    ];
    assert_invalid(
        config(&endpoint("127.0.0.1"), duplicate_series, 1_000, 4_096),
        "two features fed by the same physical series in one task",
    );

    let duplicate_transform = vec![calendar_route(), {
        HistoryFeatureRoute::calendar(
            task(),
            binding(),
            numeric("quarter_hour_alias", "1"),
            CADENCE_MS,
            CalendarFeature::QuarterHourOfDay,
            "calendar.quarter_hour_alias",
        )
        .expect("aliased calendar route is structurally valid")
    }];
    assert_invalid(
        config(&endpoint("127.0.0.1"), duplicate_transform, 1_000, 4_096),
        "two features fed by the same calendar transform in one task",
    );
}

#[test]
fn one_task_cannot_mix_commissioned_cadences() {
    let mixed = vec![
        stored_route(),
        stored_defined(numeric("voltage", "V"), CADENCE_MS * 2, "inst:2:M", "2"),
    ];
    assert_invalid(
        config(&endpoint("127.0.0.1"), mixed, 1_000, 4_096),
        "two cadences commissioned for one task and binding",
    );
}

#[test]
fn a_different_task_or_binding_may_reuse_the_same_physical_series() {
    let other_task = vec![stored_named("load", "inst:1:M", "1"), {
        HistoryFeatureRoute::stored(
            TaskIdentity::new("iot.anomaly-detection", 1).expect("second task is valid"),
            binding(),
            load_feature(),
            CADENCE_MS,
            "inst:1:M",
            "1",
            "energy.site.load.active_power",
        )
        .expect("cross-task route is valid")
    }];
    assert!(
        config(&endpoint("127.0.0.1"), other_task, 1_000, 4_096).is_ok(),
        "two tasks may derive different features from one physical series"
    );

    let other_binding = vec![stored_named("load", "inst:1:M", "1"), {
        HistoryFeatureRoute::stored(
            task(),
            aether_domain::BindingIdentity::new("energy.site-a", 2)
                .expect("second binding revision is valid"),
            load_feature(),
            CADENCE_MS,
            "inst:1:M",
            "1",
            "energy.site.load.active_power",
        )
        .expect("cross-binding route is valid")
    }];
    assert!(
        config(&endpoint("127.0.0.1"), other_binding, 1_000, 4_096).is_ok(),
        "binding revisions are commissioned independently"
    );
}

#[test]
fn a_validated_configuration_builds_a_client_without_contacting_the_endpoint() {
    let config = config(&endpoint("127.0.0.1"), vec![stored_route()], 1_000, 4_096)
        .expect("loopback configuration is safe");

    assert!(
        HttpHistoryQuery::new(config).is_ok(),
        "adapter construction must not require a reachable history service"
    );
}
