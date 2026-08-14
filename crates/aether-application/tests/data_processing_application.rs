use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};

use aether_application::{
    Actor, ApplicationError, AuditPolicy, DataProcessingApplication, DataProcessingBinding,
    DataProcessingRoute, PROCESS_DATA_CAPABILITY, PROCESSOR_HEALTH_CAPABILITY, PointFeatureBinding,
    RequestContext, SafetyPolicy, TASKS_LIST_CAPABILITY,
};
use aether_data_processing::compute_input_digest;
use aether_domain::{
    ArtifactProvenance, ArtifactSelector, BindingIdentity, DataProcessingRequest,
    DataProcessingTask, FallbackInfo, FallbackPolicy, FeatureDefinition, FeatureRole, FeatureValue,
    FeatureValueType, ForecastOptions, ForecastOutput, ForecastPoint, ForecastQuantile,
    ForecastTarget, ForecastTaskSpec, HistoryAggregation, HistoryDuplicatePolicy, InstanceId,
    PointAddress, PointId, PointKind, PointSample, ProcessTaskRequest, ProcessingOptions,
    ProcessingOutput, ProcessingResult, ProcessingStatus, ProcessorProvenance, SampleQuality,
    Segment, SegmentKind, Series, SourceKind, SourceProvenance, StaticFeature, TaskIdentity,
    TaskKind, TimestampMs,
};
use aether_ports::{
    AuditOutcome, AuditRecord, AuditSink, CovariateSource, CovariateWindow, DataBoundary,
    DataProcessor, DataProcessorDescriptor, HistoryQuery, HistoryWindow, LiveState, PortError,
    PortErrorKind, PortResult, ProcessorHealth, SourcedSegment,
};
use aether_store_local::ManualClock;
use async_trait::async_trait;
use tokio::sync::Notify;
use uuid::Uuid;

const CONTRACT: &str = "aether.data-processing.forecast";
const OUTER_REQUEST_ID: &str = "request-01";

fn task_identity() -> TaskIdentity {
    TaskIdentity::new("energy.site-load-forecast", 1).expect("task identity is valid")
}

fn binding_identity() -> BindingIdentity {
    BindingIdentity::new("site-a", 7).expect("binding identity is valid")
}

fn load_address() -> PointAddress {
    PointAddress::new(InstanceId::new(1), PointKind::Telemetry, PointId::new(1))
}

fn history_feature() -> FeatureDefinition {
    FeatureDefinition::numeric("load", FeatureRole::History, "kW").expect("feature is valid")
}

fn future_feature() -> FeatureDefinition {
    FeatureDefinition::numeric("temp_avg", FeatureRole::FutureCovariate, "Cel")
        .expect("feature is valid")
}

fn task() -> DataProcessingTask {
    DataProcessingTask::forecast(
        task_identity(),
        CONTRACT,
        vec![history_feature(), future_feature()],
        ForecastTaskSpec::new(
            ForecastTarget::new("load", "kW", "positive_consumption").expect("target is valid"),
            1_000,
            HistoryAggregation::Mean,
            HistoryDuplicatePolicy::Latest,
            2,
            4,
            10_000,
            0.0,
            vec!["persistence".into()],
        )
        .expect("forecast spec is valid")
        .with_input_quality_limits(1_500, 1_000)
        .expect("input quality limits are valid")
        .with_fallback_policies(vec![
            FallbackPolicy::new("persistence", "1", "load", 5_000)
                .expect("fallback policy is valid"),
        ])
        .expect("fallback policies are valid")
        .requiring_future_issue_time(),
    )
    .expect("task is valid")
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
    let segment = Segment::new(
        vec![TimestampMs::new(2_000), TimestampMs::new(3_000)],
        vec![numeric_series(history_feature(), &[10.0, 11.0])],
    )
    .expect("history is valid");
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
        ],
    )
    .expect("history is sourced")
}

fn future_data() -> SourcedSegment {
    let segment = Segment::new(
        vec![TimestampMs::new(4_000), TimestampMs::new(5_000)],
        vec![numeric_series(future_feature(), &[20.0, 21.0])],
    )
    .expect("future segment is valid");
    let provenance = SourceProvenance::new(
        SegmentKind::FutureCovariates,
        "temp_avg",
        SourceKind::Covariate,
        Some("weather.nwp.temperature"),
        TimestampMs::new(2_500),
    )
    .expect("provenance is valid")
    .with_issued_at(TimestampMs::new(2_400))
    .expect("issue time is valid");
    SourcedSegment::new(segment, vec![provenance]).expect("future segment is sourced")
}

fn process_request() -> ProcessTaskRequest {
    ProcessTaskRequest::new(
        task_identity(),
        binding_identity(),
        TimestampMs::new(3_000),
        ProcessingOptions::Forecast(ForecastOptions::new(2, vec![]).expect("options are valid")),
    )
}

