use aether_domain::{
    BindingIdentity, FeatureDefinition, FeatureRole, FeatureValue, HistoryAggregation,
    HistoryDuplicatePolicy, SampleQuality, Segment, SegmentKind, Series, SourceKind,
    SourceProvenance, TaskIdentity, TimestampMs,
};
use aether_ports::{
    CovariateSource, CovariateWindow, HistoryQuery, HistoryWindow, PortErrorKind, SourcedSegment,
};
use aether_store_local::{MemoryCovariateSource, MemoryHistoryQuery};
use aether_testkit::{assert_history_query_bounded, assert_history_query_provenance};

fn binding() -> BindingIdentity {
    BindingIdentity::new("site-a", 1).expect("binding is valid")
}

fn task() -> TaskIdentity {
    TaskIdentity::new("iot.history-test", 1).expect("task is valid")
}

fn numeric_feature(name: &str, role: FeatureRole, unit: &str) -> FeatureDefinition {
    FeatureDefinition::numeric(name, role, unit).expect("feature is valid")
}

fn numeric_series(definition: FeatureDefinition, values: &[f64]) -> Series {
    Series::new(
        definition,
        values
            .iter()
            .map(|value| FeatureValue::number(*value).expect("value is finite"))
            .collect(),
        vec![SampleQuality::Good; values.len()],
    )
    .expect("series is valid")
}

fn history_data() -> SourcedSegment {
    let load = numeric_feature("load", FeatureRole::History, "kW");
    let temperature = numeric_feature("temperature", FeatureRole::History, "Cel");
    let segment = Segment::new(
        vec![
            TimestampMs::new(1_000),
            TimestampMs::new(2_000),
            TimestampMs::new(3_000),
        ],
        vec![
            numeric_series(load, &[10.0, 11.0, 12.0]),
            numeric_series(temperature, &[20.0, 21.0, 22.0]),
        ],
    )
    .expect("segment is valid");
    SourcedSegment::new(
        segment,
        vec![
            SourceProvenance::new(
                SegmentKind::History,
                "load",
                SourceKind::History,
                Some("site.load"),
                TimestampMs::new(3_000),
            )
            .expect("provenance is valid"),
            SourceProvenance::new(
                SegmentKind::History,
                "temperature",
                SourceKind::History,
                Some("site.temperature"),
                TimestampMs::new(3_000),
            )
            .expect("provenance is valid"),
        ],
    )
    .expect("sourced segment is valid")
}

fn future_data() -> SourcedSegment {
    let temperature = numeric_feature("temperature", FeatureRole::FutureCovariate, "Cel");
    let segment = Segment::new(
        vec![TimestampMs::new(4_000), TimestampMs::new(5_000)],
        vec![numeric_series(temperature, &[23.0, 24.0])],
    )
    .expect("segment is valid");
    let provenance = SourceProvenance::new(
        SegmentKind::FutureCovariates,
        "temperature",
        SourceKind::Covariate,
        Some("weather.nwp.temperature"),
        TimestampMs::new(3_000),
    )
    .expect("provenance is valid")
    .with_issued_at(TimestampMs::new(2_500))
    .expect("issue cut is valid");
    SourcedSegment::new(segment, vec![provenance]).expect("sourced segment is valid")
}

#[tokio::test]
async fn memory_history_query_selects_declared_features_and_half_open_window() {
    let source = MemoryHistoryQuery::new();
    source
        .replace(binding(), history_data())
        .expect("fixture is accepted");

    let result = source
        .query(
            HistoryWindow::new(
                task(),
                binding(),
                vec![numeric_feature("load", FeatureRole::History, "kW")],
                TimestampMs::new(2_000),
                TimestampMs::new(4_000),
                2,
                HistoryAggregation::Mean,
                HistoryDuplicatePolicy::Latest,
            )
            .expect("window is valid"),
        )
        .await
        .expect("bounded history is available");

    assert_eq!(
        result.segment().timestamps(),
        &[TimestampMs::new(2_000), TimestampMs::new(3_000)]
    );
    assert_eq!(result.segment().series().len(), 1);
    assert_eq!(result.segment().series()[0].definition().name(), "load");
    assert_eq!(result.provenance().len(), 1);
}

