use aether_data_processing::{
    DataProcessingRequestDto, DerivedDataDto, ProcessingResultDto, compute_input_digest,
    decode_request, decode_result, encode_derived_data, encode_request, encode_result,
};
use aether_domain::{DerivedData, TimestampMs};
use chrono::{DateTime, SecondsFormat, Utc};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

const REQUEST_FIXTURE: &[u8] = include_bytes!("fixtures/forecast-processing-request.json");
const RESULT_FIXTURE: &[u8] = include_bytes!("fixtures/forecast-processing-result.json");
const DERIVED_FIXTURE: &[u8] = include_bytes!("fixtures/forecast-derived-data.json");
const PYTHON_RFC8785_DIGEST: &str =
    "sha256:66aade139532bcd80b8ae85f5e6b120332e8b29128df82546440f209064193dc";

fn refresh_digest(request: &mut Value) {
    let basis = json!({
        "task": request["task"].clone(),
        "binding": request["binding"].clone(),
        "processor_contract": request["processor_contract"].clone(),
        "artifact": request.get("artifact").cloned().unwrap_or(Value::Null),
        "frame": request["frame"].clone(),
        "options": request["options"].clone()
    });
    let canonical = serde_json_canonicalizer::to_vec(&basis).unwrap();
    request["input_digest"] = json!(format!("sha256:{:x}", Sha256::digest(canonical)));
}

fn assert_request_rejected(mut request: Value) {
    refresh_digest(&mut request);
    assert!(decode_request(&serde_json::to_vec(&request).unwrap()).is_err());
}

fn utc_seconds(seconds: i64) -> String {
    DateTime::<Utc>::from_timestamp(seconds, 0)
        .unwrap()
        .to_rfc3339_opts(SecondsFormat::Secs, true)
}

fn timestamp(value: &str) -> TimestampMs {
    let milliseconds = DateTime::parse_from_rfc3339(value)
        .expect("fixture timestamp must be RFC 3339")
        .timestamp_millis();
    TimestampMs::new(u64::try_from(milliseconds).expect("fixture timestamp must be positive"))
}

fn fixture_derived_data() -> DerivedData {
    let request = decode_request(REQUEST_FIXTURE).expect("request fixture must decode");
    let result = decode_result(RESULT_FIXTURE).expect("result fixture must decode");
    DerivedData::accept(
        "0190aee6-22ac-72da-b214-629a31ccb99c",
        timestamp("2026-07-11T12:00:03Z"),
        request.frame().quality().clone(),
        result,
    )
    .expect("validated fixture result must be accepted")
}

#[test]
fn accepted_derived_data_encodes_as_the_complete_contract_golden() {
    let derived = fixture_derived_data();

    let dto = DerivedDataDto::try_from(&derived).expect("accepted data must encode as a DTO");
    let dto_json = serde_json::to_value(&dto).expect("DTO must serialize");
    let encoded_json: Value =
        serde_json::from_slice(&encode_derived_data(&derived).expect("accepted data must encode"))
            .expect("encoded data must be JSON");
    let expected_json: Value = serde_json::from_slice(DERIVED_FIXTURE).unwrap();

    assert_eq!(dto_json, encoded_json);
    assert_eq!(
        serde_json_canonicalizer::to_vec(&encoded_json).unwrap(),
        serde_json_canonicalizer::to_vec(&expected_json).unwrap()
    );
    assert_eq!(
        encoded_json["processor"]["contract"],
        "aether.data-processing.forecast.v1"
    );
    assert_eq!(encoded_json["warnings"], json!([]));
    assert_eq!(
        encoded_json["quality"],
        json!({
            "input_watermark": "2026-07-11T12:00:00Z",
            "missing_ratio": 0.0,
            "max_gap_seconds": 900,
            "live_tail_included": false,
            "substituted_samples": 0,
            "fallback_used": false
        })
    );
}