fn commissioned_binding(live_tail: bool) -> DataProcessingBinding {
    let point = PointFeatureBinding::new("load", load_address()).expect("point binding is valid");
    let point = if live_tail {
        point.with_live_tail()
    } else {
        point
    };
    DataProcessingBinding::new(binding_identity(), vec![point])
        .expect("binding is commissioned")
        .with_artifact(
            ArtifactSelector::new("model", "site-load", Some("v3"))
                .expect("artifact selector is valid")
                .with_digest(
                    "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                )
                .expect("artifact digest is valid"),
        )
}

fn context(permission: &str, confirmed: bool) -> RequestContext {
    RequestContext::new(
        OUTER_REQUEST_ID,
        Actor::new("agent:test").with_permission(permission),
        confirmed,
        TimestampMs::new(3_100),
    )
}

#[derive(Default)]
struct RecordingHistory {
    windows: Mutex<Vec<HistoryWindow>>,
    fail: bool,
}

#[async_trait]
impl HistoryQuery for RecordingHistory {
    async fn query(&self, window: HistoryWindow) -> PortResult<SourcedSegment> {
        self.windows.lock().expect("test lock").push(window);
        if self.fail {
            return Err(PortError::new(
                PortErrorKind::Unavailable,
                "history unavailable",
            ));
        }
        Ok(history_data())
    }
}

struct DuplicateProvenanceHistory;

#[async_trait]
impl HistoryQuery for DuplicateProvenanceHistory {
    async fn query(&self, _window: HistoryWindow) -> PortResult<SourcedSegment> {
        let source = SourceProvenance::new(
            SegmentKind::History,
            "load",
            SourceKind::History,
            Some("site.load"),
            TimestampMs::new(3_000),
        )
        .expect("provenance is valid");
        SourcedSegment::new(
            history_data().segment().clone(),
            vec![source.clone(), source],
        )
    }
}

#[derive(Default)]
struct RecordingCovariates {
    windows: Mutex<Vec<CovariateWindow>>,
    fail: bool,
    omit_issue_time: bool,
}

#[async_trait]
impl CovariateSource for RecordingCovariates {
    async fn resolve(&self, window: CovariateWindow) -> PortResult<SourcedSegment> {
        self.windows.lock().expect("test lock").push(window);
        if self.fail {
            return Err(PortError::new(
                PortErrorKind::Unavailable,
                "covariates unavailable",
            ));
        }
        if self.omit_issue_time {
            let source = SourceProvenance::new(
                SegmentKind::FutureCovariates,
                "temp_avg",
                SourceKind::Covariate,
                Some("weather.nwp.temperature"),
                TimestampMs::new(2_500),
            )
            .expect("provenance is valid");
            return SourcedSegment::new(future_data().segment().clone(), vec![source]);
        }
        Ok(future_data())
    }
}

struct CalendarCovariates;

#[async_trait]
impl CovariateSource for CalendarCovariates {
    async fn resolve(&self, _window: CovariateWindow) -> PortResult<SourcedSegment> {
        SourcedSegment::new(
            future_data().segment().clone(),
            vec![
                SourceProvenance::new(
                    SegmentKind::FutureCovariates,
                    "temp_avg",
                    SourceKind::Calendar,
                    None,
                    TimestampMs::new(3_000),
                )
                .expect("calendar provenance is valid"),
            ],
        )
    }
}

#[derive(Default)]
struct EmptyLiveState {
    reads: Mutex<usize>,
}

#[async_trait]
impl LiveState for EmptyLiveState {
    async fn read(&self, _address: PointAddress) -> PortResult<Option<PointSample>> {
        *self.reads.lock().expect("test lock") += 1;
        Ok(None)
    }

    async fn read_many(&self, addresses: &[PointAddress]) -> PortResult<Vec<Option<PointSample>>> {
        *self.reads.lock().expect("test lock") += addresses.len();
        Ok(vec![None; addresses.len()])
    }
}

#[derive(Default)]
struct RecordingAudit {
    records: Mutex<Vec<AuditRecord>>,
    fail: bool,
}

#[derive(Default)]
struct BlockFirstAttemptAudit {
    blocked: AtomicBool,
}

#[async_trait]
impl AuditSink for BlockFirstAttemptAudit {
    async fn record(&self, record: AuditRecord) -> PortResult<()> {
        if record.outcome() == AuditOutcome::Attempted && !self.blocked.swap(true, Ordering::SeqCst)
        {
            std::future::pending::<()>().await;
        }
        Ok(())
    }
}

#[async_trait]
impl AuditSink for RecordingAudit {
    async fn record(&self, record: AuditRecord) -> PortResult<()> {
        if self.fail {
            return Err(PortError::new(
                PortErrorKind::Unavailable,
                "audit unavailable",
            ));
        }
        self.records.lock().expect("test lock").push(record);
        Ok(())
    }
}

#[derive(Debug, Clone, Copy)]
enum ProcessorBehavior {
    Produced,
    BadRequestId,
    BadTask,
    BadBinding,
    BadDigest,
    BadProcessorId,
    BadProcessorVersion,
    BadProcessorContract,
    BadTarget,
    BadUnit,
    BadSign,
    BadCadence,
    BadHorizon,
    BadTimestamps,
    BadQuantiles,
    ExpiryTooLong,
    BadArtifact,
    BadArtifactDigest,
    MissingArtifact,
    AllowedFallback,
    FallbackWrongValues,
    FallbackStaleSource,
    FallbackWrongVersion,
    FallbackExpiryTooLong,
    FallbackNotFromFeatureSource,
    UndeclaredFallback,
    Unavailable,
    ProcessFailure,
    HealthFailure,
}

struct RecordingProcessor {
    descriptor: DataProcessorDescriptor,
    behavior: ProcessorBehavior,
    requests: Mutex<Vec<DataProcessingRequest>>,
    health_calls: Mutex<usize>,
}

struct BlockingProcessor {
    inner: RecordingProcessor,
    entered: Arc<Notify>,
    release: Arc<Notify>,
}

struct DelayedProcessor {
    inner: RecordingProcessor,
    delay: std::time::Duration,
}

#[async_trait]
impl DataProcessor for BlockingProcessor {
    fn descriptor(&self) -> &DataProcessorDescriptor {
        self.inner.descriptor()
    }

    async fn health(&self) -> PortResult<ProcessorHealth> {
        self.inner.health().await
    }

    async fn process(&self, request: DataProcessingRequest) -> PortResult<ProcessingResult> {
        self.entered.notify_one();
        self.release.notified().await;
        self.inner.process(request).await
    }
}

#[async_trait]
impl DataProcessor for DelayedProcessor {
    fn descriptor(&self) -> &DataProcessorDescriptor {
        self.inner.descriptor()
    }

    async fn health(&self) -> PortResult<ProcessorHealth> {
        self.inner.health().await
    }

    async fn process(&self, request: DataProcessingRequest) -> PortResult<ProcessingResult> {
        tokio::time::sleep(self.delay).await;
        self.inner.process(request).await
    }
}

impl RecordingProcessor {
    fn new(boundary: DataBoundary, behavior: ProcessorBehavior) -> Self {
        Self::with_limits(boundary, behavior, 32, 1_048_576)
    }

    fn with_limits(
        boundary: DataBoundary,
        behavior: ProcessorBehavior,
        max_frame_samples: usize,
        max_request_bytes: usize,
    ) -> Self {
        Self {
            descriptor: DataProcessorDescriptor::new(
                "forecast-test",
                "1.0.0",
                vec![TaskKind::Forecast],
                vec![CONTRACT.into()],
                boundary,
                max_frame_samples,
                max_request_bytes,
            )
            .expect("descriptor is valid"),
            behavior,
            requests: Mutex::new(Vec::new()),
            health_calls: Mutex::new(0),
        }
    }
}

#[async_trait]
impl DataProcessor for RecordingProcessor {
    fn descriptor(&self) -> &DataProcessorDescriptor {
        &self.descriptor
    }

    async fn health(&self) -> PortResult<ProcessorHealth> {
        *self.health_calls.lock().expect("test lock") += 1;
        if matches!(self.behavior, ProcessorBehavior::HealthFailure) {
            return Err(PortError::new(
                PortErrorKind::Unavailable,
                "health probe unavailable",
            ));
        }
        Ok(ProcessorHealth::Healthy)
    }

    async fn process(&self, request: DataProcessingRequest) -> PortResult<ProcessingResult> {
        self.requests
            .lock()
            .expect("test lock")
            .push(request.clone());
        if matches!(self.behavior, ProcessorBehavior::ProcessFailure) {
            return Err(PortError::new(
                PortErrorKind::Unavailable,
                "processor unavailable",
            ));
        }
        let processor_contract = if matches!(self.behavior, ProcessorBehavior::BadProcessorContract)
        {
            "aether.data-processing.forecast.invalid"
        } else {
            CONTRACT
        };
        let processor = ProcessorProvenance::new(
            if matches!(self.behavior, ProcessorBehavior::BadProcessorId) {
                "different-processor"
            } else {
                "forecast-test"
            },
            if matches!(self.behavior, ProcessorBehavior::BadProcessorVersion) {
                "2.0.0"
            } else {
                "1.0.0"
            },
            processor_contract,
        )
        .expect("provenance is valid");
        if matches!(self.behavior, ProcessorBehavior::Unavailable) {
            return ProcessingResult::new(
                request.request_id(),
                request.task().clone(),
                request.binding().clone(),
                request.input_digest(),
                ProcessingStatus::Unavailable,
                processor,
                None,
                request.frame().quality().input_watermark(),
                TimestampMs::new(3_200),
                None,
                None,
                None,
                Some(
                    aether_domain::UnavailableInfo::new(
                        "MODEL_RUNTIME_UNAVAILABLE",
                        true,
                        Some(1_000),
                    )
                    .expect("unavailable metadata is valid"),
                ),
            )
            .map_err(|error| PortError::new(PortErrorKind::InvalidData, error.to_string()));
        }

        let timestamps: Vec<_> = request
            .frame()
            .future_covariates()
            .map(|future| future.timestamps().to_vec())
            .unwrap_or_else(|| {
                let ProcessingOptions::Forecast(options) = request.options();
                (1..=options.horizon_steps())
                    .map(|step| {
                        TimestampMs::new(
                            request.frame().as_of().get()
                                + request.frame().cadence_ms() * step as u64,
                        )
                    })
                    .collect()
            });
        let mut output_timestamps = timestamps;
        if matches!(self.behavior, ProcessorBehavior::BadHorizon) {
            output_timestamps.truncate(1);
        } else if matches!(self.behavior, ProcessorBehavior::BadTimestamps) {
            output_timestamps = vec![TimestampMs::new(5_000), TimestampMs::new(6_000)];
        } else if matches!(self.behavior, ProcessorBehavior::BadCadence) {
            output_timestamps = vec![TimestampMs::new(4_000), TimestampMs::new(6_000)];
        }
        let quantiles = matches!(self.behavior, ProcessorBehavior::BadQuantiles)
            .then(|| vec![ForecastQuantile::new(0.5, 12.0).expect("quantile is valid")])
            .unwrap_or_default();
        let emits_persistence_value = matches!(
            self.behavior,
            ProcessorBehavior::AllowedFallback
                | ProcessorBehavior::FallbackWrongVersion
                | ProcessorBehavior::FallbackExpiryTooLong
                | ProcessorBehavior::FallbackNotFromFeatureSource
        );
        let output = ForecastOutput::new(
            if matches!(self.behavior, ProcessorBehavior::BadTarget) {
                "pv"
            } else {
                "load"
            },
            if matches!(self.behavior, ProcessorBehavior::BadUnit) {
                "MW"
            } else {
                "kW"
            },
            if matches!(self.behavior, ProcessorBehavior::BadSign) {
                "positive_generation"
            } else {
                "positive_consumption"
            },
            if matches!(self.behavior, ProcessorBehavior::BadCadence) {
                2_000
            } else {
                1_000
            },
            output_timestamps
                .iter()
                .enumerate()
                .map(|(index, timestamp)| {
                    let value = if matches!(self.behavior, ProcessorBehavior::FallbackStaleSource) {
                        10.0
                    } else if emits_persistence_value {
                        11.0
                    } else {
                        12.0 + index as f64
                    };
                    ForecastPoint::new(*timestamp, value, quantiles.clone())
                        .expect("point is valid")
                })
                .collect(),
        )
        .expect("output is valid");
        let digest = if matches!(self.behavior, ProcessorBehavior::BadDigest) {
            "sha256:wrong"
        } else {
            request.input_digest()
        };
        let status = if matches!(
            self.behavior,
            ProcessorBehavior::AllowedFallback
                | ProcessorBehavior::FallbackWrongValues
                | ProcessorBehavior::FallbackStaleSource
                | ProcessorBehavior::FallbackWrongVersion
                | ProcessorBehavior::FallbackExpiryTooLong
                | ProcessorBehavior::FallbackNotFromFeatureSource
                | ProcessorBehavior::UndeclaredFallback
        ) {
            ProcessingStatus::Fallback
        } else {
            ProcessingStatus::Produced
        };
        let fallback = match self.behavior {
            ProcessorBehavior::AllowedFallback
            | ProcessorBehavior::FallbackWrongValues
            | ProcessorBehavior::FallbackExpiryTooLong => Some(
                FallbackInfo::new(
                    "persistence",
                    "1",
                    "MODEL_UNAVAILABLE",
                    "load",
                    request.frame().history().timestamps()[1],
                )
                .expect("fallback is valid"),
            ),
            ProcessorBehavior::FallbackStaleSource => Some(
                FallbackInfo::new(
                    "persistence",
                    "1",
                    "MODEL_UNAVAILABLE",
                    "load",
                    request.frame().history().timestamps()[0],
                )
                .expect("stale fallback source is structurally valid"),
            ),
            ProcessorBehavior::FallbackWrongVersion => Some(
                FallbackInfo::new(
                    "persistence",
                    "2",
                    "MODEL_UNAVAILABLE",
                    "load",
                    request.frame().history().timestamps()[1],
                )
                .expect("fallback is structurally valid"),
            ),
            ProcessorBehavior::FallbackNotFromFeatureSource => Some(
                FallbackInfo::new(
                    "persistence",
                    "1",
                    "MODEL_UNAVAILABLE",
                    "load",
                    TimestampMs::new(2_500),
                )
                .expect("fallback is structurally valid"),
            ),
            ProcessorBehavior::UndeclaredFallback => Some(
                FallbackInfo::new(
                    "zero-fill",
                    "1",
                    "MODEL_UNAVAILABLE",
                    "load",
                    request.frame().history().timestamps()[1],
                )
                .expect("fallback is valid"),
            ),
            _ => None,
        };
        ProcessingResult::new(
            if matches!(self.behavior, ProcessorBehavior::BadRequestId) {
                "different-request"
            } else {
                request.request_id()
            },
            if matches!(self.behavior, ProcessorBehavior::BadTask) {
                TaskIdentity::new("energy.site-load-forecast", 2).expect("identity is valid")
            } else {
                request.task().clone()
            },
            if matches!(self.behavior, ProcessorBehavior::BadBinding) {
                BindingIdentity::new("site-a", 8).expect("binding is valid")
            } else {
                request.binding().clone()
            },
            digest,
            status,
            processor,
            (status == ProcessingStatus::Produced
                && !matches!(self.behavior, ProcessorBehavior::MissingArtifact))
            .then(|| {
                ArtifactProvenance::new(
                    "model",
                    "site-load",
                    if matches!(self.behavior, ProcessorBehavior::BadArtifact) {
                        "v4"
                    } else {
                        "v3"
                    },
                    if matches!(self.behavior, ProcessorBehavior::BadArtifactDigest) {
                        "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
                    } else {
                        "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                    },
                )
                .expect("artifact provenance is valid")
            }),
            request.frame().quality().input_watermark(),
            TimestampMs::new(3_200),
            Some(
                if matches!(self.behavior, ProcessorBehavior::ExpiryTooLong) {
                    TimestampMs::new(14_000)
                } else if matches!(self.behavior, ProcessorBehavior::FallbackExpiryTooLong) {
                    TimestampMs::new(9_000)
                } else {
                    TimestampMs::new(8_000)
                },
            ),
            Some(ProcessingOutput::Forecast(output)),
            fallback,
            None,
        )
        .map_err(|error| PortError::new(PortErrorKind::InvalidData, error.to_string()))
    }
}

fn application(
    processor: Arc<RecordingProcessor>,
) -> (
    DataProcessingApplication,
    Arc<RecordingHistory>,
    Arc<RecordingCovariates>,
    Arc<EmptyLiveState>,
    Arc<RecordingAudit>,
) {
    let history = Arc::new(RecordingHistory::default());
    let covariates = Arc::new(RecordingCovariates::default());
    let live = Arc::new(EmptyLiveState::default());
    let audit = Arc::new(RecordingAudit::default());
    let route = DataProcessingRoute::new(task(), commissioned_binding(false), processor, 5_000)
        .expect("route is valid");
    let application = DataProcessingApplication::new(
        vec![route],
        history.clone(),
        Some(covariates.clone()),
        live.clone(),
        audit.clone(),
        Arc::new(ManualClock::new(TimestampMs::new(3_200))),
        SafetyPolicy,
    )
    .expect("application composition is valid");
    (application, history, covariates, live, audit)
}

#[test]
fn capability_metadata_is_stable_and_exposes_remote_egress_risk() {
    assert_eq!(TASKS_LIST_CAPABILITY.name(), "data_processing.tasks.list");
    assert_eq!(
        PROCESSOR_HEALTH_CAPABILITY.name(),
        "data_processing.processors.health"
    );
    for descriptor in [TASKS_LIST_CAPABILITY, PROCESSOR_HEALTH_CAPABILITY] {
        assert_eq!(descriptor.kind(), aether_application::OperationKind::Query);
        assert_eq!(descriptor.risk(), aether_application::RiskLevel::Low);
        assert_eq!(descriptor.required_permission(), "data_processing.read");
        assert_eq!(
            descriptor.confirmation(),
            aether_application::ConfirmationPolicy::Never
        );
        assert_eq!(descriptor.audit_policy(), AuditPolicy::NotRequired);
    }
    assert_eq!(PROCESS_DATA_CAPABILITY.name(), "data_processing.process");
    assert_eq!(
        PROCESS_DATA_CAPABILITY.kind(),
        aether_application::OperationKind::Query
    );
    assert_eq!(
        PROCESS_DATA_CAPABILITY.required_permission(),
        "data_processing.run"
    );
    assert_eq!(
        PROCESS_DATA_CAPABILITY.risk(),
        aether_application::RiskLevel::Medium
    );
    assert_eq!(
        PROCESS_DATA_CAPABILITY.confirmation(),
        aether_application::ConfirmationPolicy::Policy
    );
    assert_eq!(
        PROCESS_DATA_CAPABILITY.audit_policy(),
        AuditPolicy::Required
    );
    assert!(!PROCESS_DATA_CAPABILITY.is_idempotent());
}

#[tokio::test]
async fn process_assembles_a_complete_frame_invokes_processor_and_audits() {
    let processor = Arc::new(RecordingProcessor::new(
        DataBoundary::Local,
        ProcessorBehavior::Produced,
    ));
    let (application, history, covariates, _live, audit) = application(processor.clone());

    let derived = application
        .process(&context("data_processing.run", false), process_request())
        .await
        .expect("processing succeeds");

    assert_eq!(derived.result().status(), ProcessingStatus::Produced);
    assert_eq!(derived.result().task(), &task_identity());
    assert_eq!(derived.result().binding(), &binding_identity());
    assert!(derived.result().input_digest().starts_with("sha256:"));
    assert_eq!(history.windows.lock().expect("test lock").len(), 1);
    assert_eq!(covariates.windows.lock().expect("test lock").len(), 1);
    let requests = processor.requests.lock().expect("test lock");
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].frame().history().sample_count(), 2);
    assert_eq!(
        requests[0]
            .frame()
            .future_covariates()
            .expect("future exists")
            .sample_count(),
        2
    );
    assert_eq!(requests[0].processor_contract(), CONTRACT);
    assert!(Uuid::parse_str(requests[0].request_id()).is_ok());
    assert_ne!(requests[0].request_id(), OUTER_REQUEST_ID);
    assert_eq!(requests[0].submitted_at(), TimestampMs::new(3_100));
    assert_eq!(requests[0].deadline(), TimestampMs::new(8_100));
    assert_eq!(
        requests[0]
            .artifact_selector()
            .expect("artifact exists")
            .family(),
        "site-load"
    );
    assert_eq!(requests[0].frame().provenance().len(), 2);
    assert_eq!(
        requests[0].input_digest(),
        compute_input_digest(
            requests[0].task(),
            requests[0].binding(),
            requests[0].processor_contract(),
            requests[0].artifact_selector(),
            requests[0].frame(),
            requests[0].options(),
        )
        .expect("the shared codec can reproduce the application digest")
    );
    assert!(Uuid::parse_str(derived.result_id()).is_ok());
    let outcomes: Vec<_> = audit
        .records
        .lock()
        .expect("test lock")
        .iter()
        .map(AuditRecord::outcome)
        .collect();
    assert_eq!(
        outcomes,
        vec![AuditOutcome::Attempted, AuditOutcome::Succeeded]
    );
    assert!(
        audit
            .records
            .lock()
            .expect("test lock")
            .iter()
            .all(|record| record.request_id() == OUTER_REQUEST_ID)
    );
}

