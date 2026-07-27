use std::time::{Duration, SystemTime, UNIX_EPOCH};

use aether_data_processing::compute_input_digest;
use aether_domain::{
    ArtifactSelector, BindingIdentity, DataProcessingRequest, FeatureDefinition, FeatureRole,
    FeatureValue, FeatureValueType, ForecastOptions, ProcessingFrame, ProcessingOptions,
    ProcessingOutput, ProcessingStatus, SampleQuality, Segment, SegmentKind, Series, SourceKind,
    SourceProvenance, StaticFeature, TaskIdentity, TaskKind, TimestampMs,
};
use aether_http_data_processor::{
    BearerSecret, HttpDataProcessor, HttpDataProcessorConfig, JSON_MEDIA_TYPE,
};
use aether_ports::{
    DataBoundary, DataProcessor, DataProcessorDescriptor, PortErrorKind, ProcessorHealth,
};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const CONTRACT: &str = "aether.data-processing.forecast.v1";
const REQUEST_ID: &str = "0190aee6-2139-7a87-8448-806f1b843201";
const INPUT_DIGEST: &str =
    "sha256:8b227777d4dd1fc61c6f884f48641d02b50a8a461a77f8fae7f48e32fbd8c372";

fn unix_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("the test clock follows the Unix epoch")
        .as_millis()
        .try_into()
        .expect("the current timestamp fits u64")
}

fn timestamp_text(milliseconds: u64) -> String {
    let milliseconds = i64::try_from(milliseconds).expect("test timestamp fits i64");
    chrono::DateTime::<chrono::Utc>::from_timestamp_millis(milliseconds)
        .expect("test timestamp is representable")
        .to_rfc3339_opts(chrono::SecondsFormat::AutoSi, true)
}

fn number_series(
    name: &str,
    role: FeatureRole,
    unit: &str,
    values: Vec<FeatureValue>,
    quality: Vec<SampleQuality>,
) -> Series {
    Series::new(
        FeatureDefinition::numeric(name, role, unit).expect("numeric feature is valid"),
        values,
        quality,
    )
    .expect("series is valid")
}