#[tokio::test]
async fn memory_history_query_rejects_over_limit_and_unknown_bindings() {
    let source = MemoryHistoryQuery::new();
    source
        .replace(binding(), history_data())
        .expect("fixture is accepted");

    let over_limit = source
        .query(
            HistoryWindow::new(
                task(),
                binding(),
                vec![numeric_feature("load", FeatureRole::History, "kW")],
                TimestampMs::new(1_000),
                TimestampMs::new(4_000),
                2,
                HistoryAggregation::Mean,
                HistoryDuplicatePolicy::Latest,
            )
            .expect("window is valid"),
        )
        .await
        .expect_err("the adapter must not silently truncate a frame");
    assert_eq!(over_limit.kind(), PortErrorKind::Rejected);

    let unknown = source
        .query(
            HistoryWindow::new(
                task(),
                BindingIdentity::new("unknown", 1).expect("identity is valid"),
                vec![numeric_feature("load", FeatureRole::History, "kW")],
                TimestampMs::new(1_000),
                TimestampMs::new(2_000),
                2,
                HistoryAggregation::Mean,
                HistoryDuplicatePolicy::Latest,
            )
            .expect("window is valid"),
        )
        .await
        .expect_err("unknown binding is a permanent commissioning error");
    assert_eq!(unknown.kind(), PortErrorKind::Permanent);
    assert_eq!(unknown.message(), "history binding not found");
}

#[tokio::test]
async fn memory_history_query_preserves_requested_feature_and_provenance_order() {
    let source = MemoryHistoryQuery::new();
    source
        .replace(binding(), history_data())
        .expect("fixture is accepted");
    let window = HistoryWindow::new(
        task(),
        binding(),
        vec![
            numeric_feature("temperature", FeatureRole::History, "Cel"),
            numeric_feature("load", FeatureRole::History, "kW"),
        ],
        TimestampMs::new(1_000),
        TimestampMs::new(4_000),
        3,
        HistoryAggregation::Mean,
        HistoryDuplicatePolicy::Latest,
    )
    .expect("window is valid");

    let result = source
        .query(window)
        .await
        .expect("ordered projection is available");

    assert_eq!(
        result
            .segment()
            .series()
            .iter()
            .map(|series| series.definition().name())
            .collect::<Vec<_>>(),
        vec!["temperature", "load"]
    );
    assert_eq!(
        result
            .provenance()
            .iter()
            .map(SourceProvenance::feature)
            .collect::<Vec<_>>(),
        vec!["temperature", "load"]
    );
}

#[tokio::test]
async fn memory_history_query_distinguishes_missing_features_from_schema_mismatches() {
    let source = MemoryHistoryQuery::new();
    source
        .replace(binding(), history_data())
        .expect("fixture is accepted");

    let missing = source
        .query(
            HistoryWindow::new(
                task(),
                binding(),
                vec![numeric_feature("humidity", FeatureRole::History, "%")],
                TimestampMs::new(1_000),
                TimestampMs::new(4_000),
                3,
                HistoryAggregation::Mean,
                HistoryDuplicatePolicy::Latest,
            )
            .expect("window is valid"),
        )
        .await
        .expect_err("an uncommissioned feature is unavailable");
    assert_eq!(missing.kind(), PortErrorKind::Unavailable);

    let mismatched = source
        .query(
            HistoryWindow::new(
                task(),
                binding(),
                vec![numeric_feature("load", FeatureRole::History, "MW")],
                TimestampMs::new(1_000),
                TimestampMs::new(4_000),
                3,
                HistoryAggregation::Mean,
                HistoryDuplicatePolicy::Latest,
            )
            .expect("window is valid"),
        )
        .await
        .expect_err("matching names do not hide a unit mismatch");
    assert_eq!(mismatched.kind(), PortErrorKind::InvalidData);
}

#[tokio::test]
async fn memory_history_query_conforms_for_bounded_projection_and_provenance() {
    let source = MemoryHistoryQuery::new();
    source
        .replace(binding(), history_data())
        .expect("fixture is accepted");
    let expected = source
        .query(
            HistoryWindow::new(
                task(),
                binding(),
                vec![numeric_feature("load", FeatureRole::History, "kW")],
                TimestampMs::new(2_000),
                TimestampMs::new(4_000),
                2,
                HistoryAggregation::Mean,
                HistoryDuplicatePolicy::Latest,
            )
            .expect("window is valid"),
        )
        .await
        .expect("expected projection is available");
    let window = HistoryWindow::new(
        task(),
        binding(),
        vec![numeric_feature("load", FeatureRole::History, "kW")],
        TimestampMs::new(2_000),
        TimestampMs::new(4_000),
        2,
        HistoryAggregation::Mean,
        HistoryDuplicatePolicy::Latest,
    )
    .expect("window is valid");

    assert_history_query_bounded(&source, window.clone(), expected.clone())
        .await
        .expect("memory history respects hard bounds");
    assert_history_query_provenance(&source, window, expected.provenance())
        .await
        .expect("memory history preserves exact provenance");
}