#[tokio::test]
async fn required_future_issue_time_is_enforced_before_processor_egress() {
    let processor = Arc::new(RecordingProcessor::new(
        DataBoundary::Local,
        ProcessorBehavior::Produced,
    ));
    let application = DataProcessingApplication::new(
        vec![
            DataProcessingRoute::new(
                task(),
                commissioned_binding(false),
                processor.clone(),
                5_000,
            )
            .expect("route is valid"),
        ],
        Arc::new(RecordingHistory::default()),
        Some(Arc::new(RecordingCovariates {
            omit_issue_time: true,
            ..RecordingCovariates::default()
        })),
        Arc::new(EmptyLiveState::default()),
        Arc::new(RecordingAudit::default()),
        Arc::new(ManualClock::new(TimestampMs::new(3_200))),
        SafetyPolicy,
    )
    .expect("application is valid");

    let result = application
        .process(&context("data_processing.run", false), process_request())
        .await;

    assert!(matches!(
        result,
        Err(ApplicationError::InputQualityRejected(_))
    ));
    assert!(processor.requests.lock().expect("test lock").is_empty());
}

#[tokio::test]
async fn repeated_correlation_inputs_derive_stable_ids_but_still_execute_each_request() {
    let processor = Arc::new(RecordingProcessor::new(
        DataBoundary::Local,
        ProcessorBehavior::Produced,
    ));
    let (application, _, _, _, _) = application(processor.clone());

    let first = application
        .process(&context("data_processing.run", false), process_request())
        .await
        .expect("first processing succeeds");
    let second = application
        .process(&context("data_processing.run", false), process_request())
        .await
        .expect("repeated request succeeds");

    let requests = processor.requests.lock().expect("test lock");
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].request_id(), requests[1].request_id());
    assert!(Uuid::parse_str(requests[0].request_id()).is_ok());
    assert_eq!(first.result_id(), second.result_id());
    assert!(Uuid::parse_str(first.result_id()).is_ok());
}