fn request() -> DataProcessingRequest {
    let now = unix_millis();
    let as_of = now - 60_000;
    let history = Segment::new(
        vec![TimestampMs::new(as_of - 900_000), TimestampMs::new(as_of)],
        vec![
            number_series(
                "load",
                FeatureRole::History,
                "kW",
                vec![
                    FeatureValue::number(820.0).expect("finite value"),
                    FeatureValue::number(835.0).expect("finite value"),
                ],
                vec![SampleQuality::Good, SampleQuality::Good],
            ),
            Series::new(
                FeatureDefinition::new("mode", FeatureRole::History, FeatureValueType::Text)
                    .expect("text feature"),
                vec![FeatureValue::text("grid"), FeatureValue::text("island")],
                vec![SampleQuality::Good, SampleQuality::Uncertain],
            )
            .expect("text series"),
            Series::new(
                FeatureDefinition::new("holiday", FeatureRole::History, FeatureValueType::Boolean)
                    .expect("boolean feature"),
                vec![FeatureValue::boolean(false), FeatureValue::boolean(true)],
                vec![SampleQuality::Good, SampleQuality::Substituted],
            )
            .expect("boolean series"),
            number_series(
                "optional_sensor",
                FeatureRole::History,
                "kW",
                vec![
                    FeatureValue::number(2.5).expect("finite value"),
                    FeatureValue::missing(),
                ],
                vec![SampleQuality::Good, SampleQuality::Missing],
            ),
        ],
    )
    .expect("history is valid");
    let future = Segment::new(
        vec![
            TimestampMs::new(as_of + 900_000),
            TimestampMs::new(as_of + 1_800_000),
        ],
        vec![number_series(
            "temp_avg",
            FeatureRole::FutureCovariate,
            "Cel",
            vec![
                FeatureValue::number(32.1).expect("finite value"),
                FeatureValue::number(32.0).expect("finite value"),
            ],
            vec![SampleQuality::Good, SampleQuality::Good],
        )],
    )
    .expect("future covariates are valid");
    let static_features = vec![
        StaticFeature::new(
            FeatureDefinition::numeric("rated_power", FeatureRole::Static, "kW")
                .expect("numeric static feature"),
            FeatureValue::number(2_500.0).expect("finite value"),
            SampleQuality::Good,
        )
        .expect("static number"),
        StaticFeature::new(
            FeatureDefinition::new("tariff", FeatureRole::Static, FeatureValueType::Text)
                .expect("text static feature"),
            FeatureValue::text("tou"),
            SampleQuality::Good,
        )
        .expect("static text"),
        StaticFeature::new(
            FeatureDefinition::new("enabled", FeatureRole::Static, FeatureValueType::Boolean)
                .expect("boolean static feature"),
            FeatureValue::boolean(true),
            SampleQuality::Good,
        )
        .expect("static boolean"),
    ];
    let provenance = vec![
        SourceProvenance::new(
            SegmentKind::History,
            "load",
            SourceKind::HistoryAndLive,
            Some("energy.site.load.active_power"),
            TimestampMs::new(as_of),
        )
        .expect("history provenance"),
        SourceProvenance::new(
            SegmentKind::History,
            "mode",
            SourceKind::History,
            Some("site.operating_mode"),
            TimestampMs::new(as_of),
        )
        .expect("mode provenance"),
        SourceProvenance::new(
            SegmentKind::History,
            "holiday",
            SourceKind::Calendar,
            Some("calendar.holiday"),
            TimestampMs::new(as_of),
        )
        .expect("holiday provenance"),
        SourceProvenance::new(
            SegmentKind::History,
            "optional_sensor",
            SourceKind::History,
            Some("site.optional_sensor"),
            TimestampMs::new(as_of),
        )
        .expect("optional sensor provenance"),
        SourceProvenance::new(
            SegmentKind::FutureCovariates,
            "temp_avg",
            SourceKind::Covariate,
            Some("weather.nwp.air_temperature"),
            TimestampMs::new(as_of - 10_000),
        )
        .expect("covariate provenance")
        .with_issued_at(TimestampMs::new(as_of - 20_000))
        .expect("issue cut precedes watermark"),
        SourceProvenance::new(
            SegmentKind::StaticFeatures,
            "rated_power",
            SourceKind::Constant,
            None,
            TimestampMs::new(as_of - 1),
        )
        .expect("static provenance"),
        SourceProvenance::new(
            SegmentKind::StaticFeatures,
            "tariff",
            SourceKind::Constant,
            None,
            TimestampMs::new(as_of),
        )
        .expect("tariff provenance"),
        SourceProvenance::new(
            SegmentKind::StaticFeatures,
            "enabled",
            SourceKind::Constant,
            None,
            TimestampMs::new(as_of),
        )
        .expect("enabled provenance"),
    ];
    let frame = ProcessingFrame::new(
        TimestampMs::new(as_of),
        900_000,
        history,
        Some(future),
        static_features,
        aether_domain::FrameQuality::new(TimestampMs::new(as_of), 0.125, 900_000, true, 1)
            .expect("frame quality"),
        provenance,
    )
    .expect("processing frame");

    let task = TaskIdentity::new("energy.site-load-forecast", 1).expect("task identity");
    let binding = BindingIdentity::new("site-a", 7).expect("binding identity");
    let artifact =
        ArtifactSelector::new("model", "site-load", Some("v3")).expect("artifact selector");
    let options = ProcessingOptions::Forecast(
        ForecastOptions::new(2, vec![0.1, 0.5]).expect("forecast options"),
    );
    let input_digest =
        compute_input_digest(&task, &binding, CONTRACT, Some(&artifact), &frame, &options)
            .expect("request digest can be computed");
    DataProcessingRequest::new(
        REQUEST_ID,
        task,
        binding,
        frame,
        TimestampMs::new(now),
        TimestampMs::new(now + 10_000),
        CONTRACT,
        Some(artifact),
        input_digest,
        options,
    )
    .expect("processor request")
}

fn expired_request() -> DataProcessingRequest {
    let request = request();
    let submitted_at = request.frame().as_of().get() + 1_000;
    DataProcessingRequest::new(
        request.request_id(),
        request.task().clone(),
        request.binding().clone(),
        request.frame().clone(),
        TimestampMs::new(submitted_at),
        TimestampMs::new(submitted_at + 1_000),
        request.processor_contract(),
        request.artifact_selector().cloned(),
        request.input_digest(),
        request.options().clone(),
    )
    .expect("expired request is structurally valid")
}

fn descriptor(boundary: DataBoundary, max_request_bytes: usize) -> DataProcessorDescriptor {
    DataProcessorDescriptor::new(
        "load-forecasting-edge",
        "2.1.0",
        vec![TaskKind::Forecast],
        vec![CONTRACT.to_string()],
        boundary,
        100,
        max_request_bytes,
    )
    .expect("descriptor is valid")
}