#[test]
fn fallback_derived_data_retains_fallback_metadata_warnings_and_accepted_quality() {
    let request = decode_request(REQUEST_FIXTURE).expect("request fixture must decode");
    let mut result_json: Value = serde_json::from_slice(RESULT_FIXTURE).unwrap();
    result_json["status"] = json!("fallback");
    result_json["fallback"] = json!({
        "strategy": "persistence",
        "strategy_version": "1",
        "reason_code": "MODEL_UNAVAILABLE",
        "source_feature": "signal",
        "based_on_data_through": "2026-07-11T11:45:00Z"
    });
    result_json["warnings"] = json!(["MODEL_FALLBACK"]);
    let result = decode_result(&serde_json::to_vec(&result_json).unwrap())
        .expect("fallback result must decode");
    let derived = DerivedData::accept(
        "0190aee6-22ac-72da-b214-629a31ccb99d",
        timestamp("2026-07-11T12:00:03Z"),
        request.frame().quality().clone(),
        result,
    )
    .expect("fallback must be accepted");

    let encoded: Value = serde_json::from_slice(
        &encode_derived_data(&derived).expect("fallback derived data must encode"),
    )
    .unwrap();

    assert_eq!(encoded["processing_status"], "fallback");
    assert_eq!(encoded["fallback"], result_json["fallback"]);
    assert_eq!(encoded["warnings"], json!(["MODEL_FALLBACK"]));
    assert_eq!(encoded["quality"]["fallback_used"], true);
    assert_eq!(encoded["quality"]["max_gap_seconds"], 900);
}

#[test]
fn derived_encoder_rejects_non_wire_ids_and_impossible_acceptance_order() {
    let request = decode_request(REQUEST_FIXTURE).expect("request fixture must decode");
    let result = decode_result(RESULT_FIXTURE).expect("result fixture must decode");
    let invalid_id = DerivedData::accept(
        "not-a-uuid",
        timestamp("2026-07-11T12:00:03Z"),
        request.frame().quality().clone(),
        result.clone(),
    )
    .expect("the domain intentionally leaves wire identity policy to the codec");
    assert!(encode_derived_data(&invalid_id).is_err());

    let accepted_before_processing = DerivedData::accept(
        "0190aee6-22ac-72da-b214-629a31ccb99e",
        timestamp("2026-07-11T12:00:01Z"),
        request.frame().quality().clone(),
        result,
    )
    .expect("the wire codec owns acceptance timestamp ordering");
    assert!(encode_derived_data(&accepted_before_processing).is_err());
}

#[test]
fn request_fixture_round_trips_between_json_dto_and_domain() {
    let request = decode_request(REQUEST_FIXTURE).expect("fixture must decode");

    assert_eq!(request.task().id(), "example.signal-forecast");
    assert_eq!(request.binding().id(), "example.edge-asset");
    assert_eq!(request.frame().history().sample_count(), 4);
    assert_eq!(
        request.frame().future_covariates().unwrap().sample_count(),
        2
    );

    let dto = DataProcessingRequestDto::try_from(&request).expect("domain must encode as DTO");
    let domain_again = dto.into_domain();
    assert_eq!(domain_again, request);

    let actual_json: Value =
        serde_json::from_slice(&encode_request(&request).expect("request must encode"))
            .expect("encoded request must be JSON");
    let expected_json: Value = serde_json::from_slice(REQUEST_FIXTURE).unwrap();
    assert_eq!(
        serde_json_canonicalizer::to_vec(&actual_json).unwrap(),
        serde_json_canonicalizer::to_vec(&expected_json).unwrap()
    );
}

#[test]
fn digest_matches_python_rfc8785_reference() {
    // PYTHON_RFC8785_DIGEST was independently generated with:
    // uv run --with rfc8785 python -c '<read fixture; rfc8785.dumps; sha256>'
    let request = decode_request(REQUEST_FIXTURE).expect("fixture must decode");
    let digest = compute_input_digest(
        request.task(),
        request.binding(),
        request.processor_contract(),
        request.artifact_selector(),
        request.frame(),
        request.options(),
    )
    .expect("digest basis must canonicalize");

    assert_eq!(digest, PYTHON_RFC8785_DIGEST);
    assert_eq!(request.input_digest(), PYTHON_RFC8785_DIGEST);
}

#[test]
fn result_fixture_round_trips_between_json_dto_and_domain() {
    let result = decode_result(RESULT_FIXTURE).expect("fixture must decode");

    assert_eq!(result.processor().id(), "example-forecast-processor");
    assert!(result.warnings().is_empty());

    let dto = ProcessingResultDto::try_from(&result).expect("domain must encode as DTO");
    let domain_again = dto.into_domain();
    assert_eq!(domain_again, result);

    let actual_json: Value =
        serde_json::from_slice(&encode_result(&result).expect("result must encode")).unwrap();
    let expected_json: Value = serde_json::from_slice(RESULT_FIXTURE).unwrap();
    assert_eq!(
        serde_json_canonicalizer::to_vec(&actual_json).unwrap(),
        serde_json_canonicalizer::to_vec(&expected_json).unwrap()
    );
}

