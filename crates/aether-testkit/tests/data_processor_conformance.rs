use aether_domain::{
    BindingIdentity, DataProcessingRequest, FeatureDefinition, FeatureRole, FeatureValue,
    ForecastOptions, ForecastOutput, ForecastPoint, FrameQuality, ProcessingFrame,
    ProcessingOptions, ProcessingOutput, ProcessingResult, ProcessingStatus, ProcessorProvenance,
    SampleQuality, Segment, SegmentKind, Series, SourceKind, SourceProvenance, TaskIdentity,
    TaskKind, TimestampMs, UnavailableInfo,
};
use aether_ports::{
    DataBoundary, DataProcessor, DataProcessorDescriptor, PortError, PortErrorKind, PortResult,
    ProcessorHealth,
};
use aether_testkit::{ScriptedDataProcessor, assert_data_processor_correlation};
use async_trait::async_trait;

const CONTRACT: &str = "aether.data-processing.forecast";

fn request() -> DataProcessingRequest {
    let definition =
        FeatureDefinition::numeric("load", FeatureRole::History, "kW").expect("feature is valid");
    let series = Series::new(
        definition,
        vec![FeatureValue::number(10.0).expect("value is finite")],
        vec![SampleQuality::Good],
    )
    .expect("series is valid");
    let provenance = SourceProvenance::new(
        SegmentKind::History,
        "load",
        SourceKind::History,
        Some("site.load"),
        TimestampMs::new(2_000),
    )
    .expect("source provenance is valid");
    let frame = ProcessingFrame::new(
        TimestampMs::new(2_000),
        1_000,
        Segment::new(vec![TimestampMs::new(2_000)], vec![series]).expect("segment is valid"),
        None,
        vec![],
        FrameQuality::new(TimestampMs::new(2_000), 0.0, 0, false, 0).expect("quality is valid"),
        vec![provenance],
    )
    .expect("frame is valid");
    DataProcessingRequest::new(
        "request-1",
        TaskIdentity::new("energy.site-load-forecast", 1).expect("task is valid"),
        BindingIdentity::new("site-a", 1).expect("binding is valid"),
        frame,
        TimestampMs::new(2_100),
        TimestampMs::new(3_000),
        CONTRACT,
        None,
        "sha256:input",
        ProcessingOptions::Forecast(ForecastOptions::new(1, vec![]).expect("options are valid")),
    )
    .expect("request is valid")
}

fn request_with_identity(request_id: &str, digest: &str) -> DataProcessingRequest {
    let mut request = request();
    let frame = request.frame().clone();
    request = DataProcessingRequest::new(
        request_id,
        request.task().clone(),
        request.binding().clone(),
        frame,
        request.submitted_at(),
        request.deadline(),
        request.processor_contract(),
        request.artifact_selector().cloned(),
        digest,
        request.options().clone(),
    )
    .expect("request identity override is valid");
    request
}

fn descriptor() -> DataProcessorDescriptor {
    DataProcessorDescriptor::new(
        "test-processor",
        "1.0.0",
        vec![TaskKind::Forecast],
        vec![CONTRACT.into()],
        DataBoundary::Local,
        16,
        4_096,
    )
    .expect("descriptor is valid")
}

fn produced_result(request: &DataProcessingRequest, digest: &str) -> ProcessingResult {
    let output = ForecastOutput::new(
        "load",
        "kW",
        "positive_consumption",
        1_000,
        vec![
            ForecastPoint::new(TimestampMs::new(3_000), 11.0, vec![])
                .expect("first forecast point is finite"),
            ForecastPoint::new(TimestampMs::new(4_000), 12.0, vec![])
                .expect("second forecast point is finite"),
        ],
    )
    .expect("forecast output is ordered");
    ProcessingResult::new(
        request.request_id(),
        request.task().clone(),
        request.binding().clone(),
        digest,
        ProcessingStatus::Produced,
        ProcessorProvenance::new("test-processor", "1.0.0", CONTRACT).expect("provenance is valid"),
        None,
        request.frame().quality().input_watermark(),
        TimestampMs::new(2_200),
        Some(TimestampMs::new(5_000)),
        Some(ProcessingOutput::Forecast(output)),
        None,
        None,
    )
    .expect("produced result is valid")
}

fn unavailable_result(request: &DataProcessingRequest) -> ProcessingResult {
    ProcessingResult::new(
        request.request_id(),
        request.task().clone(),
        request.binding().clone(),
        request.input_digest(),
        ProcessingStatus::Unavailable,
        ProcessorProvenance::new("test-processor", "1.0.0", CONTRACT).expect("provenance is valid"),
        None,
        request.frame().quality().input_watermark(),
        TimestampMs::new(2_200),
        None,
        None,
        None,
        Some(UnavailableInfo::new("NO_ARTIFACT", false, None).expect("unavailable info is valid")),
    )
    .expect("unavailable result is valid")
}

struct UnavailableProcessor {
    descriptor: DataProcessorDescriptor,
    corrupt_digest: bool,
}

impl UnavailableProcessor {
    fn new(corrupt_digest: bool) -> Self {
        Self {
            descriptor: DataProcessorDescriptor::new(
                "test-processor",
                "1.0.0",
                vec![TaskKind::Forecast],
                vec![CONTRACT.into()],
                DataBoundary::Local,
                16,
                4_096,
            )
            .expect("descriptor is valid"),
            corrupt_digest,
        }
    }
}