fn config(
    server: &MockServer,
    boundary: DataBoundary,
    request_timeout: Duration,
    max_request_bytes: usize,
    max_response_bytes: usize,
) -> HttpDataProcessorConfig {
    HttpDataProcessorConfig::new(
        server.uri(),
        descriptor(boundary, max_request_bytes),
        Duration::from_millis(100),
        request_timeout,
        max_response_bytes,
    )
    .expect("HTTP processor configuration is valid")
}

fn result_json(status: &str) -> Value {
    let request = request();
    let as_of = request.frame().as_of().get();
    let watermark = request.frame().quality().input_watermark().get();
    let issued = unix_millis();
    let base = json!({
        "schema": "aether.data-processing.result.v1",
        "request_id": REQUEST_ID,
        "task": {
            "id": "energy.site-load-forecast",
            "revision": 1,
            "kind": "forecast"
        },
        "binding": {"id": "site-a", "revision": 7},
        "input_digest": INPUT_DIGEST,
        "status": status,
        "issued_at": timestamp_text(issued),
        "input_watermark": timestamp_text(watermark),
        "processor": {
            "id": "load-forecasting-edge",
            "version": "2.1.0",
            "contract": CONTRACT
        },
        "warnings": []
    });
    let mut result = base.as_object().expect("base is an object").clone();
    if status != "unavailable" {
        result.insert(
            "expires_at".to_string(),
            Value::String(timestamp_text(issued + 3_600_000)),
        );
        result.insert(
            "output".to_string(),
            json!({
                "schema": "aether.data-processing.output.forecast.v1",
                "kind": "forecast",
                "target": "load",
                "unit": "kW",
                "sign_convention": "positive_consumption",
                "cadence_seconds": 900,
                "timestamp_semantics": "interval_end",
                "points": [
                    {
                        "timestamp": timestamp_text(as_of + 900_000),
                        "value": 846.2,
                        "quantiles": [
                            {"probability": 0.1, "value": 812.0},
                            {"probability": 0.5, "value": 846.2}
                        ]
                    },
                    {
                        "timestamp": timestamp_text(as_of + 1_800_000),
                        "value": 852.7,
                        "quantiles": [
                            {"probability": 0.1, "value": 818.0},
                            {"probability": 0.5, "value": 852.7}
                        ]
                    }
                ]
            }),
        );
    }
    match status {
        "produced" => {
            result.insert(
                "artifact".to_string(),
                json!({
                    "kind": "model",
                    "family": "site-load",
                    "version": "v3",
                    "artifact_digest": "sha256:f04c532f2f814a3690f0f40e6f26fa82b0d69b9c510e7c0bb9f9f4de35b5a882"
                }),
            );
            result.insert("warnings".to_string(), json!(["CALIBRATED"]));
        },
        "fallback" => {
            result.insert(
                "fallback".to_string(),
                json!({
                    "strategy": "persistence",
                    "strategy_version": "1",
                    "reason_code": "MODEL_UNAVAILABLE",
                    "source_feature": "load",
                    "based_on_data_through": timestamp_text(watermark)
                }),
            );
            result.insert("warnings".to_string(), json!(["MODEL_FALLBACK_USED"]));
        },
        "unavailable" => {
            result.insert(
                "unavailable".to_string(),
                json!({
                    "reason_code": "INSUFFICIENT_HISTORY",
                    "retryable": true,
                    "retry_after_seconds": 900
                }),
            );
        },
        _ => panic!("test status must be known"),
    }
    Value::Object(result)
}

fn process_response(body: Value) -> ResponseTemplate {
    versioned_json_response(200, &body)
}

fn versioned_json_response(status: u16, body: &Value) -> ResponseTemplate {
    ResponseTemplate::new(status)
        .insert_header("content-type", JSON_MEDIA_TYPE)
        .set_body_bytes(serde_json::to_vec(&body).expect("response JSON encodes"))
}

fn error_response(
    status: u16,
    code: &str,
    category: &str,
    retryable: bool,
    retry_after_seconds: Option<u64>,
) -> ResponseTemplate {
    let mut body = json!({
        "schema": "aether.data-processing.error.v1",
        "request_id": REQUEST_ID,
        "code": code,
        "category": category,
        "message": "secret-token file:///private/model stack trace",
        "retryable": retryable,
        "details": {
            "path": "/private/model",
            "rule": "secret-token must never cross the adapter"
        }
    });
    if let Some(seconds) = retry_after_seconds {
        body["details"]["retry_after_seconds"] = json!(seconds);
    }
    versioned_json_response(status, &body)
}

