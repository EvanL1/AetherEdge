use aether_domain::{
    ArtifactProvenance, ArtifactSelector, BindingIdentity, DataProcessingRequest,
    DataProcessingTask, DerivedData, DomainError, FallbackInfo, FallbackPolicy, FeatureDefinition,
    FeatureRole, FeatureValue, ForecastOptions, ForecastOutput, ForecastPoint, ForecastQuantile,
    ForecastTarget, ForecastTaskSpec, FrameQuality, HistoryAggregation, HistoryDuplicatePolicy,
    HistoryFeaturePolicy, ProcessTaskRequest, ProcessingFrame, ProcessingOptions, ProcessingOutput,
    ProcessingResult, ProcessingStatus, ProcessorProvenance, SampleQuality, Segment, SegmentKind,
    SourceKind, SourceProvenance, TaskIdentity, TaskKind, TimestampMs, UnavailableInfo,
};

fn task_identity() -> TaskIdentity {
    TaskIdentity::new("energy.site-load-forecast", 1).expect("task identity is valid")
}

#[test]
fn per_feature_history_policies_must_exactly_cover_the_task_history_schema() {
    let ambient = FeatureDefinition::numeric("ambient", FeatureRole::History, "Cel")
        .expect("ambient feature is valid");
    let policies = vec![
        HistoryFeaturePolicy::new(
            "load",
            HistoryAggregation::Mean,
            HistoryDuplicatePolicy::Latest,
        )
        .expect("load policy is valid"),
        HistoryFeaturePolicy::new(
            "ambient",
            HistoryAggregation::Last,
            HistoryDuplicatePolicy::Reject,
        )
        .expect("ambient policy is valid"),
    ];
    let specification = forecast_task_spec()
        .with_history_feature_policies(policies)
        .expect("complete policies are valid");
    assert_eq!(
        specification.history_aggregation_for("ambient"),
        HistoryAggregation::Last
    );
    assert_eq!(
        specification.history_duplicate_policy_for("ambient"),
        HistoryDuplicatePolicy::Reject
    );
    DataProcessingTask::forecast(
        task_identity(),
        "aether.data-processing.forecast",
        vec![load_definition(), ambient.clone()],
        specification,
    )
    .expect("complete feature policies commission the task");

    let incomplete = forecast_task_spec()
        .with_history_feature_policies(vec![
            HistoryFeaturePolicy::new(
                "load",
                HistoryAggregation::Mean,
                HistoryDuplicatePolicy::Latest,
            )
            .expect("load policy is valid"),
        ])
        .expect("the specification is structurally representable");
    assert_eq!(
        DataProcessingTask::forecast(
            task_identity(),
            "aether.data-processing.forecast",
            vec![load_definition(), ambient],
            incomplete,
        ),
        Err(DomainError::InvalidProcessingState)
    );

    let wrong_persistence_source = ForecastTaskSpec::new(
        ForecastTarget::new("load", "kW", "positive_consumption").expect("target is valid"),
        1_000,
        HistoryAggregation::Mean,
        HistoryDuplicatePolicy::Latest,
        2,
        16,
        3_600_000,
        0.0,
        vec!["persistence".into()],
    )
    .expect("base specification is valid")
    .with_fallback_policies(vec![
        FallbackPolicy::new("persistence", "1", "ambient", 1_000)
            .expect("fallback policy is structurally valid"),
    ])
    .expect("fallback policy set is complete");
    assert_eq!(
        DataProcessingTask::forecast(
            task_identity(),
            "aether.data-processing.forecast",
            vec![
                load_definition(),
                FeatureDefinition::numeric("ambient", FeatureRole::History, "Cel")
                    .expect("ambient is valid")
            ],
            wrong_persistence_source,
        ),
        Err(DomainError::InvalidProcessingState)
    );
}

fn binding_identity() -> BindingIdentity {
    BindingIdentity::new("station-01", 7).expect("binding identity is valid")
}

