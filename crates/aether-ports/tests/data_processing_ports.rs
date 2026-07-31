use std::sync::Arc;

use aether_domain::{
    BindingIdentity, DataProcessingRequest, FeatureDefinition, FeatureRole, FeatureValue,
    HistoryAggregation, HistoryDuplicatePolicy, HistoryFeaturePolicy, ProcessingResult,
    SampleQuality, Segment, SegmentKind, Series, SourceKind, SourceProvenance, TaskIdentity,
    TaskKind, TimestampMs,
};
use aether_ports::{
    CovariateSource, CovariateWindow, DataBoundary, DataProcessor, DataProcessorDescriptor,
    HistoryQuery, HistoryWindow, PortErrorKind, PortResult, ProcessorHealth, SourcedSegment,
};
use async_trait::async_trait;

fn binding() -> BindingIdentity {
    BindingIdentity::new("station-01", 7).expect("binding is valid")
}

fn task() -> TaskIdentity {
    TaskIdentity::new("iot.history-test", 1).expect("task is valid")
}

fn history_feature() -> FeatureDefinition {
    FeatureDefinition::numeric("load", FeatureRole::History, "kW").expect("feature is valid")
}

fn future_feature() -> FeatureDefinition {
    FeatureDefinition::numeric("temp_avg", FeatureRole::FutureCovariate, "Cel")
        .expect("feature is valid")
}

#[test]
fn history_queries_require_a_bounded_logical_window() {
    let window = HistoryWindow::new(
        task(),
        binding(),
        vec![history_feature()],
        TimestampMs::new(1_000),
        TimestampMs::new(2_000),
        128,
        HistoryAggregation::Mean,
        HistoryDuplicatePolicy::Latest,
    )
    .expect("window is bounded");

    assert_eq!(window.binding(), &binding());
    assert_eq!(window.features(), &[history_feature()]);
    assert_eq!(window.start(), TimestampMs::new(1_000));
    assert_eq!(window.end(), TimestampMs::new(2_000));
    assert_eq!(window.cutoff(), TimestampMs::new(2_000));
    assert_eq!(window.max_samples(), 128);

    let tailored = window
        .clone()
        .with_feature_policies(vec![
            HistoryFeaturePolicy::new(
                "load",
                HistoryAggregation::Sum,
                HistoryDuplicatePolicy::Reject,
            )
            .expect("feature policy is valid"),
        ])
        .expect("exact policy coverage is accepted");
    let policy = tailored.policy("load").expect("load policy is present");
    assert_eq!(policy.aggregation(), HistoryAggregation::Sum);
    assert_eq!(policy.duplicate_policy(), HistoryDuplicatePolicy::Reject);
    assert!(window.clone().with_feature_policies(vec![]).is_err());
    assert!(
        window
            .clone()
            .with_feature_policies(vec![
                HistoryFeaturePolicy::new(
                    "ambient",
                    HistoryAggregation::Mean,
                    HistoryDuplicatePolicy::Latest,
                )
                .expect("unrequested policy is structurally valid"),
            ])
            .is_err()
    );

    let cut_off = window
        .clone()
        .with_cutoff(TimestampMs::new(1_500))
        .expect("a read cutoff inside the logical window is valid");
    assert_eq!(cut_off.cutoff(), TimestampMs::new(1_500));

    let future_cutoff = window
        .with_cutoff(TimestampMs::new(2_001))
        .expect_err("history cannot be read beyond its logical window");
    assert_eq!(future_cutoff.kind(), PortErrorKind::InvalidData);

    let unbounded = HistoryWindow::new(
        task(),
        binding(),
        vec![history_feature()],
        TimestampMs::new(1_000),
        TimestampMs::new(2_000),
        0,
        HistoryAggregation::Mean,
        HistoryDuplicatePolicy::Latest,
    )
    .expect_err("zero samples would make the query unbounded or useless");
    assert_eq!(unbounded.kind(), PortErrorKind::InvalidData);

    let reversed = HistoryWindow::new(
        task(),
        binding(),
        vec![history_feature()],
        TimestampMs::new(2_000),
        TimestampMs::new(1_000),
        128,
        HistoryAggregation::Mean,
        HistoryDuplicatePolicy::Latest,
    )
    .expect_err("history range must be half-open and increasing");
    assert_eq!(reversed.kind(), PortErrorKind::InvalidData);

    let no_logical_features = HistoryWindow::new(
        task(),
        binding(),
        vec![],
        TimestampMs::new(1_000),
        TimestampMs::new(2_000),
        128,
        HistoryAggregation::Mean,
        HistoryDuplicatePolicy::Latest,
    )
    .expect_err("a logical query names at least one feature");
    assert_eq!(no_logical_features.kind(), PortErrorKind::InvalidData);
}

#[test]
fn covariate_queries_are_bounded_by_cutoff_window_and_sample_count() {
    let window = CovariateWindow::new(
        binding(),
        vec![future_feature()],
        TimestampMs::new(3_000),
        TimestampMs::new(4_000),
        TimestampMs::new(6_000),
        16,
    )
    .expect("covariate request is bounded");

    assert_eq!(window.as_of(), TimestampMs::new(3_000));
    assert_eq!(window.start(), TimestampMs::new(4_000));
    assert_eq!(window.end(), TimestampMs::new(6_000));
    assert_eq!(window.max_samples(), 16);

    let invalid = CovariateWindow::new(
        binding(),
        vec![future_feature()],
        TimestampMs::new(3_000),
        TimestampMs::new(2_000),
        TimestampMs::new(6_000),
        16,
    )
    .expect_err("future covariates cannot begin before the observation cutoff");
    assert_eq!(invalid.kind(), PortErrorKind::InvalidData);
}