#[tokio::test]
async fn process_sends_complete_v1_request_and_decodes_produced_result() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/process"))
        .and(header("content-type", JSON_MEDIA_TYPE))
        .and(header("accept", JSON_MEDIA_TYPE))
        .respond_with(process_response(result_json("produced")))
        .expect(1)
        .mount(&server)
        .await;
    let processor = HttpDataProcessor::new(config(
        &server,
        DataBoundary::Local,
        Duration::from_secs(2),
        64 * 1024,
        64 * 1024,
    ))
    .expect("client builds");

    let result = processor
        .process(request())
        .await
        .expect("request succeeds");

    assert_eq!(result.status(), ProcessingStatus::Produced);
    assert_eq!(result.processor().id(), "load-forecasting-edge");
    assert_eq!(result.artifact().expect("artifact").kind(), "model");
    assert_eq!(result.warnings(), ["CALIBRATED"]);
    let ProcessingOutput::Forecast(output) = result.output().expect("forecast output");
    assert_eq!(output.target(), "load");
    assert_eq!(output.points().len(), 2);
    assert_eq!(output.points()[0].quantiles()[0].probability(), 0.1);

    let received = server
        .received_requests()
        .await
        .expect("requests can be inspected");
    let payload: Value = serde_json::from_slice(&received[0].body).expect("request JSON");
    assert_eq!(payload["schema"], "aether.data-processing.request.v1");
    assert_eq!(payload["task"]["kind"], "forecast");
    assert_eq!(payload["binding"]["revision"], 7);
    assert_eq!(payload["artifact"]["family"], "site-load");
    assert_eq!(payload["frame"]["schema"], "aether.processing-frame.v1");
    assert_eq!(payload["frame"]["cadence_seconds"], 900);
    assert_eq!(
        payload["frame"]["history"]["features"]["mode"]["values"][0],
        "grid"
    );
    assert_eq!(
        payload["frame"]["history"]["features"]["holiday"]["values"][1],
        true
    );
    assert!(payload["frame"]["history"]["features"]["optional_sensor"]["values"][1].is_null());
    assert_eq!(
        payload["frame"]["static_features"]["tariff"]["value"],
        "tou"
    );
    let provenance = payload["frame"]["provenance"]
        .as_array()
        .expect("provenance array");
    let future_provenance = provenance
        .iter()
        .find(|entry| entry["feature"] == "temp_avg")
        .expect("future provenance");
    assert!(future_provenance["issued_at"].as_str().is_some());
    assert_eq!(payload["options"]["horizon_steps"], 2);
    assert_eq!(payload["options"]["quantiles"], json!([0.1, 0.5]));
}

#[tokio::test]
async fn process_decodes_explicit_fallback_and_unavailable_results() {
    for (status, expected) in [
        ("fallback", ProcessingStatus::Fallback),
        ("unavailable", ProcessingStatus::Unavailable),
    ] {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/process"))
            .respond_with(process_response(result_json(status)))
            .mount(&server)
            .await;
        let processor = HttpDataProcessor::new(config(
            &server,
            DataBoundary::Local,
            Duration::from_secs(2),
            64 * 1024,
            64 * 1024,
        ))
        .expect("client builds");

        let result = processor.process(request()).await.expect("result decodes");

        assert_eq!(result.status(), expected);
        if expected == ProcessingStatus::Fallback {
            assert_eq!(
                result.fallback().expect("fallback").strategy(),
                "persistence"
            );
        } else {
            let unavailable = result.unavailable().expect("unavailable metadata");
            assert!(unavailable.retryable());
            assert_eq!(unavailable.retry_after_ms(), Some(900_000));
        }
    }
}

#[tokio::test]
async fn health_uses_the_versioned_endpoint_and_validates_identity() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/health"))
        .and(header("accept", JSON_MEDIA_TYPE))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "status": "degraded",
            "processor": "load-forecasting-edge",
            "version": "2.1.0",
            "contract": CONTRACT
        })))
        .expect(1)
        .mount(&server)
        .await;
    let processor = HttpDataProcessor::new(config(
        &server,
        DataBoundary::Local,
        Duration::from_secs(2),
        64 * 1024,
        64 * 1024,
    ))
    .expect("client builds");

    assert_eq!(
        processor.health().await.expect("health succeeds"),
        ProcessorHealth::Degraded
    );
}

#[tokio::test]
async fn request_is_rejected_before_network_when_encoded_body_exceeds_descriptor_limit() {
    let server = MockServer::start().await;
    let processor = HttpDataProcessor::new(config(
        &server,
        DataBoundary::Local,
        Duration::from_secs(2),
        128,
        64 * 1024,
    ))
    .expect("client builds");

    let error = processor
        .process(request())
        .await
        .expect_err("large request is rejected");

    assert_eq!(error.kind(), PortErrorKind::Rejected);
    assert!(
        server
            .received_requests()
            .await
            .expect("request log")
            .is_empty()
    );
}