fn load_definition() -> FeatureDefinition {
    FeatureDefinition::numeric("load", FeatureRole::History, "kW").expect("load feature is valid")
}

fn forecast_task_spec() -> ForecastTaskSpec {
    ForecastTaskSpec::new(
        ForecastTarget::new("load", "kW", "positive_consumption")
            .expect("forecast target is valid"),
        1_000,
        HistoryAggregation::Mean,
        HistoryDuplicatePolicy::Latest,
        2,
        16,
        3_600_000,
        0.01,
        vec!["persistence".into()],
    )
    .expect("forecast task spec is valid")
    .with_input_quality_limits(5_000, 2_000)
    .expect("input quality limits are valid")
    .with_fallback_policies(vec![
        FallbackPolicy::new("persistence", "1", "load", 1_800_000)
            .expect("fallback policy is valid"),
    ])
    .expect("fallback policies are valid")
    .requiring_future_issue_time()
}

fn history_segment() -> Segment {
    let series = SeriesBuilder::numeric(
        load_definition(),
        &[820.0, 835.0],
        &[SampleQuality::Good, SampleQuality::Good],
    );
    Segment::new(
        vec![TimestampMs::new(2_000), TimestampMs::new(3_000)],
        vec![series],
    )
    .expect("history segment is valid")
}

fn future_segment() -> Segment {
    let feature = FeatureDefinition::numeric("temp_avg", FeatureRole::FutureCovariate, "Cel")
        .expect("future feature is valid");
    let series = SeriesBuilder::numeric(
        feature,
        &[32.1, 32.0],
        &[SampleQuality::Good, SampleQuality::Good],
    );
    Segment::new(
        vec![TimestampMs::new(4_000), TimestampMs::new(5_000)],
        vec![series],
    )
    .expect("future segment is valid")
}

fn processing_frame() -> ProcessingFrame {
    let quality = FrameQuality::new(TimestampMs::new(3_000), 0.0, 1_000, true, 0)
        .expect("frame quality is valid");
    let provenance = vec![
        SourceProvenance::new(
            SegmentKind::History,
            "load",
            SourceKind::HistoryAndLive,
            Some("energy.site.load.active_power"),
            TimestampMs::new(3_000),
        )
        .expect("history provenance is valid"),
        SourceProvenance::new(
            SegmentKind::FutureCovariates,
            "temp_avg",
            SourceKind::Covariate,
            Some("weather.nwp.temperature"),
            TimestampMs::new(2_400),
        )
        .expect("future provenance is valid"),
    ];

    ProcessingFrame::new(
        TimestampMs::new(3_000),
        1_000,
        history_segment(),
        Some(future_segment()),
        vec![],
        quality,
        provenance,
    )
    .expect("processing frame is valid")
}

fn forecast_options() -> ProcessingOptions {
    ProcessingOptions::Forecast(
        ForecastOptions::new(2, vec![0.1, 0.5, 0.9]).expect("forecast options are valid"),
    )
}

fn forecast_output() -> ForecastOutput {
    let first = ForecastPoint::new(
        TimestampMs::new(4_000),
        846.2,
        vec![
            ForecastQuantile::new(0.1, 812.0).expect("quantile is valid"),
            ForecastQuantile::new(0.5, 846.2).expect("quantile is valid"),
            ForecastQuantile::new(0.9, 901.0).expect("quantile is valid"),
        ],
    )
    .expect("forecast point is valid");
    let second = ForecastPoint::new(TimestampMs::new(5_000), 852.7, vec![])
        .expect("forecast point is valid");

    ForecastOutput::new(
        "load",
        "kW",
        "positive_consumption",
        1_000,
        vec![first, second],
    )
    .expect("forecast output is valid")
}