#[test]
fn decoder_rejects_unknown_schemas_and_unknown_fields() {
    let mut request: Value = serde_json::from_slice(REQUEST_FIXTURE).unwrap();
    request["schema"] = json!("aether.data-processing.request.v2");
    assert!(decode_request(&serde_json::to_vec(&request).unwrap()).is_err());

    let mut request: Value = serde_json::from_slice(REQUEST_FIXTURE).unwrap();
    request["callback"] = json!("http://aether.internal/shm");
    assert!(decode_request(&serde_json::to_vec(&request).unwrap()).is_err());

    let mut result: Value = serde_json::from_slice(RESULT_FIXTURE).unwrap();
    result["schema"] = json!("aether.data-processing.result.v2");
    assert!(decode_result(&serde_json::to_vec(&result).unwrap()).is_err());
}

#[test]
fn decoder_rejects_digest_mismatch_and_invalid_identifiers() {
    let mut request: Value = serde_json::from_slice(REQUEST_FIXTURE).unwrap();
    request["input_digest"] = json!(format!("sha256:{}", "0".repeat(64)));
    assert!(decode_request(&serde_json::to_vec(&request).unwrap()).is_err());

    let mut request: Value = serde_json::from_slice(REQUEST_FIXTURE).unwrap();
    request["request_id"] = json!("not-a-uuid");
    assert!(decode_request(&serde_json::to_vec(&request).unwrap()).is_err());
}

#[test]
fn decoder_rejects_non_utc_and_out_of_range_timestamps() {
    let mut request: Value = serde_json::from_slice(REQUEST_FIXTURE).unwrap();
    request["submitted_at"] = json!("2026-07-11T12:00:01+00:00");
    assert!(decode_request(&serde_json::to_vec(&request).unwrap()).is_err());

    let mut request: Value = serde_json::from_slice(REQUEST_FIXTURE).unwrap();
    request["deadline"] = json!("99999-07-11T12:00:06Z");
    assert!(decode_request(&serde_json::to_vec(&request).unwrap()).is_err());
}

#[test]
fn decoder_rejects_non_finite_numbers() {
    let raw = String::from_utf8(REQUEST_FIXTURE.to_vec())
        .unwrap()
        .replacen("818.0", "NaN", 1);
    assert!(decode_request(raw.as_bytes()).is_err());

    let raw = String::from_utf8(RESULT_FIXTURE.to_vec())
        .unwrap()
        .replacen("846.2", "1e9999", 1);
    assert!(decode_result(raw.as_bytes()).is_err());
}

#[test]
fn decoder_rejects_invalid_status_field_combinations() {
    let mut result: Value = serde_json::from_slice(RESULT_FIXTURE).unwrap();
    result["status"] = json!("unavailable");
    result["unavailable"] = json!({
        "reason_code": "MODEL_UNAVAILABLE",
        "retryable": true,
        "retry_after_seconds": 30
    });
    assert!(decode_result(&serde_json::to_vec(&result).unwrap()).is_err());

    let mut result: Value = serde_json::from_slice(RESULT_FIXTURE).unwrap();
    result["status"] = json!("fallback");
    assert!(decode_result(&serde_json::to_vec(&result).unwrap()).is_err());

    let mut result: Value = serde_json::from_slice(RESULT_FIXTURE).unwrap();
    result["fallback"] = Value::Null;
    assert!(decode_result(&serde_json::to_vec(&result).unwrap()).is_err());
}

#[test]
fn decoder_rejects_explicit_null_for_optional_fields_and_normalizes_digest_timestamps() {
    let mut request: Value = serde_json::from_slice(REQUEST_FIXTURE).unwrap();
    request["artifact"] = Value::Null;
    assert!(decode_request(&serde_json::to_vec(&request).unwrap()).is_err());

    let mut request: Value = serde_json::from_slice(REQUEST_FIXTURE).unwrap();
    request["frame"]["as_of"] = json!("2026-07-11T12:00:00.000Z");
    let decoded = decode_request(&serde_json::to_vec(&request).unwrap())
        .expect("lexically equivalent timestamp keeps the normalized digest");
    assert_eq!(decoded.input_digest(), PYTHON_RFC8785_DIGEST);
}