#[tokio::test]
async fn aggregate_cell_limit_counts_every_feature_not_only_timestamp_rows() {
    let server = MockServer::start().await;
    let tight_descriptor = DataProcessorDescriptor::new(
        "load-forecasting-edge",
        "2.1.0",
        vec![TaskKind::Forecast],
        vec![CONTRACT.to_string()],
        DataBoundary::Local,
        5,
        64 * 1024,
    )
    .expect("descriptor is valid");
    let config = HttpDataProcessorConfig::new(
        server.uri(),
        tight_descriptor,
        Duration::from_millis(100),
        Duration::from_secs(2),
        64 * 1024,
    )
    .expect("configuration is valid");
    let processor = HttpDataProcessor::new(config).expect("client builds");
    let request = request();
    assert_eq!(request.frame().sample_count(), 4);
    assert_eq!(request.frame().cell_count(), 13);

    let error = processor
        .process(request)
        .await
        .expect_err("all scalar cells count toward the frame limit");

    assert_eq!(error.kind(), PortErrorKind::Rejected);
    assert!(
        server
            .received_requests()
            .await
            .expect("request log")
            .is_empty()
    );
}

#[tokio::test]
async fn elapsed_absolute_deadline_is_rejected_before_network() {
    let server = MockServer::start().await;
    let processor = HttpDataProcessor::new(config(
        &server,
        DataBoundary::Local,
        Duration::from_secs(2),
        64 * 1024,
        64 * 1024,
    ))
    .expect("client builds");

    let error = processor
        .process(expired_request())
        .await
        .expect_err("elapsed deadline is rejected");

    assert_eq!(error.kind(), PortErrorKind::Timeout);
    assert!(
        server
            .received_requests()
            .await
            .expect("request log")
            .is_empty()
    );
}

#[tokio::test]
async fn timeout_and_http_statuses_have_stable_port_error_kinds_without_body_leakage() {
    let cases = [
        (
            400,
            "OPTION_UNKNOWN",
            "invalid_request",
            false,
            None,
            PortErrorKind::Rejected,
        ),
        (
            401,
            "PROCESSOR_AUTH_REQUIRED",
            "authorization",
            false,
            None,
            PortErrorKind::Permanent,
        ),
        (
            404,
            "MODEL_NOT_FOUND",
            "not_found",
            false,
            None,
            PortErrorKind::Permanent,
        ),
        (
            409,
            "REQUEST_ID_REUSED",
            "conflict",
            true,
            None,
            PortErrorKind::Conflict,
        ),
        (
            413,
            "FRAME_TOO_LARGE",
            "resource_limit",
            false,
            None,
            PortErrorKind::Rejected,
        ),
        (
            422,
            "FRAME_INVALID",
            "invalid_data",
            false,
            None,
            PortErrorKind::InvalidData,
        ),
        (
            429,
            "PROCESSOR_BUSY",
            "capacity",
            true,
            Some(7),
            PortErrorKind::Unavailable,
        ),
        (
            500,
            "PROCESSOR_INTERNAL",
            "internal",
            false,
            None,
            PortErrorKind::Permanent,
        ),
        (
            503,
            "MODEL_RUNTIME_UNAVAILABLE",
            "unavailable",
            true,
            Some(3),
            PortErrorKind::Unavailable,
        ),
        (
            504,
            "DEADLINE_EXCEEDED",
            "timeout",
            true,
            None,
            PortErrorKind::Timeout,
        ),
    ];
    for (status, code, category, retryable, retry_after_seconds, expected) in cases {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(error_response(
                status,
                code,
                category,
                retryable,
                retry_after_seconds,
            ))
            .mount(&server)
            .await;
        let processor = HttpDataProcessor::new(config(
            &server,
            DataBoundary::Local,
            Duration::from_secs(2),
            64 * 1024,
            64 * 1024,
        ))
        .expect("client builds");

        let error = processor
            .process(request())
            .await
            .expect_err("status is an error");

        assert_eq!(error.kind(), expected, "HTTP status {status}");
        assert!(error.message().contains(code));
        assert!(error.message().contains(category));
        assert!(error.message().contains(&format!("retryable={retryable}")));
        assert!(error.message().contains(REQUEST_ID));
        if let Some(seconds) = retry_after_seconds {
            assert!(
                error
                    .message()
                    .contains(&format!("retry_after_ms={}", seconds * 1_000))
            );
        }
        assert!(!error.message().contains("secret-token"));
        assert!(!error.message().contains("/private/model"));
        assert!(!error.message().contains(&server.uri()));
    }

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            process_response(result_json("produced")).set_delay(Duration::from_millis(250)),
        )
        .mount(&server)
        .await;
    let processor = HttpDataProcessor::new(config(
        &server,
        DataBoundary::Local,
        Duration::from_millis(30),
        64 * 1024,
        64 * 1024,
    ))
    .expect("client builds");
    let error = processor
        .process(request())
        .await
        .expect_err("request times out");
    assert_eq!(error.kind(), PortErrorKind::Timeout);
}