fn produced_result() -> ProcessingResult {
    ProcessingResult::new(
        "request-01",
        task_identity(),
        binding_identity(),
        "sha256:input",
        ProcessingStatus::Produced,
        ProcessorProvenance::new(
            "load-forecasting-edge",
            "2.1.0",
            "aether.data-processing.forecast",
        )
        .expect("processor provenance is valid"),
        Some(
            ArtifactProvenance::new(
                "model",
                "site-load",
                "v3",
                "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            )
            .expect("artifact provenance is valid"),
        ),
        TimestampMs::new(3_000),
        TimestampMs::new(3_100),
        Some(TimestampMs::new(6_000)),
        Some(ProcessingOutput::Forecast(forecast_output())),
        None,
        None,
    )
    .expect("produced result is valid")
    .with_warnings(vec!["SHADOW_MODE".into()])
    .expect("warning is valid")
}

struct SeriesBuilder;

impl SeriesBuilder {
    fn numeric(
        definition: FeatureDefinition,
        values: &[f64],
        quality: &[SampleQuality],
    ) -> aether_domain::Series {
        let values = values
            .iter()
            .map(|value| FeatureValue::number(*value).expect("test value is finite"))
            .collect();
        aether_domain::Series::new(definition, values, quality.to_vec()).expect("series is valid")
    }
}

#[test]
fn identities_and_tasks_reject_empty_ids_zero_revisions_and_duplicate_features() {
    assert_eq!(TaskIdentity::new("", 1), Err(DomainError::EmptyIdentifier));
    assert_eq!(
        BindingIdentity::new("station-01", 0),
        Err(DomainError::ZeroRevision)
    );

    let feature = load_definition();
    let duplicate = DataProcessingTask::forecast(
        task_identity(),
        "aether.data-processing.forecast",
        vec![feature.clone(), feature],
        forecast_task_spec(),
    );
    assert_eq!(duplicate, Err(DomainError::DuplicateFeature));

    let task = DataProcessingTask::forecast(
        task_identity(),
        "aether.data-processing.forecast",
        vec![load_definition()],
        forecast_task_spec(),
    )
    .expect("task is valid");
    assert_eq!(task.identity(), &task_identity());
    assert_eq!(task.kind(), TaskKind::Forecast);
    assert_eq!(task.processor_contract(), "aether.data-processing.forecast");
    assert_eq!(task.features().len(), 1);
    assert_eq!(task.forecast_spec(), Some(&forecast_task_spec()));
    assert!(!task.remote_egress_allowed());
    assert!(
        task.clone()
            .allowing_remote_egress()
            .remote_egress_allowed()
    );
    assert_eq!(
        task.forecast_spec()
            .and_then(ForecastTaskSpec::max_input_age_ms),
        Some(5_000)
    );
    assert_eq!(
        task.forecast_spec().and_then(ForecastTaskSpec::max_gap_ms),
        Some(2_000)
    );
    let fallback = task
        .forecast_spec()
        .and_then(|spec| spec.fallback_policy("persistence"))
        .expect("persistence policy is present");
    assert_eq!(fallback.version(), "1");
    assert_eq!(fallback.max_output_age_ms(), 1_800_000);
    assert!(
        task.forecast_spec()
            .is_some_and(ForecastTaskSpec::requires_future_issue_time)
    );

    let future_load = FeatureDefinition::numeric("load", FeatureRole::FutureCovariate, "MW")
        .expect("future feature is valid");
    let future_target_leakage = DataProcessingTask::forecast(
        task_identity(),
        "aether.data-processing.forecast",
        vec![future_load, load_definition()],
        forecast_task_spec(),
    );
    assert_eq!(future_target_leakage, Err(DomainError::FeatureTypeMismatch));

    assert_eq!(
        ForecastTaskSpec::new(
            ForecastTarget::new("load", "kW", "positive_consumption").expect("target is valid"),
            1_000,
            HistoryAggregation::Mean,
            HistoryDuplicatePolicy::Latest,
            2,
            16,
            3_600_000,
            f64::NAN,
            vec![],
        ),
        Err(DomainError::InvalidFrameQuality)
    );
    assert_eq!(
        ForecastTaskSpec::new(
            ForecastTarget::new("load", "kW", "positive_consumption").expect("target is valid"),
            1_000,
            HistoryAggregation::Mean,
            HistoryDuplicatePolicy::Latest,
            2,
            16,
            3_600_000,
            0.0,
            vec![],
        )
        .expect("base spec is valid")
        .with_input_quality_limits(5_000, 999),
        Err(DomainError::InvalidFrameQuality)
    );
    assert_eq!(
        FallbackPolicy::new("persistence", "1", "load", 0),
        Err(DomainError::InvalidProcessingWindow)
    );
    assert_eq!(
        ForecastTaskSpec::new(
            ForecastTarget::new("load", "kW", "positive_consumption").expect("target is valid"),
            1_000,
            HistoryAggregation::Mean,
            HistoryDuplicatePolicy::Latest,
            2,
            16,
            3_600_000,
            0.0,
            vec!["persistence".into()],
        )
        .expect("base spec is valid")
        .with_fallback_policies(vec![
            FallbackPolicy::new("zero-fill", "1", "load", 1_000)
                .expect("policy is structurally valid"),
        ]),
        Err(DomainError::InvalidProcessingState)
    );
}

#[test]
fn feature_values_and_series_reject_non_finite_values_type_mismatches_and_bad_lengths() {
    assert_eq!(
        FeatureValue::number(f64::NAN),
        Err(DomainError::NonFiniteProcessingValue)
    );

    let type_mismatch = aether_domain::Series::new(
        load_definition(),
        vec![FeatureValue::text("not-a-number")],
        vec![SampleQuality::Good],
    );
    assert_eq!(type_mismatch, Err(DomainError::FeatureTypeMismatch));

    let length_mismatch = aether_domain::Series::new(
        load_definition(),
        vec![FeatureValue::number(1.0).expect("finite")],
        vec![],
    );
    assert_eq!(length_mismatch, Err(DomainError::ArrayLengthMismatch));

    let missing_without_missing_quality = aether_domain::Series::new(
        load_definition(),
        vec![FeatureValue::missing()],
        vec![SampleQuality::Good],
    );
    assert_eq!(
        missing_without_missing_quality,
        Err(DomainError::InvalidSampleQuality)
    );
}

#[test]
fn segments_reject_unordered_timestamps_and_series_length_mismatches() {
    let series = SeriesBuilder::numeric(
        load_definition(),
        &[1.0, 2.0],
        &[SampleQuality::Good, SampleQuality::Good],
    );
    let unordered = Segment::new(
        vec![TimestampMs::new(2_000), TimestampMs::new(1_000)],
        vec![series],
    );
    assert_eq!(unordered, Err(DomainError::TimestampsNotStrictlyIncreasing));

    let short_series = SeriesBuilder::numeric(load_definition(), &[1.0], &[SampleQuality::Good]);
    let mismatched = Segment::new(
        vec![TimestampMs::new(1_000), TimestampMs::new(2_000)],
        vec![short_series],
    );
    assert_eq!(mismatched, Err(DomainError::ArrayLengthMismatch));
}

#[test]
fn provenance_source_kinds_are_closed_by_segment_and_issue_time_semantics() {
    assert_eq!(
        SourceProvenance::new(
            SegmentKind::History,
            "load",
            SourceKind::Constant,
            None,
            TimestampMs::new(1_000),
        ),
        Err(DomainError::InvalidProcessingState)
    );
    assert_eq!(
        SourceProvenance::new(
            SegmentKind::FutureCovariates,
            "weather",
            SourceKind::History,
            None,
            TimestampMs::new(1_000),
        ),
        Err(DomainError::InvalidProcessingState)
    );
    assert_eq!(
        SourceProvenance::new(
            SegmentKind::StaticFeatures,
            "site_class",
            SourceKind::Covariate,
            None,
            TimestampMs::new(1_000),
        ),
        Err(DomainError::InvalidProcessingState)
    );
    let history = SourceProvenance::new(
        SegmentKind::History,
        "load",
        SourceKind::History,
        None,
        TimestampMs::new(1_000),
    )
    .expect("history provenance is valid");
    assert_eq!(
        history.with_issued_at(TimestampMs::new(900)),
        Err(DomainError::InvalidProcessingWindow)
    );

    for physical_or_secret_bearing_reference in [
        "https://weather.example/forecast",
        "/var/lib/aether/history.db",
        "postgres://history",
        "instance:1:telemetry:7",
        "select * from samples",
    ] {
        assert_eq!(
            SourceProvenance::new(
                SegmentKind::History,
                "load",
                SourceKind::History,
                Some(physical_or_secret_bearing_reference),
                TimestampMs::new(1_000),
            ),
            Err(DomainError::InvalidProcessingState)
        );
    }
}

#[test]
fn frames_reject_invalid_quality_and_history_or_future_on_the_wrong_side_of_as_of() {
    assert_eq!(
        FrameQuality::new(TimestampMs::new(2_500), f64::INFINITY, 1_000, false, 0),
        Err(DomainError::InvalidFrameQuality)
    );

    let quality = FrameQuality::new(TimestampMs::new(2_500), 0.0, 1_000, true, 0)
        .expect("frame quality is valid");
    let history_after_cutoff = ProcessingFrame::new(
        TimestampMs::new(1_500),
        1_000,
        history_segment(),
        None,
        vec![],
        quality.clone(),
        vec![],
    );
    assert_eq!(
        history_after_cutoff,
        Err(DomainError::InvalidProcessingWindow)
    );

    let future_at_cutoff = Segment::new(
        vec![TimestampMs::new(3_000)],
        vec![SeriesBuilder::numeric(
            FeatureDefinition::numeric("temp_avg", FeatureRole::FutureCovariate, "Cel")
                .expect("feature is valid"),
            &[31.0],
            &[SampleQuality::Good],
        )],
    )
    .expect("segment is structurally valid");
    let invalid_future = ProcessingFrame::new(
        TimestampMs::new(3_000),
        1_000,
        history_segment(),
        Some(future_at_cutoff),
        vec![],
        quality,
        vec![],
    );
    assert_eq!(invalid_future, Err(DomainError::InvalidProcessingWindow));
}

#[test]
fn frames_require_exact_provenance_and_an_actual_input_watermark() {
    let quality = FrameQuality::new(TimestampMs::new(2_500), 0.0, 1_000, false, 0)
        .expect("frame quality is valid");
    let history_source = SourceProvenance::new(
        SegmentKind::History,
        "load",
        SourceKind::History,
        Some("site.load"),
        TimestampMs::new(2_500),
    )
    .expect("history provenance is valid");
    let future_source = SourceProvenance::new(
        SegmentKind::FutureCovariates,
        "temp_avg",
        SourceKind::Covariate,
        Some("weather.temperature"),
        TimestampMs::new(2_400),
    )
    .expect("future provenance is valid");

    for provenance in [
        vec![history_source.clone()],
        vec![history_source.clone(), history_source.clone()],
        vec![
            history_source.clone(),
            SourceProvenance::new(
                SegmentKind::FutureCovariates,
                "unknown",
                SourceKind::Covariate,
                None,
                TimestampMs::new(2_400),
            )
            .expect("unknown provenance is structurally valid"),
        ],
    ] {
        assert_eq!(
            ProcessingFrame::new(
                TimestampMs::new(3_000),
                1_000,
                history_segment(),
                Some(future_segment()),
                vec![],
                quality.clone(),
                provenance,
            ),
            Err(DomainError::InvalidProcessingState)
        );
    }

    let wrong_watermark = FrameQuality::new(TimestampMs::new(2_400), 0.0, 1_000, false, 0)
        .expect("watermark is structurally valid");
    assert_eq!(
        ProcessingFrame::new(
            TimestampMs::new(3_000),
            1_000,
            history_segment(),
            Some(future_segment()),
            vec![],
            wrong_watermark,
            vec![history_source.clone(), future_source.clone()],
        ),
        Err(DomainError::InvalidFrameQuality)
    );

    ProcessingFrame::new(
        TimestampMs::new(3_000),
        1_000,
        history_segment(),
        Some(future_segment()),
        vec![],
        quality,
        vec![history_source, future_source],
    )
    .expect("one provenance record per feature is valid");
}

#[test]
fn application_and_processor_requests_keep_their_boundaries_explicit() {
    let application_request = ProcessTaskRequest::new(
        task_identity(),
        binding_identity(),
        TimestampMs::new(3_000),
        forecast_options(),
    );
    assert_eq!(application_request.task(), &task_identity());
    assert_eq!(application_request.binding(), &binding_identity());
    assert_eq!(application_request.as_of(), TimestampMs::new(3_000));
    assert!(matches!(
        application_request.options(),
        ProcessingOptions::Forecast(_)
    ));

    let processor_request = DataProcessingRequest::new(
        "request-01",
        task_identity(),
        binding_identity(),
        processing_frame(),
        TimestampMs::new(3_100),
        TimestampMs::new(10_000),
        "aether.data-processing.forecast",
        Some(
            ArtifactSelector::new("model", "site-load", Some("v3"))
                .expect("artifact selector is valid")
                .with_digest(
                    "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                )
                .expect("artifact digest is valid"),
        ),
        "sha256:input",
        forecast_options(),
    )
    .expect("processor request is complete");
    assert_eq!(processor_request.request_id(), "request-01");
    assert_eq!(processor_request.frame().as_of(), TimestampMs::new(3_000));
    assert_eq!(processor_request.submitted_at(), TimestampMs::new(3_100));
    assert_eq!(processor_request.deadline(), TimestampMs::new(10_000));
    assert_eq!(
        processor_request.processor_contract(),
        "aether.data-processing.forecast"
    );
    assert_eq!(
        processor_request
            .artifact_selector()
            .and_then(ArtifactSelector::digest),
        Some("sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
    );
    assert_eq!(
        processor_request
            .artifact_selector()
            .expect("selector was supplied")
            .family(),
        "site-load"
    );
    assert_eq!(processor_request.input_digest(), "sha256:input");
    assert!(processor_request.frame().static_features().is_empty());

    assert_eq!(
        DataProcessingRequest::new(
            "request-01",
            task_identity(),
            binding_identity(),
            processing_frame(),
            TimestampMs::new(3_100),
            TimestampMs::new(3_100),
            "aether.data-processing.forecast",
            None,
            "sha256:input",
            forecast_options(),
        ),
        Err(DomainError::InvalidProcessingWindow)
    );
}

#[test]
fn forecast_outputs_reject_non_finite_values_unordered_points_and_quantiles() {
    assert_eq!(
        ForecastPoint::new(TimestampMs::new(4_000), f64::NAN, vec![]),
        Err(DomainError::NonFiniteProcessingValue)
    );
    assert_eq!(
        ForecastQuantile::new(1.0, 10.0),
        Err(DomainError::InvalidQuantile)
    );

    let later = ForecastPoint::new(TimestampMs::new(5_000), 2.0, vec![]).expect("point is valid");
    let earlier = ForecastPoint::new(TimestampMs::new(4_000), 1.0, vec![]).expect("point is valid");
    let unordered = ForecastOutput::new(
        "load",
        "kW",
        "positive_consumption",
        1_000,
        vec![later, earlier],
    );
    assert_eq!(unordered, Err(DomainError::TimestampsNotStrictlyIncreasing));
}

#[test]
fn processing_results_reject_illegal_status_combinations() {
    let unavailable_with_output = ProcessingResult::new(
        "request-01",
        task_identity(),
        binding_identity(),
        "sha256:input",
        ProcessingStatus::Unavailable,
        ProcessorProvenance::new(
            "load-forecasting-edge",
            "2.1.0",
            "aether.data-processing.forecast",
        )
        .expect("processor provenance is valid"),
        None,
        TimestampMs::new(2_500),
        TimestampMs::new(3_100),
        None,
        Some(ProcessingOutput::Forecast(forecast_output())),
        None,
        Some(
            UnavailableInfo::new("INSUFFICIENT_HISTORY", true, Some(900_000))
                .expect("reason is valid"),
        ),
    );
    assert_eq!(
        unavailable_with_output,
        Err(DomainError::InvalidProcessingState)
    );

    let fallback_without_metadata = ProcessingResult::new(
        "request-01",
        task_identity(),
        binding_identity(),
        "sha256:input",
        ProcessingStatus::Fallback,
        ProcessorProvenance::new(
            "load-forecasting-edge",
            "2.1.0",
            "aether.data-processing.forecast",
        )
        .expect("processor provenance is valid"),
        None,
        TimestampMs::new(2_500),
        TimestampMs::new(3_100),
        Some(TimestampMs::new(6_000)),
        Some(ProcessingOutput::Forecast(forecast_output())),
        None,
        None,
    );
    assert_eq!(
        fallback_without_metadata,
        Err(DomainError::InvalidProcessingState)
    );

    let fallback = ProcessingResult::new(
        "request-01",
        task_identity(),
        binding_identity(),
        "sha256:input",
        ProcessingStatus::Fallback,
        ProcessorProvenance::new(
            "load-forecasting-edge",
            "2.1.0",
            "aether.data-processing.forecast",
        )
        .expect("processor provenance is valid"),
        None,
        TimestampMs::new(2_500),
        TimestampMs::new(3_100),
        Some(TimestampMs::new(6_000)),
        Some(ProcessingOutput::Forecast(forecast_output())),
        Some(
            FallbackInfo::new(
                "persistence",
                "1",
                "MODEL_UNAVAILABLE",
                "load",
                TimestampMs::new(2_500),
            )
            .expect("fallback is valid"),
        ),
        None,
    )
    .expect("fallback combination is valid");
    assert_eq!(fallback.status(), ProcessingStatus::Fallback);
    assert!(fallback.fallback().is_some());
}

#[test]
fn derived_data_accepts_only_usable_unexpired_results() {
    let accepted = DerivedData::accept(
        "result-01",
        TimestampMs::new(3_200),
        processing_frame().quality().clone(),
        produced_result(),
    )
    .expect("produced output may be accepted");
    assert_eq!(accepted.result_id(), "result-01");
    assert_eq!(accepted.accepted_at(), TimestampMs::new(3_200));
    assert_eq!(accepted.result().status(), ProcessingStatus::Produced);
    assert_eq!(accepted.result().input_watermark(), TimestampMs::new(3_000));
    assert_eq!(accepted.result().warnings(), &["SHADOW_MODE"]);
    assert_eq!(accepted.frame_quality().missing_ratio(), 0.0);
    assert_eq!(accepted.frame_quality().max_gap_ms(), 1_000);

    let unavailable = ProcessingResult::new(
        "request-02",
        task_identity(),
        binding_identity(),
        "sha256:input-2",
        ProcessingStatus::Unavailable,
        ProcessorProvenance::new(
            "load-forecasting-edge",
            "2.1.0",
            "aether.data-processing.forecast",
        )
        .expect("processor provenance is valid"),
        None,
        TimestampMs::new(2_500),
        TimestampMs::new(3_100),
        None,
        None,
        None,
        Some(UnavailableInfo::new("NO_MODEL", false, None).expect("reason is valid")),
    )
    .expect("unavailable result is a valid untrusted response");
    assert_eq!(
        DerivedData::accept(
            "result-02",
            TimestampMs::new(3_200),
            processing_frame().quality().clone(),
            unavailable,
        ),
        Err(DomainError::InvalidProcessingState)
    );
}