#[test]
fn decoder_requires_exactly_one_provenance_entry_per_feature_key() {
    let mut request: Value = serde_json::from_slice(REQUEST_FIXTURE).unwrap();
    let duplicate = json!({
        "segment": "history",
        "feature": "signal",
        "source_kind": "history",
        "source_ref": "example.asset.signal.archive",
        "watermark": "2026-07-11T11:45:00Z"
    });
    request["frame"]["provenance"]
        .as_array_mut()
        .unwrap()
        .push(duplicate);
    refresh_digest(&mut request);
    assert!(decode_request(&serde_json::to_vec(&request).unwrap()).is_err());

    let mut request: Value = serde_json::from_slice(REQUEST_FIXTURE).unwrap();
    request["frame"]["provenance"]
        .as_array_mut()
        .unwrap()
        .retain(|entry| {
            entry["segment"] != "future_covariates" || entry["feature"] != "quarter_hour"
        });
    refresh_digest(&mut request);
    assert!(decode_request(&serde_json::to_vec(&request).unwrap()).is_err());
}

#[test]
fn decoder_rejects_irregular_frame_cadence_with_a_valid_digest() {
    let mut request: Value = serde_json::from_slice(REQUEST_FIXTURE).unwrap();
    request["frame"]["history"]["timestamps"][1] = json!("2026-07-11T11:16:00Z");
    refresh_digest(&mut request);

    assert!(decode_request(&serde_json::to_vec(&request).unwrap()).is_err());
}

#[test]
fn decoder_enforces_interval_end_history_and_future_cutoff_boundaries() {
    let mut early_history: Value = serde_json::from_slice(REQUEST_FIXTURE).unwrap();
    early_history["frame"]["history"]["timestamps"] = json!([
        "2026-07-11T11:00:00Z",
        "2026-07-11T11:15:00Z",
        "2026-07-11T11:30:00Z",
        "2026-07-11T11:45:00Z"
    ]);
    refresh_digest(&mut early_history);
    assert!(decode_request(&serde_json::to_vec(&early_history).unwrap()).is_err());

    let mut late_future: Value = serde_json::from_slice(REQUEST_FIXTURE).unwrap();
    late_future["frame"]["future_covariates"]["timestamps"] =
        json!(["2026-07-11T12:30:00Z", "2026-07-11T12:45:00Z"]);
    refresh_digest(&mut late_future);
    assert!(decode_request(&serde_json::to_vec(&late_future).unwrap()).is_err());
}

#[test]
fn codec_round_trips_missing_aware_boundary_gaps_and_single_interval_history() {
    for missing_indices in [vec![0_usize], vec![1], vec![2, 3]] {
        let mut request: Value = serde_json::from_slice(REQUEST_FIXTURE).unwrap();
        for index in &missing_indices {
            request["frame"]["history"]["features"]["signal"]["values"][*index] = Value::Null;
            request["frame"]["history"]["features"]["signal"]["quality"][*index] = json!("missing");
        }
        request["frame"]["quality"]["missing_ratio"] = json!(missing_indices.len() as f64 / 28.0);
        request["frame"]["quality"]["max_gap_seconds"] = json!(1800);
        if missing_indices == [2, 3] {
            request["frame"]["provenance"][0]["watermark"] = json!("2026-07-11T11:30:00Z");
        }
        refresh_digest(&mut request);

        let decoded = decode_request(&serde_json::to_vec(&request).unwrap())
            .expect("missing-aware frame must decode");
        assert_eq!(decoded.frame().quality().max_gap_ms(), 1_800_000);
        let encoded: Value = serde_json::from_slice(
            &encode_request(&decoded).expect("missing-aware frame must encode"),
        )
        .unwrap();
        assert_eq!(encoded["frame"]["quality"]["max_gap_seconds"], 1800);
    }

    let mut single: Value = serde_json::from_slice(REQUEST_FIXTURE).unwrap();
    single["frame"]["history"]["timestamps"] = json!(["2026-07-11T12:00:00Z"]);
    for series in single["frame"]["history"]["features"]
        .as_object_mut()
        .unwrap()
        .values_mut()
    {
        let last_value = series["values"][3].clone();
        series["values"] = Value::Array(vec![last_value]);
        series["quality"] = json!(["good"]);
    }
    single["frame"]["quality"]["max_gap_seconds"] = json!(900);
    refresh_digest(&mut single);
    let decoded = decode_request(&serde_json::to_vec(&single).unwrap())
        .expect("one interval still has one cadence of observation gap");
    assert_eq!(decoded.frame().quality().max_gap_ms(), 900_000);
    encode_request(&decoded).expect("single-interval history must round trip");
}