#[tokio::test]
async fn non_success_error_contract_is_strict_correlated_and_bounded() {
    let valid_error = json!({
        "schema": "aether.data-processing.error.v1",
        "request_id": REQUEST_ID,
        "code": "FRAME_INVALID",
        "category": "invalid_data",
        "message": "invalid frame",
        "retryable": false
    });
    let mut unknown_field = valid_error.clone();
    unknown_field["debug"] = json!("/private/model");
    let mut unknown_detail = valid_error.clone();
    unknown_detail["details"] = json!({"debug": "/private/model"});
    let mut explicit_null = valid_error.clone();
    explicit_null["request_id"] = Value::Null;
    let cases = vec![
        (
            ResponseTemplate::new(422)
                .insert_header("content-type", JSON_MEDIA_TYPE)
                .set_body_string("{not-json"),
            "malformed JSON",
        ),
        (
            ResponseTemplate::new(422)
                .insert_header("content-type", "application/json")
                .set_body_json(&unknown_field),
            "wrong media type",
        ),
        (
            versioned_json_response(422, &unknown_field),
            "unknown field",
        ),
        (
            versioned_json_response(422, &unknown_detail),
            "unknown details field",
        ),
        (
            versioned_json_response(422, &explicit_null),
            "explicit null",
        ),
        (
            ResponseTemplate::new(429)
                .insert_header(
                    "content-type",
                    "application/vnd.aether.data-processing+json;version=1;version=1",
                )
                .set_body_bytes(
                    serde_json::to_vec(&json!({
                        "schema": "aether.data-processing.error.v1",
                        "request_id": REQUEST_ID,
                        "code": "PROCESSOR_BUSY",
                        "category": "capacity",
                        "message": "processor is busy",
                        "retryable": true
                    }))
                    .expect("error JSON encodes"),
                ),
            "duplicate media type parameter",
        ),
    ];
    for (response, label) in cases {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(response)
            .mount(&server)
            .await;
        let processor = HttpDataProcessor::new(config(
            &server,
            DataBoundary::Local,
            Duration::from_secs(2),
            64 * 1024,
            64 * 1024,
        ))
        .expect("client builds");

        let error = processor.process(request()).await.expect_err(label);

        assert_eq!(error.kind(), PortErrorKind::InvalidData, "{label}");
        assert!(!error.message().contains("code="), "{label}");
        assert!(!error.message().contains("/private/model"));
    }

    let invalid_values = [
        ("schema", json!("aether.data-processing.error.v2")),
        ("code", json!("not_stable")),
        ("request_id", json!("not-a-uuid")),
        ("request_id", json!("0190aee6-2139-7a87-8448-806f1b843202")),
        ("category", json!("invalid_request")),
    ];
    for (field, value) in invalid_values {
        let mut invalid_error = valid_error.clone();
        invalid_error[field] = value;
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(versioned_json_response(422, &invalid_error))
            .mount(&server)
            .await;
        let processor = HttpDataProcessor::new(config(
            &server,
            DataBoundary::Local,
            Duration::from_secs(2),
            64 * 1024,
            64 * 1024,
        ))
        .expect("client builds");

        let error = processor
            .process(request())
            .await
            .expect_err("invalid error envelope is rejected");

        assert_eq!(error.kind(), PortErrorKind::InvalidData, "field {field}");
        assert!(!error.message().contains("code="), "field {field}");
    }

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(503)
                .insert_header("content-type", JSON_MEDIA_TYPE)
                .set_body_bytes(vec![b'x'; 4_097]),
        )
        .mount(&server)
        .await;
    let processor = HttpDataProcessor::new(config(
        &server,
        DataBoundary::Local,
        Duration::from_secs(2),
        64 * 1024,
        4_096,
    ))
    .expect("client builds");

    let error = processor
        .process(request())
        .await
        .expect_err("oversized error envelope is rejected");

    assert_eq!(error.kind(), PortErrorKind::InvalidData);
}