#[tokio::test]
async fn commissioned_static_features_receive_exact_constant_provenance() {
    let static_definition =
        FeatureDefinition::new("site_class", FeatureRole::Static, FeatureValueType::Text)
            .expect("static definition is valid");
    let static_task = DataProcessingTask::forecast(
        task_identity(),
        CONTRACT,
        vec![
            history_feature(),
            future_feature(),
            static_definition.clone(),
        ],
        ForecastTaskSpec::new(
            ForecastTarget::new("load", "kW", "positive_consumption").expect("target is valid"),
            1_000,
            HistoryAggregation::Mean,
            HistoryDuplicatePolicy::Latest,
            2,
            4,
            10_000,
            0.0,
            vec![],
        )
        .expect("spec is valid"),
    )
    .expect("task is valid");
    let binding = commissioned_binding(false).with_static_features(vec![
        StaticFeature::new(
            static_definition,
            FeatureValue::text("commercial"),
            SampleQuality::Good,
        )
        .expect("static feature is valid"),
    ]);
    let processor = Arc::new(RecordingProcessor::new(
        DataBoundary::Local,
        ProcessorBehavior::Produced,
    ));
    let application = DataProcessingApplication::new(
        vec![
            DataProcessingRoute::new(static_task, binding, processor.clone(), 5_000)
                .expect("route is valid"),
        ],
        Arc::new(RecordingHistory::default()),
        Some(Arc::new(RecordingCovariates::default())),
        Arc::new(EmptyLiveState::default()),
        Arc::new(RecordingAudit::default()),
        Arc::new(ManualClock::new(TimestampMs::new(3_200))),
        SafetyPolicy,
    )
    .expect("application is valid");

    application
        .process(&context("data_processing.run", false), process_request())
        .await
        .expect("processing succeeds");

    let requests = processor.requests.lock().expect("test lock");
    let frame = requests[0].frame();
    assert_eq!(frame.static_features().len(), 1);
    assert_eq!(frame.provenance().len(), 3);
    let static_source = frame
        .provenance()
        .iter()
        .find(|source| {
            source.segment() == SegmentKind::StaticFeatures && source.feature() == "site_class"
        })
        .expect("static provenance exists");
    assert_eq!(static_source.source_kind(), SourceKind::Constant);
    assert_eq!(static_source.watermark(), TimestampMs::new(3_000));
    assert_eq!(static_source.source_ref(), None);
    assert_eq!(frame.quality().input_watermark(), TimestampMs::new(3_000));
}

#[tokio::test]
async fn calendar_provenance_does_not_advance_the_actual_input_watermark() {
    let processor = Arc::new(RecordingProcessor::new(
        DataBoundary::Local,
        ProcessorBehavior::Produced,
    ));
    let application = DataProcessingApplication::new(
        vec![
            DataProcessingRoute::new(
                task(),
                commissioned_binding(false),
                processor.clone(),
                5_000,
            )
            .expect("route is valid"),
        ],
        Arc::new(RecordingHistory::default()),
        Some(Arc::new(CalendarCovariates)),
        Arc::new(EmptyLiveState::default()),
        Arc::new(RecordingAudit::default()),
        Arc::new(ManualClock::new(TimestampMs::new(3_200))),
        SafetyPolicy,
    )
    .expect("application is valid");

    application
        .process(&context("data_processing.run", false), process_request())
        .await
        .expect("processing succeeds");

    let requests = processor.requests.lock().expect("test lock");
    assert_eq!(
        requests[0].frame().quality().input_watermark(),
        TimestampMs::new(3_000)
    );
}

#[tokio::test]
async fn duplicate_or_extra_source_provenance_is_rejected_before_processing() {
    let processor = Arc::new(RecordingProcessor::new(
        DataBoundary::Local,
        ProcessorBehavior::Produced,
    ));
    let application = DataProcessingApplication::new(
        vec![
            DataProcessingRoute::new(
                task(),
                commissioned_binding(false),
                processor.clone(),
                5_000,
            )
            .expect("route is valid"),
        ],
        Arc::new(DuplicateProvenanceHistory),
        Some(Arc::new(RecordingCovariates::default())),
        Arc::new(EmptyLiveState::default()),
        Arc::new(RecordingAudit::default()),
        Arc::new(ManualClock::new(TimestampMs::new(3_200))),
        SafetyPolicy,
    )
    .expect("application is valid");

    let result = application
        .process(&context("data_processing.run", false), process_request())
        .await;

    assert!(
        matches!(
            &result,
            Err(ApplicationError::InvalidProcessingRequest(_))
                | Err(ApplicationError::HistoryQueryFailed(_))
        ),
        "unexpected result: {result:?}"
    );
    assert!(processor.requests.lock().expect("test lock").is_empty());
}

#[tokio::test]
async fn permission_denial_happens_before_any_data_read_or_processor_call() {
    let processor = Arc::new(RecordingProcessor::new(
        DataBoundary::Local,
        ProcessorBehavior::Produced,
    ));
    let (application, history, covariates, live, audit) = application(processor.clone());

    let result = application
        .process(&context("data_processing.read", false), process_request())
        .await;

    assert!(matches!(
        result,
        Err(ApplicationError::PermissionDenied { .. })
    ));
    assert!(history.windows.lock().expect("test lock").is_empty());
    assert!(covariates.windows.lock().expect("test lock").is_empty());
    assert_eq!(*live.reads.lock().expect("test lock"), 0);
    assert!(processor.requests.lock().expect("test lock").is_empty());
    assert_eq!(
        audit.records.lock().expect("test lock")[0].outcome(),
        AuditOutcome::Rejected
    );
}