#[test]
fn decoder_rejects_future_covariates_that_do_not_match_forecast_horizon() {
    let mut request: Value = serde_json::from_slice(REQUEST_FIXTURE).unwrap();
    request["frame"]["future_covariates"]["timestamps"]
        .as_array_mut()
        .unwrap()
        .pop();
    for series in request["frame"]["future_covariates"]["features"]
        .as_object_mut()
        .unwrap()
        .values_mut()
    {
        series["values"].as_array_mut().unwrap().pop();
        series["quality"].as_array_mut().unwrap().pop();
    }
    refresh_digest(&mut request);

    assert!(decode_request(&serde_json::to_vec(&request).unwrap()).is_err());
}

#[test]
fn omitted_quantiles_round_trip_but_explicit_empty_arrays_are_rejected() {
    let mut request: Value = serde_json::from_slice(REQUEST_FIXTURE).unwrap();
    request["options"]
        .as_object_mut()
        .unwrap()
        .remove("quantiles");
    refresh_digest(&mut request);
    let domain = decode_request(&serde_json::to_vec(&request).unwrap()).unwrap();
    let encoded: Value = serde_json::from_slice(&encode_request(&domain).unwrap()).unwrap();
    assert!(encoded["options"].get("quantiles").is_none());

    let mut request: Value = serde_json::from_slice(REQUEST_FIXTURE).unwrap();
    request["options"]["quantiles"] = json!([]);
    refresh_digest(&mut request);
    assert!(decode_request(&serde_json::to_vec(&request).unwrap()).is_err());

    let mut result: Value = serde_json::from_slice(RESULT_FIXTURE).unwrap();
    result["output"]["points"][0]["quantiles"] = json!([]);
    assert!(decode_result(&serde_json::to_vec(&result).unwrap()).is_err());
}

#[test]
fn decoder_enforces_frame_collection_and_cadence_limits() {
    let mut request: Value = serde_json::from_slice(REQUEST_FIXTURE).unwrap();
    request["frame"]["cadence_seconds"] = json!(86_401);
    assert_request_rejected(request);

    let mut request: Value = serde_json::from_slice(REQUEST_FIXTURE).unwrap();
    let history = &mut request["frame"]["history"];
    let as_of_seconds = DateTime::parse_from_rfc3339("2026-07-11T12:00:00Z")
        .unwrap()
        .timestamp();
    let sample_count = 20_001_usize;
    history["timestamps"] = Value::Array(
        (0..sample_count)
            .map(|index| {
                let offset = i64::try_from(sample_count - index).unwrap() * 900;
                Value::String(utc_seconds(as_of_seconds - offset))
            })
            .collect(),
    );
    for series in history["features"].as_object_mut().unwrap().values_mut() {
        let value = series["values"][0].clone();
        series["values"] = Value::Array(vec![value; sample_count]);
        series["quality"] = Value::Array(vec![json!("good"); sample_count]);
    }
    assert_request_rejected(request);

    let mut request: Value = serde_json::from_slice(REQUEST_FIXTURE).unwrap();
    for index in 0..124 {
        let feature = format!("extra_{index:03}");
        request["frame"]["history"]["features"][&feature] = json!({
            "value_type": "number",
            "unit": "1",
            "values": [1.0, 1.0, 1.0, 1.0],
            "quality": ["good", "good", "good", "good"]
        });
        request["frame"]["provenance"]
            .as_array_mut()
            .unwrap()
            .push(json!({
                "segment": "history",
                "feature": feature,
                "source_kind": "history",
                "watermark": "2026-07-11T11:45:00Z"
            }));
    }
    assert_request_rejected(request);

    let mut request: Value = serde_json::from_slice(REQUEST_FIXTURE).unwrap();
    for index in 0..129 {
        let feature = format!("static_{index:03}");
        request["frame"]["static_features"][&feature] = json!({
            "value_type": "number",
            "unit": "1",
            "value": 1.0,
            "quality": "good"
        });
        request["frame"]["provenance"]
            .as_array_mut()
            .unwrap()
            .push(json!({
                "segment": "static_features",
                "feature": feature,
                "source_kind": "constant",
                "watermark": "2026-07-11T12:00:00Z"
            }));
    }
    assert_request_rejected(request);

    let mut request: Value = serde_json::from_slice(REQUEST_FIXTURE).unwrap();
    let first = request["frame"]["provenance"][0].clone();
    while request["frame"]["provenance"].as_array().unwrap().len() <= 512 {
        request["frame"]["provenance"]
            .as_array_mut()
            .unwrap()
            .push(first.clone());
    }
    assert_request_rejected(request);
}