#[tokio::test]
async fn memory_history_query_clamps_provenance_to_the_selected_history_cut() {
    let source = MemoryHistoryQuery::new();
    source
        .replace(binding(), history_data())
        .expect("fixture is accepted");

    let result = source
        .query(
            HistoryWindow::new(
                task(),
                binding(),
                vec![numeric_feature("load", FeatureRole::History, "kW")],
                TimestampMs::new(1_000),
                TimestampMs::new(2_500),
                2,
                HistoryAggregation::Mean,
                HistoryDuplicatePolicy::Latest,
            )
            .expect("window is valid"),
        )
        .await
        .expect("history cut is available");

    assert_eq!(result.provenance()[0].watermark(), TimestampMs::new(2_000));
}

#[tokio::test]
async fn memory_covariate_source_preserves_issue_provenance_and_bounds() {
    let source = MemoryCovariateSource::new();
    source
        .replace(binding(), future_data())
        .expect("fixture is accepted");

    let result = source
        .resolve(
            CovariateWindow::new(
                binding(),
                vec![numeric_feature(
                    "temperature",
                    FeatureRole::FutureCovariate,
                    "Cel",
                )],
                TimestampMs::new(3_000),
                TimestampMs::new(4_000),
                TimestampMs::new(6_000),
                2,
            )
            .expect("window is valid"),
        )
        .await
        .expect("future data is available");

    assert_eq!(result.segment().sample_count(), 2);
    assert_eq!(
        result.provenance()[0].issued_at(),
        Some(TimestampMs::new(2_500))
    );
}

#[tokio::test]
async fn memory_covariate_source_enforces_sample_limit_and_as_of_provenance_cut() {
    let source = MemoryCovariateSource::new();
    source
        .replace(binding(), future_data())
        .expect("fixture is accepted");

    let over_limit = source
        .resolve(
            CovariateWindow::new(
                binding(),
                vec![numeric_feature(
                    "temperature",
                    FeatureRole::FutureCovariate,
                    "Cel",
                )],
                TimestampMs::new(3_000),
                TimestampMs::new(4_000),
                TimestampMs::new(6_000),
                1,
            )
            .expect("window is valid"),
        )
        .await
        .expect_err("covariates must not be silently truncated");
    assert_eq!(over_limit.kind(), PortErrorKind::Rejected);

    let after_cutoff = source
        .resolve(
            CovariateWindow::new(
                binding(),
                vec![numeric_feature(
                    "temperature",
                    FeatureRole::FutureCovariate,
                    "Cel",
                )],
                TimestampMs::new(2_500),
                TimestampMs::new(4_000),
                TimestampMs::new(6_000),
                2,
            )
            .expect("window is valid"),
        )
        .await
        .expect_err("source watermark after as-of would leak future information");
    assert_eq!(after_cutoff.kind(), PortErrorKind::InvalidData);
}

#[tokio::test]
async fn memory_covariate_source_reports_unknown_binding_as_permanent() {
    let source = MemoryCovariateSource::new();
    let error = source
        .resolve(
            CovariateWindow::new(
                binding(),
                vec![numeric_feature(
                    "temperature",
                    FeatureRole::FutureCovariate,
                    "Cel",
                )],
                TimestampMs::new(3_000),
                TimestampMs::new(4_000),
                TimestampMs::new(6_000),
                2,
            )
            .expect("window is valid"),
        )
        .await
        .expect_err("unknown binding is a permanent commissioning error");

    assert_eq!(error.kind(), PortErrorKind::Permanent);
    assert_eq!(error.message(), "covariate binding not found");
}

#[test]
fn memory_query_ports_are_object_safe() {
    fn accepts_history(_: &dyn HistoryQuery) {}
    fn accepts_covariates(_: &dyn CovariateSource) {}

    accepts_history(&MemoryHistoryQuery::new());
    accepts_covariates(&MemoryCovariateSource::new());
}