#[async_trait]
impl DataProcessor for UnavailableProcessor {
    fn descriptor(&self) -> &DataProcessorDescriptor {
        &self.descriptor
    }

    async fn health(&self) -> PortResult<ProcessorHealth> {
        Ok(ProcessorHealth::Healthy)
    }

    async fn process(&self, request: DataProcessingRequest) -> PortResult<ProcessingResult> {
        ProcessingResult::new(
            request.request_id(),
            request.task().clone(),
            request.binding().clone(),
            if self.corrupt_digest {
                "sha256:corrupt"
            } else {
                request.input_digest()
            },
            ProcessingStatus::Unavailable,
            ProcessorProvenance::new("test-processor", "1.0.0", CONTRACT)
                .expect("provenance is valid"),
            None,
            request.frame().quality().input_watermark(),
            TimestampMs::new(2_200),
            None,
            None,
            None,
            Some(
                UnavailableInfo::new("NO_ARTIFACT", false, None)
                    .expect("unavailable info is valid"),
            ),
        )
        .map_err(|error| PortError::new(PortErrorKind::InvalidData, error.to_string()))
    }
}

#[tokio::test]
async fn conformance_accepts_explicit_unavailable_with_exact_correlation() {
    let result = assert_data_processor_correlation(&UnavailableProcessor::new(false), request())
        .await
        .expect("processor conforms");

    assert_eq!(result.status(), ProcessingStatus::Unavailable);
    assert!(result.output().is_none());
}

#[tokio::test]
async fn conformance_rejects_a_processor_that_changes_the_input_identity() {
    let error = assert_data_processor_correlation(&UnavailableProcessor::new(true), request())
        .await
        .expect_err("digest corruption violates the port contract");

    assert_eq!(error.kind(), PortErrorKind::InvalidData);
}

#[tokio::test]
async fn scripted_processor_queues_results_and_errors_and_records_exact_requests() {
    let processor = ScriptedDataProcessor::new(descriptor());
    let first = request_with_identity("request-produced", "sha256:produced");
    let second = request_with_identity("request-timeout", "sha256:timeout");
    let first_result = produced_result(&first, first.input_digest());
    processor
        .enqueue_result(first_result.clone())
        .expect("result is queued");
    processor
        .enqueue_error(PortError::new(PortErrorKind::Timeout, "scripted timeout"))
        .expect("error is queued");
    processor
        .set_health(Ok(ProcessorHealth::Degraded))
        .expect("health is configured");

    assert_eq!(processor.descriptor(), &descriptor());
    assert_eq!(processor.health().await, Ok(ProcessorHealth::Degraded));
    assert_eq!(processor.process(first.clone()).await, Ok(first_result));
    assert_eq!(
        processor.process(second.clone()).await,
        Err(PortError::new(PortErrorKind::Timeout, "scripted timeout"))
    );
    assert_eq!(
        processor
            .requests()
            .expect("recorded requests are readable"),
        vec![first, second]
    );
    assert_eq!(
        processor
            .queued_outcomes()
            .expect("queue depth is readable"),
        0
    );
}

#[tokio::test]
async fn scripted_processor_reports_an_empty_queue_as_permanent_after_recording_the_request() {
    let processor = ScriptedDataProcessor::new(descriptor());
    let request = request_with_identity("request-empty", "sha256:empty");

    let error = processor
        .process(request.clone())
        .await
        .expect_err("an unconfigured script is a test composition failure");

    assert_eq!(error.kind(), PortErrorKind::Permanent);
    assert_eq!(
        error.message(),
        "scripted processor response queue is empty"
    );
    assert_eq!(
        processor
            .requests()
            .expect("recorded requests are readable"),
        vec![request]
    );
}

#[tokio::test]
async fn processor_conformance_covers_finite_ordered_output_and_unavailable_without_output() {
    let processor = ScriptedDataProcessor::new(descriptor());
    let produced_request = request_with_identity("request-produced", "sha256:produced");
    let unavailable_request = request_with_identity("request-unavailable", "sha256:unavailable");
    processor
        .enqueue_result(produced_result(
            &produced_request,
            produced_request.input_digest(),
        ))
        .expect("produced response is queued");
    processor
        .enqueue_result(unavailable_result(&unavailable_request))
        .expect("unavailable response is queued");

    let produced = assert_data_processor_correlation(&processor, produced_request)
        .await
        .expect("finite ordered output conforms");
    let unavailable = assert_data_processor_correlation(&processor, unavailable_request)
        .await
        .expect("explicit unavailable response conforms");

    let ProcessingOutput::Forecast(output) = produced.output().expect("output is present");
    assert!(
        output
            .points()
            .iter()
            .all(|point| point.value().is_finite())
    );
    assert!(
        output
            .points()
            .windows(2)
            .all(|pair| pair[0].timestamp() < pair[1].timestamp())
    );
    assert_eq!(unavailable.status(), ProcessingStatus::Unavailable);
    assert!(unavailable.output().is_none());
}

#[tokio::test]
async fn processor_conformance_rejects_a_non_echoed_digest_from_a_scripted_processor() {
    let processor = ScriptedDataProcessor::new(descriptor());
    let request = request_with_identity("request-corrupt", "sha256:expected");
    processor
        .enqueue_result(produced_result(&request, "sha256:wrong"))
        .expect("malformed response is queued");

    let error = assert_data_processor_correlation(&processor, request)
        .await
        .expect_err("digest is exact correlation data");

    assert_eq!(error.kind(), PortErrorKind::InvalidData);
}