#[test]
fn decoder_enforces_forecast_collection_limits() {
    let mut request: Value = serde_json::from_slice(REQUEST_FIXTURE).unwrap();
    request["options"]["quantiles"] = Value::Array(
        (1..=20)
            .map(|value| json!(f64::from(value) / 21.0))
            .collect(),
    );
    assert_request_rejected(request);

    let mut request: Value = serde_json::from_slice(REQUEST_FIXTURE).unwrap();
    request["options"]["horizon_steps"] = json!(4_097);
    assert_request_rejected(request);

    let mut result: Value = serde_json::from_slice(RESULT_FIXTURE).unwrap();
    let start = DateTime::parse_from_rfc3339("2026-07-11T12:15:00Z")
        .unwrap()
        .timestamp();
    result["output"]["points"] = Value::Array(
        (0..4_097)
            .map(|index| {
                json!({
                    "timestamp": utc_seconds(start + i64::from(index) * 900),
                    "value": 1.0
                })
            })
            .collect(),
    );
    assert!(decode_result(&serde_json::to_vec(&result).unwrap()).is_err());

    let mut result: Value = serde_json::from_slice(RESULT_FIXTURE).unwrap();
    result["warnings"] = Value::Array(
        (0..65)
            .map(|index| Value::String(format!("WARNING_{index}")))
            .collect(),
    );
    assert!(decode_result(&serde_json::to_vec(&result).unwrap()).is_err());

    let mut result: Value = serde_json::from_slice(RESULT_FIXTURE).unwrap();
    result["output"]["points"][0]["quantiles"] = Value::Array(
        (1..=20)
            .map(|value| {
                json!({
                    "probability": f64::from(value) / 21.0,
                    "value": f64::from(value)
                })
            })
            .collect(),
    );
    assert!(decode_result(&serde_json::to_vec(&result).unwrap()).is_err());

    let mut result: Value = serde_json::from_slice(RESULT_FIXTURE).unwrap();
    result["output"]["cadence_seconds"] = json!(86_401);
    assert!(decode_result(&serde_json::to_vec(&result).unwrap()).is_err());
}

#[test]
fn decoder_enforces_identifier_unit_and_source_reference_bounds() {
    let mut request: Value = serde_json::from_slice(REQUEST_FIXTURE).unwrap();
    request["task"]["id"] = json!("a".repeat(257));
    assert_request_rejected(request);

    let mut request: Value = serde_json::from_slice(REQUEST_FIXTURE).unwrap();
    request["frame"]["history"]["features"]["signal"]["unit"] = json!("u".repeat(65));
    assert_request_rejected(request);

    let mut request: Value = serde_json::from_slice(REQUEST_FIXTURE).unwrap();
    request["frame"]["provenance"][0]["source_ref"] = json!("s".repeat(2_049));
    assert_request_rejected(request);
}

#[test]
fn calendar_and_constant_sources_do_not_raise_the_actual_input_watermark() {
    let mut wire: Value = serde_json::from_slice(REQUEST_FIXTURE).unwrap();
    for source in wire["frame"]["provenance"]
        .as_array_mut()
        .expect("provenance is an array")
    {
        if !matches!(
            source["source_kind"].as_str(),
            Some("calendar" | "constant")
        ) {
            source["watermark"] = json!("2026-07-11T11:50:00Z");
        }
    }
    wire["frame"]["quality"]["input_watermark"] = json!("2026-07-11T11:50:00Z");
    refresh_digest(&mut wire);
    let request = decode_request(&serde_json::to_vec(&wire).unwrap()).unwrap();
    assert_eq!(
        request.frame().quality().input_watermark().get(),
        1_783_770_600_000
    );

    let mut invalid = wire;
    invalid["frame"]["quality"]["input_watermark"] = json!("2026-07-11T12:00:00Z");
    assert_request_rejected(invalid);
}