#[tokio::test]
async fn mandatory_attempt_audit_fails_closed_before_processor_egress() {
    let processor = Arc::new(RecordingProcessor::new(
        DataBoundary::Local,
        ProcessorBehavior::Produced,
    ));
    let application = DataProcessingApplication::new(
        vec![
            DataProcessingRoute::new(
                task(),
                commissioned_binding(false),
                processor.clone(),
                5_000,
            )
            .expect("route is valid"),
        ],
        Arc::new(RecordingHistory::default()),
        Some(Arc::new(RecordingCovariates::default())),
        Arc::new(EmptyLiveState::default()),
        Arc::new(RecordingAudit {
            records: Mutex::new(vec![]),
            fail: true,
        }),
        Arc::new(ManualClock::new(TimestampMs::new(3_200))),
        SafetyPolicy,
    )
    .expect("application is valid");

    let result = application
        .process(&context("data_processing.run", false), process_request())
        .await;

    assert!(matches!(result, Err(ApplicationError::AuditUnavailable(_))));
    assert!(processor.requests.lock().expect("test lock").is_empty());
}

#[tokio::test]
async fn invalid_horizon_and_stale_revisions_fail_before_source_reads() {
    let processor = Arc::new(RecordingProcessor::new(
        DataBoundary::Local,
        ProcessorBehavior::Produced,
    ));
    let (application, history, covariates, live, _) = application(processor.clone());
    let oversized = ProcessTaskRequest::new(
        task_identity(),
        binding_identity(),
        TimestampMs::new(3_000),
        ProcessingOptions::Forecast(
            ForecastOptions::new(5, vec![]).expect("caller options are structurally valid"),
        ),
    );
    let result = application
        .process(&context("data_processing.run", false), oversized)
        .await;
    assert!(matches!(
        result,
        Err(ApplicationError::InvalidProcessingRequest(_))
    ));
    assert!(history.windows.lock().expect("test lock").is_empty());
    assert!(covariates.windows.lock().expect("test lock").is_empty());

    let unsupported_quantile = ProcessTaskRequest::new(
        task_identity(),
        binding_identity(),
        TimestampMs::new(3_000),
        ProcessingOptions::Forecast(
            ForecastOptions::new(2, vec![0.5]).expect("caller quantile is structurally valid"),
        ),
    );
    let result = application
        .process(&context("data_processing.run", false), unsupported_quantile)
        .await;
    assert!(matches!(
        result,
        Err(ApplicationError::InvalidProcessingRequest(_))
    ));
    assert!(history.windows.lock().expect("test lock").is_empty());
    assert!(covariates.windows.lock().expect("test lock").is_empty());

    let stale = ProcessTaskRequest::new(
        TaskIdentity::new("energy.site-load-forecast", 2).expect("identity is valid"),
        binding_identity(),
        TimestampMs::new(3_000),
        ProcessingOptions::Forecast(ForecastOptions::new(2, vec![]).expect("options are valid")),
    );
    let result = application
        .process(&context("data_processing.run", false), stale)
        .await;
    assert!(matches!(
        result,
        Err(ApplicationError::InvalidProcessingConfiguration(_))
    ));
    let stale_binding = ProcessTaskRequest::new(
        task_identity(),
        BindingIdentity::new("site-a", 8).expect("binding is valid"),
        TimestampMs::new(3_000),
        ProcessingOptions::Forecast(ForecastOptions::new(2, vec![]).expect("options are valid")),
    );
    let result = application
        .process(&context("data_processing.run", false), stale_binding)
        .await;
    assert!(matches!(
        result,
        Err(ApplicationError::InvalidProcessingConfiguration(_))
    ));
    assert!(history.windows.lock().expect("test lock").is_empty());
    assert!(covariates.windows.lock().expect("test lock").is_empty());
    assert_eq!(*live.reads.lock().expect("test lock"), 0);
    assert!(processor.requests.lock().expect("test lock").is_empty());
}

#[tokio::test]
async fn future_as_of_is_rejected_before_any_source_read() {
    let processor = Arc::new(RecordingProcessor::new(
        DataBoundary::Local,
        ProcessorBehavior::Produced,
    ));
    let (application, history, covariates, live, _) = application(processor.clone());
    let future = ProcessTaskRequest::new(
        task_identity(),
        binding_identity(),
        TimestampMs::new(3_101),
        ProcessingOptions::Forecast(ForecastOptions::new(2, vec![]).expect("options are valid")),
    );

    let result = application
        .process(&context("data_processing.run", false), future)
        .await;

    assert!(matches!(
        result,
        Err(ApplicationError::InvalidProcessingRequest(_))
    ));
    assert!(history.windows.lock().expect("test lock").is_empty());
    assert!(covariates.windows.lock().expect("test lock").is_empty());
    assert_eq!(*live.reads.lock().expect("test lock"), 0);
    assert!(processor.requests.lock().expect("test lock").is_empty());
}

#[tokio::test]
async fn off_cadence_as_of_is_rejected_before_any_source_read() {
    let processor = Arc::new(RecordingProcessor::new(
        DataBoundary::Local,
        ProcessorBehavior::Produced,
    ));
    let (application, history, covariates, live, _) = application(processor.clone());
    let off_cadence = ProcessTaskRequest::new(
        task_identity(),
        binding_identity(),
        TimestampMs::new(2_500),
        ProcessingOptions::Forecast(ForecastOptions::new(2, vec![]).expect("options are valid")),
    );

    let result = application
        .process(&context("data_processing.run", false), off_cadence)
        .await;

    assert!(matches!(
        result,
        Err(ApplicationError::InvalidProcessingRequest(_))
    ));
    assert!(history.windows.lock().expect("test lock").is_empty());
    assert!(covariates.windows.lock().expect("test lock").is_empty());
    assert_eq!(*live.reads.lock().expect("test lock"), 0);
    assert!(processor.requests.lock().expect("test lock").is_empty());
}

#[tokio::test]
async fn route_deadline_bounds_mandatory_audit_and_releases_the_route_permit() {
    let processor = Arc::new(RecordingProcessor::new(
        DataBoundary::Local,
        ProcessorBehavior::Produced,
    ));
    let application = DataProcessingApplication::new(
        vec![
            DataProcessingRoute::new(task(), commissioned_binding(false), processor, 100)
                .expect("route is valid"),
        ],
        Arc::new(RecordingHistory::default()),
        Some(Arc::new(RecordingCovariates::default())),
        Arc::new(EmptyLiveState::default()),
        Arc::new(BlockFirstAttemptAudit::default()),
        Arc::new(ManualClock::new(TimestampMs::new(3_200))),
        SafetyPolicy,
    )
    .expect("application is valid");

    let started = std::time::Instant::now();
    let first = application
        .process(&context("data_processing.run", false), process_request())
        .await;
    assert!(matches!(first, Err(ApplicationError::AuditUnavailable(_))));
    assert!(started.elapsed() < std::time::Duration::from_secs(1));

    application
        .process(&context("data_processing.run", false), process_request())
        .await
        .expect("the timed-out audit releases the route permit for the next request");
}

#[tokio::test]
async fn processor_timeout_reserves_time_for_the_mandatory_terminal_audit() {
    let processor = Arc::new(DelayedProcessor {
        inner: RecordingProcessor::new(DataBoundary::Local, ProcessorBehavior::Produced),
        delay: std::time::Duration::from_millis(250),
    });
    let audit = Arc::new(RecordingAudit::default());
    let application = DataProcessingApplication::new(
        vec![
            DataProcessingRoute::new(task(), commissioned_binding(false), processor, 50)
                .expect("route is valid"),
        ],
        Arc::new(RecordingHistory::default()),
        Some(Arc::new(RecordingCovariates::default())),
        Arc::new(EmptyLiveState::default()),
        audit.clone(),
        Arc::new(ManualClock::new(TimestampMs::new(3_200))),
        SafetyPolicy,
    )
    .expect("application is valid");

    let result = application
        .process(&context("data_processing.run", false), process_request())
        .await;

    assert!(matches!(
        result,
        Err(ApplicationError::ProcessorFailed(ref error))
            if error.kind() == PortErrorKind::Timeout
    ));
    assert_eq!(
        audit
            .records
            .lock()
            .expect("test lock")
            .iter()
            .map(AuditRecord::outcome)
            .collect::<Vec<_>>(),
        vec![AuditOutcome::Attempted, AuditOutcome::Failed]
    );
}