#[test]
fn sourced_segments_require_exact_role_aware_provenance() {
    let segment = Segment::new(
        vec![TimestampMs::new(1_000)],
        vec![
            Series::new(
                history_feature(),
                vec![FeatureValue::number(10.0).expect("value is finite")],
                vec![SampleQuality::Good],
            )
            .expect("series is valid"),
        ],
    )
    .expect("segment is valid");
    let source = SourceProvenance::new(
        SegmentKind::History,
        "load",
        SourceKind::History,
        Some("site.load"),
        TimestampMs::new(1_000),
    )
    .expect("provenance is valid");

    SourcedSegment::new(segment.clone(), vec![source.clone()])
        .expect("one matching provenance entry is accepted");

    for invalid in [
        Vec::new(),
        vec![
            source,
            SourceProvenance::new(
                SegmentKind::History,
                "load",
                SourceKind::History,
                Some("another.logical.source"),
                TimestampMs::new(900),
            )
            .expect("second provenance value is structurally valid"),
        ],
        vec![
            SourceProvenance::new(
                SegmentKind::FutureCovariates,
                "load",
                SourceKind::Covariate,
                None,
                TimestampMs::new(1_000),
            )
            .expect("wrong-segment provenance is structurally valid"),
        ],
        vec![
            SourceProvenance::new(
                SegmentKind::History,
                "unknown",
                SourceKind::History,
                None,
                TimestampMs::new(1_000),
            )
            .expect("unknown-feature provenance is structurally valid"),
        ],
    ] {
        assert_eq!(
            SourcedSegment::new(segment.clone(), invalid)
                .expect_err("provenance must map one-to-one by segment and feature")
                .kind(),
            PortErrorKind::InvalidData
        );
    }
}

#[derive(Debug)]
struct FakeHistory;

#[async_trait]
impl HistoryQuery for FakeHistory {
    async fn query(&self, _window: HistoryWindow) -> PortResult<SourcedSegment> {
        Err(aether_ports::PortError::new(
            PortErrorKind::Unavailable,
            "test adapter has no history",
        ))
    }
}

#[derive(Debug)]
struct FakeCovariates;

#[async_trait]
impl CovariateSource for FakeCovariates {
    async fn resolve(&self, _window: CovariateWindow) -> PortResult<SourcedSegment> {
        Err(aether_ports::PortError::new(
            PortErrorKind::Unavailable,
            "test adapter has no covariates",
        ))
    }
}

#[derive(Debug)]
struct FakeProcessor {
    descriptor: DataProcessorDescriptor,
}

#[async_trait]
impl DataProcessor for FakeProcessor {
    fn descriptor(&self) -> &DataProcessorDescriptor {
        &self.descriptor
    }

    async fn health(&self) -> PortResult<ProcessorHealth> {
        Ok(ProcessorHealth::Healthy)
    }

    async fn process(&self, _request: DataProcessingRequest) -> PortResult<ProcessingResult> {
        Err(aether_ports::PortError::new(
            PortErrorKind::Unavailable,
            "test processor has no model",
        ))
    }
}

#[test]
fn data_processing_ports_are_object_safe_and_processors_are_discoverable() {
    fn accepts_history(_: Option<Arc<dyn HistoryQuery>>) {}
    fn accepts_covariates(_: Option<Arc<dyn CovariateSource>>) {}
    fn accepts_processor(_: Option<Arc<dyn DataProcessor>>) {}

    accepts_history(Some(Arc::new(FakeHistory)));
    accepts_covariates(Some(Arc::new(FakeCovariates)));

    let descriptor = DataProcessorDescriptor::new(
        "load-forecasting-edge",
        "2.1.0",
        vec![TaskKind::Forecast],
        vec!["aether.data-processing.forecast.v1".into()],
        DataBoundary::Local,
        4_096,
        4_194_304,
    )
    .expect("descriptor is valid");
    let processor = Arc::new(FakeProcessor { descriptor });
    assert_eq!(processor.descriptor().id(), "load-forecasting-edge");
    assert_eq!(processor.descriptor().version(), "2.1.0");
    assert!(processor.descriptor().supports(TaskKind::Forecast));
    assert!(
        processor
            .descriptor()
            .supports_contract("aether.data-processing.forecast.v1")
    );
    assert_eq!(processor.descriptor().data_boundary(), DataBoundary::Local);
    assert_eq!(processor.descriptor().max_frame_samples(), 4_096);
    assert_eq!(processor.descriptor().max_request_bytes(), 4_194_304);
    accepts_processor(Some(processor));
}

#[test]
fn processor_descriptors_reject_empty_identity_capabilities_and_limits() {
    let empty_id = DataProcessorDescriptor::new(
        "",
        "2.1.0",
        vec![TaskKind::Forecast],
        vec!["aether.data-processing.forecast.v1".into()],
        DataBoundary::Local,
        10,
        1_024,
    )
    .expect_err("processor identity is required");
    assert_eq!(empty_id.kind(), PortErrorKind::InvalidData);

    let no_tasks = DataProcessorDescriptor::new(
        "processor",
        "2.1.0",
        vec![],
        vec!["aether.data-processing.forecast.v1".into()],
        DataBoundary::Local,
        10,
        1_024,
    )
    .expect_err("at least one typed task is required");
    assert_eq!(no_tasks.kind(), PortErrorKind::InvalidData);

    let no_limit = DataProcessorDescriptor::new(
        "processor",
        "2.1.0",
        vec![TaskKind::Forecast],
        vec!["aether.data-processing.forecast.v1".into()],
        DataBoundary::Remote,
        0,
        1_024,
    )
    .expect_err("a processor must advertise a finite frame bound");
    assert_eq!(no_limit.kind(), PortErrorKind::InvalidData);
}