#[tokio::test]
async fn redirect_is_not_followed() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/process"))
        .respond_with(ResponseTemplate::new(302).insert_header("location", "/v1/redirect-target"))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(path("/v1/redirect-target"))
        .respond_with(process_response(result_json("produced")))
        .expect(0)
        .mount(&server)
        .await;
    let processor = HttpDataProcessor::new(config(
        &server,
        DataBoundary::Local,
        Duration::from_secs(2),
        64 * 1024,
        64 * 1024,
    ))
    .expect("client builds");

    let error = processor
        .process(request())
        .await
        .expect_err("redirect is rejected");

    assert_eq!(error.kind(), PortErrorKind::Permanent);
}

#[tokio::test]
async fn malformed_wrong_media_type_and_invalid_results_are_rejected() {
    let mut cases = vec![
        (
            ResponseTemplate::new(200)
                .insert_header("content-type", JSON_MEDIA_TYPE)
                .set_body_string("{not-json"),
            "malformed JSON",
        ),
        (
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/plain")
                .set_body_json(result_json("produced")),
            "wrong media type",
        ),
    ];
    let mut out_of_order = result_json("produced");
    let points = out_of_order["output"]["points"]
        .as_array_mut()
        .expect("points array");
    points.swap(0, 1);
    cases.push((process_response(out_of_order), "out-of-order timestamps"));

    let mut invalid_status = result_json("unavailable");
    invalid_status["expires_at"] = Value::String(timestamp_text(unix_millis() + 60_000));
    cases.push((process_response(invalid_status), "invalid status structure"));

    let mut unknown_field = result_json("produced");
    unknown_field["vendor_command"] = json!({"write": true});
    cases.push((process_response(unknown_field), "unknown field"));

    let mut wrong_number = result_json("produced");
    wrong_number["output"]["points"][0]["value"] = Value::String("NaN".to_string());
    cases.push((process_response(wrong_number), "non-finite substitute"));

    let raw_non_finite = serde_json::to_string(&result_json("produced"))
        .expect("result JSON")
        .replacen("846.2", "NaN", 1);
    cases.push((
        ResponseTemplate::new(200)
            .insert_header("content-type", JSON_MEDIA_TYPE)
            .set_body_string(raw_non_finite),
        "non-finite JSON number",
    ));

    for (response, label) in cases {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(response)
            .mount(&server)
            .await;
        let processor = HttpDataProcessor::new(config(
            &server,
            DataBoundary::Local,
            Duration::from_secs(2),
            64 * 1024,
            64 * 1024,
        ))
        .expect("client builds");

        let error = processor.process(request()).await.expect_err(label);

        assert_eq!(error.kind(), PortErrorKind::InvalidData, "{label}");
        assert!(!error.message().contains('{'));
    }
}

#[tokio::test]
async fn streaming_response_limit_is_enforced_without_content_length() {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("listener binds");
    let address = listener.local_addr().expect("listener address");
    let response_json = serde_json::to_vec(&result_json("produced")).expect("result JSON");
    let server_task = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.expect("connection accepted");
        let mut request_buffer = vec![0_u8; 16 * 1024];
        let _ = stream
            .read(&mut request_buffer)
            .await
            .expect("request can be read");
        stream
            .write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: {JSON_MEDIA_TYPE}\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n"
                )
                .as_bytes(),
            )
            .await
            .expect("headers are written");
        for chunk in response_json.chunks(64) {
            stream
                .write_all(format!("{:x}\r\n", chunk.len()).as_bytes())
                .await
                .expect("chunk size is written");
            stream.write_all(chunk).await.expect("chunk is written");
            stream
                .write_all(b"\r\n")
                .await
                .expect("chunk terminator is written");
        }
        stream
            .write_all(b"0\r\n\r\n")
            .await
            .expect("body terminates");
    });
    let endpoint = format!("http://{address}");
    let config = HttpDataProcessorConfig::new(
        endpoint,
        descriptor(DataBoundary::Local, 64 * 1024),
        Duration::from_millis(100),
        Duration::from_secs(2),
        128,
    )
    .expect("configuration is valid");
    let processor = HttpDataProcessor::new(config).expect("client builds");

    let error = processor
        .process(request())
        .await
        .expect_err("response is too large");

    assert_eq!(error.kind(), PortErrorKind::InvalidData);
    server_task.await.expect("server task completes");
}