#[tokio::test]
async fn route_concurrency_bound_rejects_excess_work_before_frame_assembly() {
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let processor = Arc::new(BlockingProcessor {
        inner: RecordingProcessor::new(DataBoundary::Local, ProcessorBehavior::Produced),
        entered: entered.clone(),
        release: release.clone(),
    });
    let history = Arc::new(RecordingHistory::default());
    let covariates = Arc::new(RecordingCovariates::default());
    let route = DataProcessingRoute::new(
        task(),
        commissioned_binding(false),
        processor.clone(),
        5_000,
    )
    .expect("route is valid")
    .with_max_concurrency(1)
    .expect("bound is valid");
    let application = Arc::new(
        DataProcessingApplication::new(
            vec![route],
            history.clone(),
            Some(covariates.clone()),
            Arc::new(EmptyLiveState::default()),
            Arc::new(RecordingAudit::default()),
            Arc::new(ManualClock::new(TimestampMs::new(3_200))),
            SafetyPolicy,
        )
        .expect("application is valid"),
    );
    let first_application = application.clone();
    let first = tokio::spawn(async move {
        first_application
            .process(&context("data_processing.run", false), process_request())
            .await
    });
    entered.notified().await;

    let before = history.windows.lock().expect("test lock").len();
    let rejected = application
        .process(&context("data_processing.run", false), process_request())
        .await;
    assert!(matches!(
        rejected,
        Err(ApplicationError::ProcessingUnavailable {
            retryable: true,
            ..
        })
    ));
    assert_eq!(history.windows.lock().expect("test lock").len(), before);

    release.notify_one();
    first
        .await
        .expect("task joins")
        .expect("first request completes");
}

#[tokio::test]
async fn remote_route_requires_policy_confirmation_before_data_egress() {
    let processor = Arc::new(RecordingProcessor::new(
        DataBoundary::Remote,
        ProcessorBehavior::Produced,
    ));
    assert!(matches!(
        DataProcessingRoute::new(
            task(),
            commissioned_binding(false),
            processor.clone(),
            5_000
        ),
        Err(ApplicationError::InvalidProcessingConfiguration(_))
    ));
    let history = Arc::new(RecordingHistory::default());
    let application = DataProcessingApplication::new(
        vec![
            DataProcessingRoute::new(
                task().allowing_remote_egress(),
                commissioned_binding(false).allowing_remote_egress(),
                processor.clone(),
                5_000,
            )
            .expect("task-owned egress policy permits the remote route"),
        ],
        history.clone(),
        Some(Arc::new(RecordingCovariates::default())),
        Arc::new(EmptyLiveState::default()),
        Arc::new(RecordingAudit::default()),
        Arc::new(ManualClock::new(TimestampMs::new(3_200))),
        SafetyPolicy,
    )
    .expect("application is valid");

    let denied = application
        .process(&context("data_processing.run", false), process_request())
        .await;

    assert!(matches!(
        denied,
        Err(ApplicationError::ConfirmationRequired { .. })
    ));
    assert!(history.windows.lock().expect("test lock").is_empty());
    assert!(processor.requests.lock().expect("test lock").is_empty());
}

#[tokio::test]
async fn mismatched_processor_correlation_is_rejected_as_untrusted_output() {
    let processor = Arc::new(RecordingProcessor::new(
        DataBoundary::Local,
        ProcessorBehavior::BadDigest,
    ));
    let (application, _, _, _, audit) = application(processor);

    let result = application
        .process(&context("data_processing.run", false), process_request())
        .await;

    assert!(matches!(
        result,
        Err(ApplicationError::InvalidProcessorResult(_))
    ));
    assert_eq!(
        audit
            .records
            .lock()
            .expect("test lock")
            .last()
            .map(AuditRecord::outcome),
        Some(AuditOutcome::Rejected)
    );
}

#[tokio::test]
async fn every_processor_correlation_and_contract_field_is_verified() {
    for behavior in [
        ProcessorBehavior::BadRequestId,
        ProcessorBehavior::BadTask,
        ProcessorBehavior::BadBinding,
        ProcessorBehavior::BadDigest,
        ProcessorBehavior::BadProcessorId,
        ProcessorBehavior::BadProcessorVersion,
        ProcessorBehavior::BadProcessorContract,
    ] {
        let processor = Arc::new(RecordingProcessor::new(DataBoundary::Local, behavior));
        let (application, _, _, _, _) = application(processor);

        let result = application
            .process(&context("data_processing.run", false), process_request())
            .await;

        assert!(
            matches!(result, Err(ApplicationError::InvalidProcessorResult(_))),
            "behavior {behavior:?} must fail closed"
        );
    }
}

#[tokio::test]
async fn forecast_semantics_shape_time_axis_quantiles_and_expiry_are_verified() {
    for behavior in [
        ProcessorBehavior::BadTarget,
        ProcessorBehavior::BadUnit,
        ProcessorBehavior::BadSign,
        ProcessorBehavior::BadCadence,
        ProcessorBehavior::BadHorizon,
        ProcessorBehavior::BadTimestamps,
        ProcessorBehavior::BadQuantiles,
        ProcessorBehavior::ExpiryTooLong,
        ProcessorBehavior::BadArtifact,
        ProcessorBehavior::BadArtifactDigest,
        ProcessorBehavior::MissingArtifact,
    ] {
        let processor = Arc::new(RecordingProcessor::new(DataBoundary::Local, behavior));
        let (application, _, _, _, _) = application(processor);

        let result = application
            .process(&context("data_processing.run", false), process_request())
            .await;

        assert!(
            matches!(result, Err(ApplicationError::InvalidProcessorResult(_))),
            "behavior {behavior:?} must fail closed"
        );
    }
}

#[tokio::test]
async fn only_task_approved_fallbacks_can_be_accepted_as_derived_data() {
    let allowed = Arc::new(RecordingProcessor::new(
        DataBoundary::Local,
        ProcessorBehavior::AllowedFallback,
    ));
    let (allowed_application, _, _, _, _) = application(allowed);
    let derived = allowed_application
        .process(&context("data_processing.run", false), process_request())
        .await
        .expect("declared fallback is usable derived data");
    assert_eq!(derived.result().status(), ProcessingStatus::Fallback);

    let future_dated = Arc::new(RecordingProcessor::new(
        DataBoundary::Local,
        ProcessorBehavior::FallbackNotFromFeatureSource,
    ));
    let (future_dated_application, _, _, _, _) = application(future_dated);
    let result = future_dated_application
        .process(&context("data_processing.run", false), process_request())
        .await;
    assert!(matches!(
        result,
        Err(ApplicationError::InvalidProcessorResult(_))
    ));

    for behavior in [
        ProcessorBehavior::FallbackWrongValues,
        ProcessorBehavior::FallbackStaleSource,
        ProcessorBehavior::FallbackWrongVersion,
        ProcessorBehavior::FallbackExpiryTooLong,
    ] {
        let processor = Arc::new(RecordingProcessor::new(DataBoundary::Local, behavior));
        let (application, _, _, _, _) = application(processor);
        let result = application
            .process(&context("data_processing.run", false), process_request())
            .await;
        assert!(
            matches!(result, Err(ApplicationError::InvalidProcessorResult(_))),
            "fallback behavior {behavior:?} must fail closed"
        );
    }

    let undeclared = Arc::new(RecordingProcessor::new(
        DataBoundary::Local,
        ProcessorBehavior::UndeclaredFallback,
    ));
    let (undeclared_application, _, _, _, _) = application(undeclared);
    let result = undeclared_application
        .process(&context("data_processing.run", false), process_request())
        .await;
    assert!(matches!(
        result,
        Err(ApplicationError::InvalidProcessorResult(_))
    ));

    let ungoverned_task = DataProcessingTask::forecast(
        task_identity(),
        CONTRACT,
        vec![history_feature(), future_feature()],
        ForecastTaskSpec::new(
            ForecastTarget::new("load", "kW", "positive_consumption").expect("target is valid"),
            1_000,
            HistoryAggregation::Mean,
            HistoryDuplicatePolicy::Latest,
            2,
            4,
            10_000,
            0.0,
            vec!["persistence".into()],
        )
        .expect("name-only fallback policy is structurally valid"),
    )
    .expect("task is structurally valid");
    let processor = Arc::new(RecordingProcessor::new(
        DataBoundary::Local,
        ProcessorBehavior::AllowedFallback,
    ));
    assert!(matches!(
        DataProcessingRoute::new(
            ungoverned_task,
            commissioned_binding(false),
            processor,
            5_000,
        ),
        Err(ApplicationError::InvalidProcessingConfiguration(_))
    ));
}