#[tokio::test]
async fn content_length_response_limit_is_rejected_before_decoding() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", JSON_MEDIA_TYPE)
                .set_body_bytes(vec![b'x'; 4_097]),
        )
        .expect(1)
        .mount(&server)
        .await;
    let processor = HttpDataProcessor::new(config(
        &server,
        DataBoundary::Local,
        Duration::from_secs(2),
        64 * 1024,
        4_096,
    ))
    .expect("client builds");

    let error = processor
        .process(request())
        .await
        .expect_err("fixed-length response is too large");

    assert_eq!(error.kind(), PortErrorKind::InvalidData);
}

#[tokio::test]
async fn explicit_bearer_secret_is_sent_but_redacted_from_debug_output() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(header(
            "authorization",
            "Bearer highly-sensitive-token-0123456789abcdef",
        ))
        .respond_with(process_response(result_json("produced")))
        .expect(1)
        .mount(&server)
        .await;
    let secret =
        BearerSecret::new("highly-sensitive-token-0123456789abcdef").expect("secret is valid");
    assert!(!format!("{secret:?}").contains("highly-sensitive-token-0123456789abcdef"));
    let config = config(
        &server,
        DataBoundary::Local,
        Duration::from_secs(2),
        64 * 1024,
        64 * 1024,
    )
    .with_bearer_secret(secret);
    assert!(!format!("{config:?}").contains("highly-sensitive-token-0123456789abcdef"));
    let processor = HttpDataProcessor::new(config).expect("client builds");
    assert!(!format!("{processor:?}").contains("highly-sensitive-token-0123456789abcdef"));

    processor
        .process(request())
        .await
        .expect("authenticated request succeeds");
}

#[test]
fn endpoint_policy_rejects_remote_http_credentials_query_fragment_and_bad_limits() {
    let remote_http = HttpDataProcessorConfig::new(
        "http://processor.example",
        descriptor(DataBoundary::Remote, 1024),
        Duration::from_secs(1),
        Duration::from_secs(2),
        1024,
    )
    .expect_err("remote HTTP is forbidden");
    assert_eq!(remote_http.kind(), PortErrorKind::Permanent);

    let disguised_remote = HttpDataProcessorConfig::new(
        "https://processor.example",
        descriptor(DataBoundary::Local, 1024),
        Duration::from_secs(1),
        Duration::from_secs(2),
        1024,
    )
    .expect_err("a local boundary cannot target a remote HTTPS origin");
    assert_eq!(disguised_remote.kind(), PortErrorKind::Permanent);

    for endpoint in [
        "https://user:password@processor.example",
        "https://processor.example?site=a",
        "https://processor.example#fragment",
        "ftp://processor.example",
    ] {
        let error = HttpDataProcessorConfig::new(
            endpoint,
            descriptor(DataBoundary::Remote, 1024),
            Duration::from_secs(1),
            Duration::from_secs(2),
            1024,
        )
        .expect_err("unsafe endpoint is rejected");
        assert_eq!(error.kind(), PortErrorKind::Permanent);
        assert!(!error.message().contains("password"));
        assert!(!error.message().contains(endpoint));
    }

    let zero_timeout = HttpDataProcessorConfig::new(
        "http://127.0.0.1:8989",
        descriptor(DataBoundary::Local, 1024),
        Duration::ZERO,
        Duration::from_secs(2),
        1024,
    )
    .expect_err("zero connect timeout is rejected");
    assert_eq!(zero_timeout.kind(), PortErrorKind::Permanent);

    let zero_request_timeout = HttpDataProcessorConfig::new(
        "http://127.0.0.1:8989",
        descriptor(DataBoundary::Local, 1024),
        Duration::from_secs(1),
        Duration::ZERO,
        1024,
    )
    .expect_err("zero request timeout is rejected");
    assert_eq!(zero_request_timeout.kind(), PortErrorKind::Permanent);

    let zero_response_limit = HttpDataProcessorConfig::new(
        "http://127.0.0.1:8989",
        descriptor(DataBoundary::Local, 1024),
        Duration::from_secs(1),
        Duration::from_secs(2),
        0,
    )
    .expect_err("zero response limit is rejected");
    assert_eq!(zero_response_limit.kind(), PortErrorKind::Permanent);
}

#[test]
fn local_http_is_an_explicit_supported_boundary() {
    let config = HttpDataProcessorConfig::new(
        "http://127.0.0.1:8989",
        descriptor(DataBoundary::Local, 1024),
        Duration::from_secs(1),
        Duration::from_secs(2),
        1024,
    )
    .expect("local HTTP is explicitly allowed");

    assert_eq!(config.descriptor().data_boundary(), DataBoundary::Local);
    assert_eq!(config.max_response_bytes(), 1024);
}