#[tokio::test]
async fn explicit_unavailable_result_is_not_stamped_as_derived_data() {
    let processor = Arc::new(RecordingProcessor::new(
        DataBoundary::Local,
        ProcessorBehavior::Unavailable,
    ));
    let (application, _, _, _, _) = application(processor);

    let result = application
        .process(&context("data_processing.run", false), process_request())
        .await;

    assert!(matches!(
        result,
        Err(ApplicationError::ProcessingUnavailable {
            retryable: true,
            ..
        })
    ));
}

#[tokio::test]
async fn task_and_processor_discovery_use_read_permission_and_same_routes() {
    let processor = Arc::new(RecordingProcessor::new(
        DataBoundary::Local,
        ProcessorBehavior::Produced,
    ));
    let (application, _, _, _, audit) = application(processor);
    let read_context = context("data_processing.read", false);

    let tasks = application
        .list_tasks(&read_context)
        .await
        .expect("task discovery succeeds");
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0].task(), &task_identity());
    assert_eq!(tasks[0].binding(), &binding_identity());
    assert_eq!(tasks[0].data_boundary(), DataBoundary::Local);
    assert_eq!(tasks[0].definition().processor_contract(), CONTRACT);
    assert_eq!(tasks[0].definition().features().len(), 2);
    assert_eq!(
        tasks[0]
            .definition()
            .forecast_spec()
            .expect("forecast policy exists")
            .fallback_policies()
            .len(),
        1
    );
    assert_eq!(tasks[0].processor_version(), "1.0.0");
    assert_eq!(tasks[0].max_concurrency(), 1);
    assert_eq!(tasks[0].deadline_ms(), 5_000);
    assert_eq!(
        tasks[0]
            .artifact()
            .expect("artifact is discoverable")
            .digest(),
        Some("sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
    );

    let health = application
        .processor_health(&read_context)
        .await
        .expect("health discovery succeeds");
    assert_eq!(health.len(), 1);
    assert_eq!(health[0].processor_id(), "forecast-test");
    assert_eq!(health[0].health(), ProcessorHealth::Healthy);
    assert!(audit.records.lock().expect("test lock").is_empty());
}

#[tokio::test]
async fn an_unconfigured_optional_module_lists_no_routes_without_external_services() {
    let application = DataProcessingApplication::new(
        vec![],
        Arc::new(RecordingHistory::default()),
        None,
        Arc::new(EmptyLiveState::default()),
        Arc::new(RecordingAudit::default()),
        Arc::new(ManualClock::new(TimestampMs::new(3_200))),
        SafetyPolicy,
    )
    .expect("an optional unconfigured module is valid");

    assert!(
        application
            .list_tasks(&context("data_processing.read", false))
            .await
            .expect("discovery succeeds")
            .is_empty()
    );
}

#[tokio::test]
async fn discovery_is_authorized_before_health_ports_and_health_failures_are_isolated() {
    let processor = Arc::new(RecordingProcessor::new(
        DataBoundary::Local,
        ProcessorBehavior::HealthFailure,
    ));
    let (application, _, _, _, _) = application(processor.clone());

    let denied = application
        .processor_health(&context("data_processing.run", false))
        .await;
    assert!(matches!(
        denied,
        Err(ApplicationError::PermissionDenied { .. })
    ));
    assert_eq!(*processor.health_calls.lock().expect("test lock"), 0);

    let health = application
        .processor_health(&context("data_processing.read", false))
        .await
        .expect("a failed probe is represented as unavailable discovery state");
    assert_eq!(health.len(), 1);
    assert_eq!(health[0].health(), ProcessorHealth::Unavailable);
}

#[tokio::test]
async fn history_covariate_and_processor_failures_remain_typed_and_isolated() {
    let processor = Arc::new(RecordingProcessor::new(
        DataBoundary::Local,
        ProcessorBehavior::Produced,
    ));
    let history = Arc::new(RecordingHistory {
        fail: true,
        ..RecordingHistory::default()
    });
    let covariates = Arc::new(RecordingCovariates::default());
    let live = Arc::new(EmptyLiveState::default());
    let audit = Arc::new(RecordingAudit::default());
    let history_failure_application = DataProcessingApplication::new(
        vec![
            DataProcessingRoute::new(
                task(),
                commissioned_binding(false),
                processor.clone(),
                5_000,
            )
            .expect("route is valid"),
        ],
        history,
        Some(covariates.clone()),
        live,
        audit,
        Arc::new(ManualClock::new(TimestampMs::new(3_200))),
        SafetyPolicy,
    )
    .expect("application is valid");
    let result = history_failure_application
        .process(&context("data_processing.run", false), process_request())
        .await;
    assert!(matches!(
        result,
        Err(ApplicationError::HistoryQueryFailed(_))
    ));
    assert!(covariates.windows.lock().expect("test lock").is_empty());
    assert!(processor.requests.lock().expect("test lock").is_empty());

    let processor = Arc::new(RecordingProcessor::new(
        DataBoundary::Local,
        ProcessorBehavior::Produced,
    ));
    let history = Arc::new(RecordingHistory::default());
    let covariates = Arc::new(RecordingCovariates {
        fail: true,
        ..RecordingCovariates::default()
    });
    let covariate_failure_application = DataProcessingApplication::new(
        vec![
            DataProcessingRoute::new(
                task(),
                commissioned_binding(false),
                processor.clone(),
                5_000,
            )
            .expect("route is valid"),
        ],
        history.clone(),
        Some(covariates),
        Arc::new(EmptyLiveState::default()),
        Arc::new(RecordingAudit::default()),
        Arc::new(ManualClock::new(TimestampMs::new(3_200))),
        SafetyPolicy,
    )
    .expect("application is valid");
    let result = covariate_failure_application
        .process(&context("data_processing.run", false), process_request())
        .await;
    assert!(matches!(
        result,
        Err(ApplicationError::CovariateSourceFailed(_))
    ));
    assert_eq!(history.windows.lock().expect("test lock").len(), 1);
    assert!(processor.requests.lock().expect("test lock").is_empty());

    let processor = Arc::new(RecordingProcessor::new(
        DataBoundary::Local,
        ProcessorBehavior::ProcessFailure,
    ));
    let (application, _, _, _, audit) = application(processor);
    let result = application
        .process(&context("data_processing.run", false), process_request())
        .await;
    assert!(matches!(result, Err(ApplicationError::ProcessorFailed(_))));
    assert_eq!(
        audit
            .records
            .lock()
            .expect("test lock")
            .last()
            .map(AuditRecord::outcome),
        Some(AuditOutcome::Failed)
    );
}

#[test]
fn commissioned_binding_rejects_writable_points_and_unmapped_targets() {
    let writable = PointFeatureBinding::new(
        "load",
        PointAddress::new(InstanceId::new(1), PointKind::Command, PointId::new(1)),
    );
    assert!(matches!(
        writable,
        Err(ApplicationError::InvalidProcessingConfiguration(_))
    ));

    let wrong_feature = DataProcessingBinding::new(
        binding_identity(),
        vec![
            PointFeatureBinding::new("unknown", load_address()).expect("point itself is read-only"),
        ],
    )
    .expect("binding shape is valid");
    let processor = Arc::new(RecordingProcessor::new(
        DataBoundary::Local,
        ProcessorBehavior::Produced,
    ));
    let route = DataProcessingRoute::new(task(), wrong_feature, processor, 5_000);
    assert!(matches!(
        route,
        Err(ApplicationError::InvalidProcessingConfiguration(_))
    ));

    let processor = Arc::new(RecordingProcessor::new(
        DataBoundary::Local,
        ProcessorBehavior::Produced,
    ));
    let mean_live_tail =
        DataProcessingRoute::new(task(), commissioned_binding(true), processor, 5_000);
    assert!(matches!(
        mean_live_tail,
        Err(ApplicationError::InvalidProcessingConfiguration(_))
    ));
}

#[tokio::test]
async fn descriptor_frame_and_encoded_request_limits_are_enforced_before_processor_execution() {
    for (cadence_ms, history_steps) in [(500_u64, 2_usize), (1_000, 20_001)] {
        let unsupported = DataProcessingTask::forecast(
            task_identity(),
            CONTRACT,
            vec![history_feature()],
            ForecastTaskSpec::new(
                ForecastTarget::new("load", "kW", "positive_consumption").expect("target is valid"),
                cadence_ms,
                HistoryAggregation::Mean,
                HistoryDuplicatePolicy::Latest,
                history_steps,
                1,
                10_000,
                0.0,
                vec![],
            )
            .expect("domain task is structurally valid"),
        )
        .expect("domain task is valid");
        let processor = Arc::new(RecordingProcessor::with_limits(
            DataBoundary::Local,
            ProcessorBehavior::Produced,
            30_000,
            1_048_576,
        ));
        assert!(matches!(
            DataProcessingRoute::new(unsupported, commissioned_binding(false), processor, 5_000,),
            Err(ApplicationError::InvalidProcessingConfiguration(_))
        ));
    }

    let invalid_wire_binding = DataProcessingBinding::new(
        BindingIdentity::new("site a", 7).expect("domain identity is structurally valid"),
        vec![PointFeatureBinding::new("load", load_address()).expect("point binding is valid")],
    )
    .expect("domain binding is structurally valid");
    let processor = Arc::new(RecordingProcessor::new(
        DataBoundary::Local,
        ProcessorBehavior::Produced,
    ));
    assert!(matches!(
        DataProcessingRoute::new(task(), invalid_wire_binding, processor, 5_000),
        Err(ApplicationError::InvalidProcessingConfiguration(_))
    ));

    let mut invalid_wire_descriptor =
        RecordingProcessor::new(DataBoundary::Local, ProcessorBehavior::Produced);
    invalid_wire_descriptor.descriptor = DataProcessorDescriptor::new(
        "forecast test",
        "1.0.0",
        vec![TaskKind::Forecast],
        vec![CONTRACT.into()],
        DataBoundary::Local,
        32,
        1_048_576,
    )
    .expect("port descriptor is structurally valid");
    assert!(matches!(
        DataProcessingRoute::new(
            task(),
            commissioned_binding(false),
            Arc::new(invalid_wire_descriptor),
            5_000,
        ),
        Err(ApplicationError::InvalidProcessingConfiguration(_))
    ));

    let too_small_for_task = Arc::new(RecordingProcessor::with_limits(
        DataBoundary::Local,
        ProcessorBehavior::Produced,
        5,
        1_048_576,
    ));
    let route = DataProcessingRoute::new(
        task(),
        commissioned_binding(false),
        too_small_for_task,
        5_000,
    );
    assert!(matches!(
        route,
        Err(ApplicationError::InvalidProcessingConfiguration(_))
    ));

    let second_history = FeatureDefinition::numeric("ambient", FeatureRole::History, "Cel")
        .expect("feature is valid");
    let multi_series_task = DataProcessingTask::forecast(
        task_identity(),
        CONTRACT,
        vec![history_feature(), second_history, future_feature()],
        ForecastTaskSpec::new(
            ForecastTarget::new("load", "kW", "positive_consumption").expect("target is valid"),
            1_000,
            HistoryAggregation::Mean,
            HistoryDuplicatePolicy::Latest,
            2,
            4,
            10_000,
            0.0,
            vec![],
        )
        .expect("spec is valid"),
    )
    .expect("task is valid");
    let row_count_only_limit = Arc::new(RecordingProcessor::with_limits(
        DataBoundary::Local,
        ProcessorBehavior::Produced,
        6,
        1_048_576,
    ));
    let route = DataProcessingRoute::new(
        multi_series_task,
        commissioned_binding(false),
        row_count_only_limit,
        5_000,
    );
    assert!(matches!(
        route,
        Err(ApplicationError::InvalidProcessingConfiguration(_))
    ));

    let wire_limited = Arc::new(RecordingProcessor::with_limits(
        DataBoundary::Local,
        ProcessorBehavior::Produced,
        32,
        1,
    ));
    let (application, _, _, _, _) = application(wire_limited.clone());
    let result = application
        .process(&context("data_processing.run", false), process_request())
        .await;
    assert!(matches!(
        result,
        Err(ApplicationError::ProcessingRequestTooLarge { max_bytes: 1, .. })
    ));
    assert!(wire_limited.requests.lock().expect("test lock").is_empty());
}

#[allow(dead_code)]
fn sample_live_point() -> (PointAddress, PointSample) {
    let address = PointAddress::new(InstanceId::new(1), PointKind::Telemetry, PointId::new(1));
    (
        address,
        PointSample::new(
            address,
            12.0,
            TimestampMs::new(3_000),
            aether_domain::PointQuality::Good,
        ),
    )
}

struct OneLiveState {
    sample: PointSample,
}

#[async_trait]
impl LiveState for OneLiveState {
    async fn read(&self, address: PointAddress) -> PortResult<Option<PointSample>> {
        Ok((address == self.sample.address()).then_some(self.sample))
    }

    async fn read_many(&self, addresses: &[PointAddress]) -> PortResult<Vec<Option<PointSample>>> {
        Ok(addresses
            .iter()
            .map(|address| (*address == self.sample.address()).then_some(self.sample))
            .collect())
    }
}

#[tokio::test]
async fn complete_fresh_live_tail_replaces_the_latest_history_cell_without_changing_authority() {
    let processor = Arc::new(RecordingProcessor::new(
        DataBoundary::Local,
        ProcessorBehavior::Produced,
    ));
    let history = Arc::new(RecordingHistory::default());
    let live_point = sample_live_point();
    let live = Arc::new(OneLiveState {
        sample: live_point.1,
    });
    let audit = Arc::new(RecordingAudit::default());
    let load_only_task = DataProcessingTask::forecast(
        task_identity(),
        CONTRACT,
        vec![history_feature()],
        ForecastTaskSpec::new(
            ForecastTarget::new("load", "kW", "positive_consumption").expect("target is valid"),
            1_000,
            HistoryAggregation::Last,
            HistoryDuplicatePolicy::Latest,
            2,
            4,
            10_000,
            0.0,
            vec![],
        )
        .expect("spec is valid"),
    )
    .expect("task is valid");
    let route = DataProcessingRoute::new(
        load_only_task,
        commissioned_binding(true),
        processor.clone(),
        5_000,
    )
    .expect("route is valid");
    let application = DataProcessingApplication::new(
        vec![route],
        history,
        None,
        live,
        audit,
        Arc::new(ManualClock::new(TimestampMs::new(3_200))),
        SafetyPolicy,
    )
    .expect("application is valid");

    application
        .process(&context("data_processing.run", false), process_request())
        .await
        .expect("processing succeeds");

    let requests = processor.requests.lock().expect("test lock");
    let frame = requests[0].frame();
    assert_eq!(
        frame.history().timestamps(),
        &[TimestampMs::new(2_000), TimestampMs::new(3_000)]
    );
    assert_eq!(
        frame.history().series()[0].values()[1].as_number(),
        Some(12.0)
    );
    assert!(frame.quality().live_tail_included());
    assert_eq!(frame.quality().input_watermark(), TimestampMs::new(3_000));
    assert_eq!(
        frame.provenance()[0].source_kind(),
        SourceKind::HistoryAndLive
    );
}

#[tokio::test]
async fn a_live_sample_off_the_task_grid_is_not_retimestamped_or_merged() {
    let processor = Arc::new(RecordingProcessor::new(
        DataBoundary::Local,
        ProcessorBehavior::Produced,
    ));
    let history = Arc::new(RecordingHistory::default());
    let live = Arc::new(OneLiveState {
        sample: PointSample::new(
            load_address(),
            12.0,
            TimestampMs::new(2_900),
            aether_domain::PointQuality::Good,
        ),
    });
    let load_only_task = DataProcessingTask::forecast(
        task_identity(),
        CONTRACT,
        vec![history_feature()],
        ForecastTaskSpec::new(
            ForecastTarget::new("load", "kW", "positive_consumption").expect("target is valid"),
            1_000,
            HistoryAggregation::Last,
            HistoryDuplicatePolicy::Latest,
            2,
            4,
            10_000,
            0.0,
            vec![],
        )
        .expect("spec is valid"),
    )
    .expect("task is valid");
    let application = DataProcessingApplication::new(
        vec![
            DataProcessingRoute::new(
                load_only_task,
                commissioned_binding(true),
                processor.clone(),
                5_000,
            )
            .expect("route is valid"),
        ],
        history,
        None,
        live,
        Arc::new(RecordingAudit::default()),
        Arc::new(ManualClock::new(TimestampMs::new(3_200))),
        SafetyPolicy,
    )
    .expect("application is valid");

    application
        .process(&context("data_processing.run", false), process_request())
        .await
        .expect("stored history remains a complete safe frame");

    let requests = processor.requests.lock().expect("test lock");
    let frame = requests[0].frame();
    assert_eq!(
        frame.history().timestamps(),
        &[TimestampMs::new(2_000), TimestampMs::new(3_000)]
    );
    assert!(!frame.quality().live_tail_included());
    assert_eq!(frame.provenance()[0].source_kind(), SourceKind::History);
}
