use std::collections::BTreeMap;
use std::fmt::Write as _;

use aether_domain::{
    ArtifactProvenance, ArtifactSelector, BindingIdentity, DataProcessingRequest,
    DataProcessingTask, DerivedData, DomainError, FallbackInfo, FeatureDefinition, FeatureRole,
    FeatureValue, FeatureValueType, ForecastOptions, ForecastOutput, ForecastPoint,
    ForecastQuantile, FrameQuality, ProcessingFrame, ProcessingOptions, ProcessingOutput,
    ProcessingResult, ProcessingStatus, ProcessorProvenance, SampleQuality, Segment, SegmentKind,
    Series, SourceKind, SourceProvenance, StaticFeature, TaskIdentity, TimestampMs,
    UnavailableInfo, maximum_observation_gap,
};
use chrono::{DateTime, SecondsFormat, Utc};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::dto::{
    AcceptedFrameQualityWire, AcceptedProcessingStatusWire, ArtifactProvenanceWire,
    ArtifactSelectorWire, BindingWire, DataProcessingRequestDto, DerivedDataDto, DerivedDataWire,
    DigestBasisWire, FallbackWire, FeatureValueTypeWire, ForecastOutputWire, ForecastPointWire,
    ForecastQuantileWire, FrameQualityWire, FrameWire, OptionsWire, ProcessingResultDto,
    ProcessingStatusWire, ProcessorWire, RequestWire, ResultWire, SampleQualityWire, ScalarWire,
    SegmentKindWire, SegmentWire, SeriesWire, SourceKindWire, SourceProvenanceWire,
    StaticFeatureWire, TaskKindWire, TaskWire, TimestampSemanticsWire, UnavailableWire,
};
use crate::{
    DERIVED_DATA_SCHEMA, FORECAST_OUTPUT_SCHEMA, FRAME_SCHEMA, REQUEST_SCHEMA, RESULT_SCHEMA,
};

const MAX_SEGMENT_SAMPLES: usize = 20_000;
const MAX_FEATURES_PER_SEGMENT: usize = 128;
const MAX_STATIC_FEATURES: usize = 128;
const MAX_PROVENANCE_ENTRIES: usize = 512;
const MAX_WARNINGS: usize = 64;
const MAX_FORECAST_POINTS: usize = 4_096;
const MAX_QUANTILES: usize = 19;
const MAX_CADENCE_SECONDS: u64 = 86_400;

/// Failure while converting or validating the Data Processing wire format.
#[derive(Debug, Error)]
pub enum CodecError {
    /// JSON syntax or strict DTO deserialization failed.
    #[error("invalid Data Processing JSON")]
    Json(#[source] serde_json::Error),
    /// A wire-only invariant was violated.
    #[error("invalid Data Processing contract: {0}")]
    Contract(&'static str),
    /// A domain constructor rejected the decoded value.
    #[error("invalid Data Processing domain value: {0}")]
    Domain(DomainError),
    /// RFC 8785 canonicalization failed.
    #[error("unable to canonicalize the Data Processing digest basis")]
    CanonicalJson(#[source] serde_json::Error),
}

impl From<DomainError> for CodecError {
    fn from(error: DomainError) -> Self {
        Self::Domain(error)
    }
}

/// Encodes a complete domain request as strict Data Processing JSON.
pub fn encode_request(request: &DataProcessingRequest) -> Result<Vec<u8>, CodecError> {
    let dto = DataProcessingRequestDto::try_from(request)?;
    serde_json::to_vec(&dto).map_err(CodecError::Json)
}

/// Decodes strict Data Processing JSON into a validated domain request.
pub fn decode_request(bytes: &[u8]) -> Result<DataProcessingRequest, CodecError> {
    serde_json::from_slice::<DataProcessingRequestDto>(bytes)
        .map(DataProcessingRequestDto::into_domain)
        .map_err(CodecError::Json)
}

/// Encodes an untrusted domain result as strict Data Processing JSON.
pub fn encode_result(result: &ProcessingResult) -> Result<Vec<u8>, CodecError> {
    let dto = ProcessingResultDto::try_from(result)?;
    serde_json::to_vec(&dto).map_err(CodecError::Json)
}

/// Decodes strict Data Processing JSON into a structurally valid result.
///
/// Correlation and commissioned task-policy validation remain the
/// application's responsibility.
pub fn decode_result(bytes: &[u8]) -> Result<ProcessingResult, CodecError> {
    serde_json::from_slice::<ProcessingResultDto>(bytes)
        .map(ProcessingResultDto::into_domain)
        .map_err(CodecError::Json)
}

/// Encodes Aether-accepted derived data as strict Data Processing JSON.
///
/// There is deliberately no inverse decoder: only Aether's application layer
/// may create [`DerivedData`] after accepting an untrusted processor result.
pub fn encode_derived_data(derived: &DerivedData) -> Result<Vec<u8>, CodecError> {
    let dto = DerivedDataDto::try_from(derived)?;
    serde_json::to_vec(&dto).map_err(CodecError::Json)
}

/// Computes the lowercase SHA-256 digest of the RFC 8785 canonical input basis.
pub fn compute_input_digest(
    task: &TaskIdentity,
    binding: &BindingIdentity,
    processor_contract: &str,
    artifact: Option<&ArtifactSelector>,
    frame: &ProcessingFrame,
    options: &ProcessingOptions,
) -> Result<String, CodecError> {
    validate_identifier(processor_contract)?;
    validate_request_horizon(frame, options)?;
    let task = task_to_wire(task)?;
    let binding = binding_to_wire(binding)?;
    let artifact = artifact_selector_to_wire(artifact)?;
    let frame = frame_to_wire(frame)?;
    let options = options_to_wire(options)?;
    let basis = DigestBasisWire {
        task: &task,
        binding: &binding,
        processor_contract,
        artifact: artifact.as_ref(),
        frame: &frame,
        options: &options,
    };
    let canonical = serde_json_canonicalizer::to_vec(&basis).map_err(CodecError::CanonicalJson)?;
    let digest = Sha256::digest(canonical);
    let mut encoded = String::with_capacity(71);
    encoded.push_str("sha256:");
    for byte in digest {
        write!(&mut encoded, "{byte:02x}")
            .map_err(|_| CodecError::Contract("digest encoding failed"))?;
    }
    Ok(encoded)
}

/// Validates that a commissioned task is representable by the wire limits.
///
/// Composition roots call this before advertising a route so unsupported
/// cadence, collection, identifier, or unit shapes fail at startup rather
/// than after source reads.
pub fn validate_task_contract(task: &DataProcessingTask) -> Result<(), CodecError> {
    validate_identifier(task.identity().id())?;
    validate_identifier(task.processor_contract())?;
    let specification = task
        .forecast_spec()
        .ok_or(CodecError::Contract("task has no forecast specification"))?;
    if !specification.cadence_ms().is_multiple_of(1_000)
        || specification.cadence_ms() / 1_000 > MAX_CADENCE_SECONDS
        || specification.history_steps() > MAX_SEGMENT_SAMPLES
        || specification.max_horizon_steps() > MAX_FORECAST_POINTS
    {
        return Err(CodecError::Contract(
            "task cadence or sample bounds exceed the contract",
        ));
    }
    if specification.allowed_fallbacks().len() != specification.fallback_policies().len()
        || specification
            .fallback_policies()
            .iter()
            .any(|policy| policy.strategy() != "persistence")
    {
        return Err(CodecError::Contract(
            "routes require a complete registered fallback verifier",
        ));
    }
    validate_feature_name(specification.target().name())?;
    validate_unit(specification.target().unit())?;
    validate_identifier(specification.target().sign_convention())?;
    for fallback in specification.allowed_fallbacks() {
        validate_identifier(fallback)?;
    }
    for policy in specification.fallback_policies() {
        validate_identifier(policy.strategy())?;
        validate_identifier(policy.version())?;
        validate_feature_name(policy.source_feature())?;
    }
    let mut history = 0usize;
    let mut future = 0usize;
    let mut static_features = 0usize;
    for feature in task.features() {
        validate_feature_name(feature.name())?;
        if let Some(unit) = feature.unit() {
            validate_unit(unit)?;
        }
        match feature.role() {
            FeatureRole::History => history = history.saturating_add(1),
            FeatureRole::FutureCovariate => future = future.saturating_add(1),
            FeatureRole::Static => static_features = static_features.saturating_add(1),
        }
    }
    if history > MAX_FEATURES_PER_SEGMENT
        || future > MAX_FEATURES_PER_SEGMENT
        || static_features > MAX_STATIC_FEATURES
        || history
            .saturating_add(future)
            .saturating_add(static_features)
            > MAX_PROVENANCE_ENTRIES
    {
        return Err(CodecError::Contract(
            "task feature bounds exceed the contract",
        ));
    }
    Ok(())
}

/// Validates every identity that a composition root will place on the wire.
///
/// This complements [`validate_task_contract`] with site binding, artifact,
/// and processor descriptor values that are not owned by the task itself.
#[allow(clippy::too_many_arguments)]
pub fn validate_commissioned_route_contract(
    task: &DataProcessingTask,
    binding: &BindingIdentity,
    artifact: Option<&ArtifactSelector>,
    processor_id: &str,
    processor_version: &str,
    supported_contracts: &[String],
) -> Result<(), CodecError> {
    validate_task_contract(task)?;
    binding_to_wire(binding)?;
    artifact_selector_to_wire(artifact)?;
    validate_identifier(processor_id)?;
    validate_identifier(processor_version)?;
    for contract in supported_contracts {
        validate_identifier(contract)?;
    }
    Ok(())
}

pub(crate) fn request_to_wire(request: &DataProcessingRequest) -> Result<RequestWire, CodecError> {
    validate_uuid(request.request_id())?;
    validate_identifier(request.processor_contract())?;
    validate_digest(request.input_digest())?;
    let expected_digest = compute_input_digest(
        request.task(),
        request.binding(),
        request.processor_contract(),
        request.artifact_selector(),
        request.frame(),
        request.options(),
    )?;
    if expected_digest != request.input_digest() {
        return Err(CodecError::Contract(
            "input digest does not match request content",
        ));
    }
    validate_request_horizon(request.frame(), request.options())?;
    Ok(RequestWire {
        schema: REQUEST_SCHEMA.to_string(),
        request_id: request.request_id().to_string(),
        submitted_at: format_timestamp(request.submitted_at())?,
        deadline: format_timestamp(request.deadline())?,
        task: task_to_wire(request.task())?,
        binding: binding_to_wire(request.binding())?,
        processor_contract: request.processor_contract().to_string(),
        artifact: artifact_selector_to_wire(request.artifact_selector())?,
        frame: frame_to_wire(request.frame())?,
        options: options_to_wire(request.options())?,
        input_digest: request.input_digest().to_string(),
    })
}

pub(crate) fn request_from_wire(wire: RequestWire) -> Result<DataProcessingRequest, CodecError> {
    if wire.schema != REQUEST_SCHEMA {
        return Err(CodecError::Contract("invalid request schema"));
    }
    validate_uuid(&wire.request_id)?;
    validate_identifier(&wire.processor_contract)?;
    validate_digest(&wire.input_digest)?;

    let task = task_from_wire(wire.task)?;
    let binding = binding_from_wire(wire.binding)?;
    let submitted_at = parse_timestamp(&wire.submitted_at)?;
    let deadline = parse_timestamp(&wire.deadline)?;
    let artifact = artifact_selector_from_wire(wire.artifact)?;
    let frame = frame_from_wire(wire.frame)?;
    let options = options_from_wire(wire.options)?;
    validate_request_horizon(&frame, &options)?;
    let expected_digest = compute_input_digest(
        &task,
        &binding,
        &wire.processor_contract,
        artifact.as_ref(),
        &frame,
        &options,
    )?;
    if expected_digest != wire.input_digest {
        return Err(CodecError::Contract(
            "input digest does not match request content",
        ));
    }

    DataProcessingRequest::new(
        wire.request_id,
        task,
        binding,
        frame,
        submitted_at,
        deadline,
        wire.processor_contract,
        artifact,
        wire.input_digest,
        options,
    )
    .map_err(CodecError::Domain)
}

pub(crate) fn result_to_wire(result: &ProcessingResult) -> Result<ResultWire, CodecError> {
    validate_uuid(result.request_id())?;
    validate_digest(result.input_digest())?;
    validate_stable_codes(result.warnings())?;
    Ok(ResultWire {
        schema: RESULT_SCHEMA.to_string(),
        request_id: result.request_id().to_string(),
        task: task_to_wire(result.task())?,
        binding: binding_to_wire(result.binding())?,
        input_digest: result.input_digest().to_string(),
        status: status_to_wire(result.status()),
        issued_at: format_timestamp(result.produced_at())?,
        expires_at: result.expires_at().map(format_timestamp).transpose()?,
        input_watermark: format_timestamp(result.input_watermark())?,
        processor: processor_to_wire(result.processor())?,
        artifact: result
            .artifact()
            .map(artifact_provenance_to_wire)
            .transpose()?,
        output: result.output().map(output_to_wire).transpose()?,
        fallback: result.fallback().map(fallback_to_wire).transpose()?,
        unavailable: result.unavailable().map(unavailable_to_wire).transpose()?,
        warnings: result.warnings().to_vec(),
    })
}

pub(crate) fn result_from_wire(wire: ResultWire) -> Result<ProcessingResult, CodecError> {
    if wire.schema != RESULT_SCHEMA {
        return Err(CodecError::Contract("invalid result schema"));
    }
    validate_uuid(&wire.request_id)?;
    validate_digest(&wire.input_digest)?;
    validate_stable_codes(&wire.warnings)?;
    let result = ProcessingResult::new(
        wire.request_id,
        task_from_wire(wire.task)?,
        binding_from_wire(wire.binding)?,
        wire.input_digest,
        status_from_wire(wire.status),
        processor_from_wire(wire.processor)?,
        wire.artifact
            .map(artifact_provenance_from_wire)
            .transpose()?,
        parse_timestamp(&wire.input_watermark)?,
        parse_timestamp(&wire.issued_at)?,
        wire.expires_at
            .as_deref()
            .map(parse_timestamp)
            .transpose()?,
        wire.output.map(output_from_wire).transpose()?,
        wire.fallback.map(fallback_from_wire).transpose()?,
        wire.unavailable.map(unavailable_from_wire).transpose()?,
    )?;
    result
        .with_warnings(wire.warnings)
        .map_err(CodecError::Domain)
}

pub(crate) fn derived_data_to_wire(derived: &DerivedData) -> Result<DerivedDataWire, CodecError> {
    validate_uuid(derived.result_id())?;
    let accepted_at = derived.accepted_at();
    let result = derived.result();
    if accepted_at < result.produced_at() {
        return Err(CodecError::Contract(
            "derived data cannot be accepted before processor completion",
        ));
    }

    let result_wire = result_to_wire(result)?;
    let (processing_status, fallback_used) = match result.status() {
        ProcessingStatus::Produced => (AcceptedProcessingStatusWire::Produced, false),
        ProcessingStatus::Fallback => (AcceptedProcessingStatusWire::Fallback, true),
        ProcessingStatus::Unavailable => {
            return Err(CodecError::Contract(
                "unavailable processor results cannot become derived data",
            ));
        },
    };
    let expires_at = result_wire.expires_at.ok_or(CodecError::Contract(
        "accepted derived data requires an expiry",
    ))?;
    let data = result_wire.output.ok_or(CodecError::Contract(
        "accepted derived data requires typed output",
    ))?;
    let quality = derived.frame_quality();

    Ok(DerivedDataWire {
        schema: DERIVED_DATA_SCHEMA.to_string(),
        result_id: derived.result_id().to_string(),
        request_id: result_wire.request_id,
        task: result_wire.task,
        binding: result_wire.binding,
        accepted_at: format_timestamp(accepted_at)?,
        expires_at,
        input_digest: result_wire.input_digest,
        processing_status,
        processor: result_wire.processor,
        artifact: result_wire.artifact,
        fallback: result_wire.fallback,
        warnings: result_wire.warnings,
        quality: AcceptedFrameQualityWire {
            input_watermark: format_timestamp(quality.input_watermark())?,
            missing_ratio: finite(quality.missing_ratio())?,
            max_gap_seconds: milliseconds_to_seconds(quality.max_gap_ms())?,
            live_tail_included: quality.live_tail_included(),
            substituted_samples: usize_to_u64(quality.substituted_samples())?,
            fallback_used,
        },
        data,
    })
}

fn task_to_wire(task: &TaskIdentity) -> Result<TaskWire, CodecError> {
    validate_identifier(task.id())?;
    if task.revision() == 0 {
        return Err(CodecError::Contract("task revision must be positive"));
    }
    Ok(TaskWire {
        id: task.id().to_string(),
        revision: task.revision(),
        kind: TaskKindWire::Forecast,
    })
}

fn task_from_wire(wire: TaskWire) -> Result<TaskIdentity, CodecError> {
    validate_identifier(&wire.id)?;
    match wire.kind {
        TaskKindWire::Forecast => {},
    }
    TaskIdentity::new(wire.id, wire.revision).map_err(CodecError::Domain)
}

fn binding_to_wire(binding: &BindingIdentity) -> Result<BindingWire, CodecError> {
    validate_identifier(binding.id())?;
    if binding.revision() == 0 {
        return Err(CodecError::Contract("binding revision must be positive"));
    }
    Ok(BindingWire {
        id: binding.id().to_string(),
        revision: binding.revision(),
    })
}

fn binding_from_wire(wire: BindingWire) -> Result<BindingIdentity, CodecError> {
    validate_identifier(&wire.id)?;
    BindingIdentity::new(wire.id, wire.revision).map_err(CodecError::Domain)
}

fn artifact_selector_to_wire(
    artifact: Option<&ArtifactSelector>,
) -> Result<Option<ArtifactSelectorWire>, CodecError> {
    artifact
        .map(|artifact| {
            validate_identifier(artifact.kind())?;
            validate_identifier(artifact.family())?;
            if let Some(version) = artifact.version() {
                validate_identifier(version)?;
            }
            if let Some(digest) = artifact.digest() {
                validate_digest(digest)?;
            }
            Ok(ArtifactSelectorWire {
                kind: artifact.kind().to_string(),
                family: artifact.family().to_string(),
                version: artifact.version().map(ToOwned::to_owned),
                artifact_digest: artifact.digest().map(ToOwned::to_owned),
            })
        })
        .transpose()
}

fn artifact_selector_from_wire(
    artifact: Option<ArtifactSelectorWire>,
) -> Result<Option<ArtifactSelector>, CodecError> {
    artifact
        .map(|artifact| {
            validate_identifier(&artifact.kind)?;
            validate_identifier(&artifact.family)?;
            if let Some(version) = &artifact.version {
                validate_identifier(version)?;
            }
            let selector =
                ArtifactSelector::new(artifact.kind, artifact.family, artifact.version.as_deref())
                    .map_err(CodecError::Domain)?;
            if let Some(digest) = artifact.artifact_digest {
                selector.with_digest(digest).map_err(CodecError::Domain)
            } else {
                Ok(selector)
            }
        })
        .transpose()
}

fn frame_to_wire(frame: &ProcessingFrame) -> Result<FrameWire, CodecError> {
    if frame.provenance().is_empty() {
        return Err(CodecError::Contract("frame provenance must not be empty"));
    }
    validate_actual_input_watermark(frame.provenance(), frame.quality().input_watermark())?;
    validate_segment_cadence(frame.history(), frame.cadence_ms())?;
    if let Some(future) = frame.future_covariates() {
        validate_segment_cadence(future, frame.cadence_ms())?;
    }
    let observed_max_gap = maximum_observation_gap(frame.history(), frame.cadence_ms());
    if frame.quality().max_gap_ms() != observed_max_gap {
        return Err(CodecError::Contract(
            "frame max gap does not match usable historical observations",
        ));
    }
    let wire = FrameWire {
        schema: FRAME_SCHEMA.to_string(),
        as_of: format_timestamp(frame.as_of())?,
        cadence_seconds: milliseconds_to_seconds(frame.cadence_ms())?,
        history: segment_to_wire(frame.history())?,
        future_covariates: frame.future_covariates().map(segment_to_wire).transpose()?,
        static_features: static_features_to_wire(frame.static_features())?,
        quality: FrameQualityWire {
            input_watermark: format_timestamp(frame.quality().input_watermark())?,
            missing_ratio: finite(frame.quality().missing_ratio())?,
            max_gap_seconds: milliseconds_to_seconds(frame.quality().max_gap_ms())?,
            live_tail_included: frame.quality().live_tail_included(),
            substituted_samples: usize_to_u64(frame.quality().substituted_samples())?,
        },
        provenance: frame
            .provenance()
            .iter()
            .map(source_provenance_to_wire)
            .collect::<Result<_, _>>()?,
    };
    validate_frame_limits(&wire)?;
    validate_provenance_shape(&wire)?;
    Ok(wire)
}

fn frame_from_wire(wire: FrameWire) -> Result<ProcessingFrame, CodecError> {
    if wire.schema != FRAME_SCHEMA {
        return Err(CodecError::Contract("invalid frame schema"));
    }
    validate_frame_limits(&wire)?;
    validate_provenance_shape(&wire)?;
    let as_of = parse_timestamp(&wire.as_of)?;
    let cadence_ms = seconds_to_milliseconds(wire.cadence_seconds)?;
    let history = segment_from_wire(wire.history, FeatureRole::History)?;
    let future_covariates = wire
        .future_covariates
        .map(|segment| segment_from_wire(segment, FeatureRole::FutureCovariate))
        .transpose()?;
    let static_features = static_features_from_wire(wire.static_features)?;
    let quality = FrameQuality::new(
        parse_timestamp(&wire.quality.input_watermark)?,
        finite(wire.quality.missing_ratio)?,
        seconds_to_milliseconds_allow_zero(wire.quality.max_gap_seconds)?,
        wire.quality.live_tail_included,
        u64_to_usize(wire.quality.substituted_samples)?,
    )?;
    validate_segment_cadence(&history, cadence_ms)?;
    if let Some(future) = &future_covariates {
        validate_segment_cadence(future, cadence_ms)?;
    }
    let observed_max_gap = maximum_observation_gap(&history, cadence_ms);
    if quality.max_gap_ms() != observed_max_gap {
        return Err(CodecError::Contract(
            "frame max gap does not match usable historical observations",
        ));
    }
    let provenance = wire
        .provenance
        .into_iter()
        .map(source_provenance_from_wire)
        .collect::<Result<Vec<_>, _>>()?;
    validate_actual_input_watermark(&provenance, quality.input_watermark())?;
    ProcessingFrame::new(
        as_of,
        cadence_ms,
        history,
        future_covariates,
        static_features,
        quality,
        provenance,
    )
    .map_err(CodecError::Domain)
}

fn segment_to_wire(segment: &Segment) -> Result<SegmentWire, CodecError> {
    let mut features = BTreeMap::new();
    for series in segment.series() {
        validate_feature_name(series.definition().name())?;
        let replaced = features.insert(
            series.definition().name().to_string(),
            SeriesWire {
                value_type: value_type_to_wire(series.definition().value_type()),
                unit: series.definition().unit().map(ToOwned::to_owned),
                values: series
                    .values()
                    .iter()
                    .map(scalar_to_wire)
                    .collect::<Result<_, _>>()?,
                quality: series
                    .quality()
                    .iter()
                    .copied()
                    .map(quality_to_wire)
                    .collect(),
            },
        );
        if replaced.is_some() {
            return Err(CodecError::Contract("duplicate feature name"));
        }
    }
    Ok(SegmentWire {
        timestamps: segment
            .timestamps()
            .iter()
            .copied()
            .map(format_timestamp)
            .collect::<Result<_, _>>()?,
        features,
    })
}

fn segment_from_wire(wire: SegmentWire, role: FeatureRole) -> Result<Segment, CodecError> {
    let timestamps = wire
        .timestamps
        .iter()
        .map(String::as_str)
        .map(parse_timestamp)
        .collect::<Result<_, _>>()?;
    let series = wire
        .features
        .into_iter()
        .map(|(name, series)| series_from_wire(name, role, series))
        .collect::<Result<_, _>>()?;
    Segment::new(timestamps, series).map_err(CodecError::Domain)
}

fn series_from_wire(
    name: String,
    role: FeatureRole,
    wire: SeriesWire,
) -> Result<Series, CodecError> {
    validate_feature_name(&name)?;
    let definition = feature_definition_from_wire(name, role, wire.value_type, wire.unit)?;
    let values = wire
        .values
        .into_iter()
        .map(|value| scalar_from_wire(value, definition.value_type()))
        .collect::<Result<_, _>>()?;
    let quality = wire.quality.into_iter().map(quality_from_wire).collect();
    Series::new(definition, values, quality).map_err(CodecError::Domain)
}

fn static_features_to_wire(
    features: &[StaticFeature],
) -> Result<BTreeMap<String, StaticFeatureWire>, CodecError> {
    let mut values = BTreeMap::new();
    for feature in features {
        validate_feature_name(feature.definition().name())?;
        let replaced = values.insert(
            feature.definition().name().to_string(),
            StaticFeatureWire {
                value_type: value_type_to_wire(feature.definition().value_type()),
                unit: feature.definition().unit().map(ToOwned::to_owned),
                value: scalar_to_wire(feature.value())?,
                quality: quality_to_wire(feature.quality()),
            },
        );
        if replaced.is_some() {
            return Err(CodecError::Contract("duplicate static feature name"));
        }
    }
    Ok(values)
}

fn static_features_from_wire(
    features: BTreeMap<String, StaticFeatureWire>,
) -> Result<Vec<StaticFeature>, CodecError> {
    features
        .into_iter()
        .map(|(name, wire)| {
            validate_feature_name(&name)?;
            let definition = feature_definition_from_wire(
                name,
                FeatureRole::Static,
                wire.value_type,
                wire.unit,
            )?;
            let value = scalar_from_wire(wire.value, definition.value_type())?;
            StaticFeature::new(definition, value, quality_from_wire(wire.quality))
                .map_err(CodecError::Domain)
        })
        .collect()
}

fn feature_definition_from_wire(
    name: String,
    role: FeatureRole,
    value_type: FeatureValueTypeWire,
    unit: Option<String>,
) -> Result<FeatureDefinition, CodecError> {
    match value_type {
        FeatureValueTypeWire::Number => {
            let unit = unit.ok_or(CodecError::Contract("numeric feature requires a unit"))?;
            validate_unit(&unit)?;
            FeatureDefinition::numeric(name, role, unit).map_err(CodecError::Domain)
        },
        FeatureValueTypeWire::String => {
            if unit.is_some() {
                return Err(CodecError::Contract("text feature must not declare a unit"));
            }
            FeatureDefinition::new(name, role, FeatureValueType::Text).map_err(CodecError::Domain)
        },
        FeatureValueTypeWire::Boolean => {
            if unit.is_some() {
                return Err(CodecError::Contract(
                    "boolean feature must not declare a unit",
                ));
            }
            FeatureDefinition::new(name, role, FeatureValueType::Boolean)
                .map_err(CodecError::Domain)
        },
    }
}

fn scalar_to_wire(value: &FeatureValue) -> Result<ScalarWire, CodecError> {
    if value.is_missing() {
        Ok(ScalarWire::Null)
    } else if let Some(number) = value.as_number() {
        Ok(ScalarWire::Number(finite(number)?))
    } else if let Some(text) = value.as_text() {
        Ok(ScalarWire::String(text.to_string()))
    } else if let Some(boolean) = value.as_boolean() {
        Ok(ScalarWire::Boolean(boolean))
    } else {
        Err(CodecError::Contract("unsupported feature value"))
    }
}

fn scalar_from_wire(
    value: ScalarWire,
    expected: FeatureValueType,
) -> Result<FeatureValue, CodecError> {
    match (value, expected) {
        (ScalarWire::Null, _) => Ok(FeatureValue::missing()),
        (ScalarWire::Number(value), FeatureValueType::Number) => {
            FeatureValue::number(finite(value)?).map_err(CodecError::Domain)
        },
        (ScalarWire::String(value), FeatureValueType::Text) => Ok(FeatureValue::text(value)),
        (ScalarWire::Boolean(value), FeatureValueType::Boolean) => Ok(FeatureValue::boolean(value)),
        _ => Err(CodecError::Contract(
            "feature value type does not match declaration",
        )),
    }
}

fn source_provenance_to_wire(
    source: &SourceProvenance,
) -> Result<SourceProvenanceWire, CodecError> {
    validate_feature_name(source.feature())?;
    if let Some(source_ref) = source.source_ref() {
        validate_text(source_ref, 2048)?;
    }
    Ok(SourceProvenanceWire {
        segment: segment_kind_to_wire(source.segment()),
        feature: source.feature().to_string(),
        source_kind: source_kind_to_wire(source.source_kind()),
        source_ref: source.source_ref().map(ToOwned::to_owned),
        watermark: format_timestamp(source.watermark())?,
        issued_at: source.issued_at().map(format_timestamp).transpose()?,
    })
}

fn source_provenance_from_wire(wire: SourceProvenanceWire) -> Result<SourceProvenance, CodecError> {
    validate_feature_name(&wire.feature)?;
    if let Some(source_ref) = &wire.source_ref {
        validate_text(source_ref, 2048)?;
    }
    let mut source = SourceProvenance::new(
        segment_kind_from_wire(wire.segment),
        wire.feature,
        source_kind_from_wire(wire.source_kind),
        wire.source_ref.as_deref(),
        parse_timestamp(&wire.watermark)?,
    )?;
    if let Some(issued_at) = wire.issued_at {
        source = source.with_issued_at(parse_timestamp(&issued_at)?)?;
    }
    Ok(source)
}

fn validate_provenance_targets(frame: &FrameWire) -> Result<(), CodecError> {
    for source in &frame.provenance {
        let exists = match source.segment {
            SegmentKindWire::History => frame.history.features.contains_key(&source.feature),
            SegmentKindWire::FutureCovariates => frame
                .future_covariates
                .as_ref()
                .is_some_and(|segment| segment.features.contains_key(&source.feature)),
            SegmentKindWire::StaticFeatures => frame.static_features.contains_key(&source.feature),
        };
        if !exists {
            return Err(CodecError::Contract(
                "provenance references an unknown frame feature",
            ));
        }
    }
    Ok(())
}

fn validate_provenance_shape(frame: &FrameWire) -> Result<(), CodecError> {
    if frame.provenance.is_empty() || !provenance_keys_are_unique(&frame.provenance) {
        return Err(CodecError::Contract(
            "frame provenance keys must be non-empty and unique",
        ));
    }
    validate_provenance_targets(frame)?;
    let complete =
        frame.history.features.keys().all(|feature| {
            provenance_contains(&frame.provenance, SegmentKindWire::History, feature)
        }) && frame.future_covariates.as_ref().is_none_or(|segment| {
            segment.features.keys().all(|feature| {
                provenance_contains(
                    &frame.provenance,
                    SegmentKindWire::FutureCovariates,
                    feature,
                )
            })
        }) && frame.static_features.keys().all(|feature| {
            provenance_contains(&frame.provenance, SegmentKindWire::StaticFeatures, feature)
        });
    if complete {
        Ok(())
    } else {
        Err(CodecError::Contract(
            "every frame feature requires exactly one provenance entry",
        ))
    }
}

fn validate_frame_limits(frame: &FrameWire) -> Result<(), CodecError> {
    if frame.cadence_seconds == 0 || frame.cadence_seconds > MAX_CADENCE_SECONDS {
        return Err(CodecError::Contract("frame cadence is outside limits"));
    }
    validate_segment_limits(&frame.history)?;
    if let Some(future) = &frame.future_covariates {
        validate_segment_limits(future)?;
    }
    if frame.static_features.len() > MAX_STATIC_FEATURES {
        return Err(CodecError::Contract(
            "static feature count exceeds the limit",
        ));
    }
    if frame.provenance.len() > MAX_PROVENANCE_ENTRIES {
        return Err(CodecError::Contract("provenance count exceeds the limit"));
    }
    Ok(())
}

fn validate_segment_limits(segment: &SegmentWire) -> Result<(), CodecError> {
    if segment.timestamps.is_empty() || segment.timestamps.len() > MAX_SEGMENT_SAMPLES {
        return Err(CodecError::Contract(
            "segment timestamp count is outside limits",
        ));
    }
    if segment.features.is_empty() || segment.features.len() > MAX_FEATURES_PER_SEGMENT {
        return Err(CodecError::Contract(
            "segment feature count is outside limits",
        ));
    }
    if segment.features.values().any(|series| {
        series.values.is_empty()
            || series.values.len() > MAX_SEGMENT_SAMPLES
            || series.quality.is_empty()
            || series.quality.len() > MAX_SEGMENT_SAMPLES
    }) {
        return Err(CodecError::Contract("series array count is outside limits"));
    }
    Ok(())
}

fn provenance_contains(
    provenance: &[SourceProvenanceWire],
    segment: SegmentKindWire,
    feature: &str,
) -> bool {
    provenance
        .iter()
        .any(|source| source.segment == segment && source.feature == feature)
}

fn provenance_keys_are_unique(provenance: &[SourceProvenanceWire]) -> bool {
    provenance.iter().enumerate().all(|(index, source)| {
        !provenance[..index]
            .iter()
            .any(|seen| seen.segment == source.segment && seen.feature == source.feature)
    })
}

fn validate_segment_cadence(segment: &Segment, cadence_ms: u64) -> Result<(), CodecError> {
    if segment
        .timestamps()
        .windows(2)
        .any(|pair| pair[1].get() - pair[0].get() != cadence_ms)
    {
        Err(CodecError::Contract(
            "frame timestamps do not match the declared cadence",
        ))
    } else {
        Ok(())
    }
}

fn validate_request_horizon(
    frame: &ProcessingFrame,
    options: &ProcessingOptions,
) -> Result<(), CodecError> {
    match options {
        ProcessingOptions::Forecast(options) => {
            if frame
                .future_covariates()
                .is_some_and(|segment| segment.sample_count() != options.horizon_steps())
            {
                Err(CodecError::Contract(
                    "future covariate length does not match forecast horizon",
                ))
            } else {
                Ok(())
            }
        },
    }
}

fn validate_actual_input_watermark(
    provenance: &[SourceProvenance],
    input_watermark: TimestampMs,
) -> Result<(), CodecError> {
    let actual_watermark = provenance
        .iter()
        .filter(|source| {
            !matches!(
                source.source_kind(),
                SourceKind::Calendar | SourceKind::Constant
            )
        })
        .map(SourceProvenance::watermark)
        .max();
    if actual_watermark == Some(input_watermark) {
        Ok(())
    } else {
        Err(CodecError::Contract(
            "input watermark must equal the newest actual source watermark",
        ))
    }
}

fn options_to_wire(options: &ProcessingOptions) -> Result<OptionsWire, CodecError> {
    match options {
        ProcessingOptions::Forecast(options) => {
            if options.horizon_steps() > MAX_FORECAST_POINTS
                || options.quantiles().len() > MAX_QUANTILES
            {
                return Err(CodecError::Contract(
                    "forecast options exceed collection limits",
                ));
            }
            Ok(OptionsWire::Forecast {
                horizon_steps: usize_to_u64(options.horizon_steps())?,
                quantiles: (!options.quantiles().is_empty())
                    .then(|| {
                        options
                            .quantiles()
                            .iter()
                            .copied()
                            .map(finite)
                            .collect::<Result<Vec<_>, _>>()
                    })
                    .transpose()?,
            })
        },
    }
}

fn options_from_wire(wire: OptionsWire) -> Result<ProcessingOptions, CodecError> {
    match wire {
        OptionsWire::Forecast {
            horizon_steps,
            quantiles,
        } => {
            if horizon_steps == 0
                || horizon_steps > u64::try_from(MAX_FORECAST_POINTS).unwrap_or(u64::MAX)
            {
                return Err(CodecError::Contract("forecast horizon is outside limits"));
            }
            if quantiles.as_ref().is_some_and(Vec::is_empty) {
                return Err(CodecError::Contract(
                    "explicit forecast quantiles must not be empty",
                ));
            }
            if quantiles
                .as_ref()
                .is_some_and(|values| values.len() > MAX_QUANTILES)
            {
                return Err(CodecError::Contract(
                    "forecast quantile count exceeds the limit",
                ));
            }
            Ok(ProcessingOptions::Forecast(ForecastOptions::new(
                u64_to_usize(horizon_steps)?,
                quantiles
                    .unwrap_or_default()
                    .into_iter()
                    .map(finite)
                    .collect::<Result<_, _>>()?,
            )?))
        },
    }
}

fn processor_to_wire(processor: &ProcessorProvenance) -> Result<ProcessorWire, CodecError> {
    validate_identifier(processor.id())?;
    validate_identifier(processor.version())?;
    validate_identifier(processor.contract())?;
    Ok(ProcessorWire {
        id: processor.id().to_string(),
        version: processor.version().to_string(),
        contract: processor.contract().to_string(),
    })
}

fn processor_from_wire(wire: ProcessorWire) -> Result<ProcessorProvenance, CodecError> {
    validate_identifier(&wire.id)?;
    validate_identifier(&wire.version)?;
    validate_identifier(&wire.contract)?;
    ProcessorProvenance::new(wire.id, wire.version, wire.contract).map_err(CodecError::Domain)
}

fn artifact_provenance_to_wire(
    artifact: &ArtifactProvenance,
) -> Result<ArtifactProvenanceWire, CodecError> {
    validate_identifier(artifact.kind())?;
    validate_identifier(artifact.family())?;
    validate_identifier(artifact.version())?;
    validate_digest(artifact.digest())?;
    Ok(ArtifactProvenanceWire {
        kind: artifact.kind().to_string(),
        family: artifact.family().to_string(),
        version: artifact.version().to_string(),
        artifact_digest: artifact.digest().to_string(),
    })
}

fn artifact_provenance_from_wire(
    wire: ArtifactProvenanceWire,
) -> Result<ArtifactProvenance, CodecError> {
    validate_identifier(&wire.kind)?;
    validate_identifier(&wire.family)?;
    validate_identifier(&wire.version)?;
    validate_digest(&wire.artifact_digest)?;
    ArtifactProvenance::new(wire.kind, wire.family, wire.version, wire.artifact_digest)
        .map_err(CodecError::Domain)
}

fn output_to_wire(output: &ProcessingOutput) -> Result<ForecastOutputWire, CodecError> {
    match output {
        ProcessingOutput::Forecast(output) => {
            if output.points().len() > MAX_FORECAST_POINTS
                || output
                    .points()
                    .iter()
                    .any(|point| point.quantiles().len() > MAX_QUANTILES)
            {
                return Err(CodecError::Contract(
                    "forecast output exceeds collection limits",
                ));
            }
            let cadence_seconds = milliseconds_to_seconds(output.cadence_ms())?;
            if cadence_seconds > MAX_CADENCE_SECONDS {
                return Err(CodecError::Contract("forecast cadence exceeds the limit"));
            }
            validate_feature_name(output.target())?;
            validate_unit(output.unit())?;
            validate_identifier(output.sign_convention())?;
            Ok(ForecastOutputWire {
                schema: FORECAST_OUTPUT_SCHEMA.to_string(),
                kind: TaskKindWire::Forecast,
                target: output.target().to_string(),
                unit: output.unit().to_string(),
                sign_convention: output.sign_convention().to_string(),
                cadence_seconds,
                timestamp_semantics: TimestampSemanticsWire::IntervalEnd,
                points: output
                    .points()
                    .iter()
                    .map(forecast_point_to_wire)
                    .collect::<Result<_, _>>()?,
            })
        },
    }
}

fn output_from_wire(wire: ForecastOutputWire) -> Result<ProcessingOutput, CodecError> {
    if wire.schema != FORECAST_OUTPUT_SCHEMA {
        return Err(CodecError::Contract("invalid forecast output schema"));
    }
    match wire.kind {
        TaskKindWire::Forecast => {},
    }
    if wire.timestamp_semantics != TimestampSemanticsWire::IntervalEnd {
        return Err(CodecError::Contract(
            "domain forecast requires interval_end timestamp semantics",
        ));
    }
    if wire.points.is_empty() || wire.points.len() > MAX_FORECAST_POINTS {
        return Err(CodecError::Contract(
            "forecast point count is outside limits",
        ));
    }
    if wire.cadence_seconds == 0 || wire.cadence_seconds > MAX_CADENCE_SECONDS {
        return Err(CodecError::Contract("forecast cadence is outside limits"));
    }
    if wire.points.iter().any(|point| {
        point
            .quantiles
            .as_ref()
            .is_some_and(|quantiles| quantiles.len() > MAX_QUANTILES)
    }) {
        return Err(CodecError::Contract(
            "forecast quantile count exceeds the limit",
        ));
    }
    validate_feature_name(&wire.target)?;
    validate_unit(&wire.unit)?;
    validate_identifier(&wire.sign_convention)?;
    let points = wire
        .points
        .into_iter()
        .map(forecast_point_from_wire)
        .collect::<Result<_, _>>()?;
    Ok(ProcessingOutput::Forecast(ForecastOutput::new(
        wire.target,
        wire.unit,
        wire.sign_convention,
        seconds_to_milliseconds(wire.cadence_seconds)?,
        points,
    )?))
}

fn forecast_point_to_wire(point: &ForecastPoint) -> Result<ForecastPointWire, CodecError> {
    Ok(ForecastPointWire {
        timestamp: format_timestamp(point.timestamp())?,
        value: finite(point.value())?,
        quantiles: (!point.quantiles().is_empty())
            .then(|| {
                point
                    .quantiles()
                    .iter()
                    .map(|quantile| {
                        Ok(ForecastQuantileWire {
                            probability: finite(quantile.probability())?,
                            value: finite(quantile.value())?,
                        })
                    })
                    .collect::<Result<Vec<_>, CodecError>>()
            })
            .transpose()?,
    })
}

fn forecast_point_from_wire(wire: ForecastPointWire) -> Result<ForecastPoint, CodecError> {
    if wire.quantiles.as_ref().is_some_and(Vec::is_empty) {
        return Err(CodecError::Contract(
            "explicit forecast point quantiles must not be empty",
        ));
    }
    ForecastPoint::new(
        parse_timestamp(&wire.timestamp)?,
        finite(wire.value)?,
        wire.quantiles
            .unwrap_or_default()
            .into_iter()
            .map(|quantile| {
                ForecastQuantile::new(finite(quantile.probability)?, finite(quantile.value)?)
                    .map_err(CodecError::Domain)
            })
            .collect::<Result<_, _>>()?,
    )
    .map_err(CodecError::Domain)
}

fn fallback_to_wire(fallback: &FallbackInfo) -> Result<FallbackWire, CodecError> {
    validate_identifier(fallback.strategy())?;
    validate_identifier(fallback.strategy_version())?;
    validate_stable_code(fallback.reason())?;
    validate_identifier(fallback.source_feature())?;
    Ok(FallbackWire {
        strategy: fallback.strategy().to_string(),
        strategy_version: fallback.strategy_version().to_string(),
        reason_code: fallback.reason().to_string(),
        source_feature: fallback.source_feature().to_string(),
        based_on_data_through: format_timestamp(fallback.based_on_data_through())?,
    })
}

fn fallback_from_wire(wire: FallbackWire) -> Result<FallbackInfo, CodecError> {
    validate_identifier(&wire.strategy)?;
    validate_identifier(&wire.strategy_version)?;
    validate_stable_code(&wire.reason_code)?;
    validate_identifier(&wire.source_feature)?;
    FallbackInfo::new(
        wire.strategy,
        wire.strategy_version,
        wire.reason_code,
        wire.source_feature,
        parse_timestamp(&wire.based_on_data_through)?,
    )
    .map_err(CodecError::Domain)
}

fn unavailable_to_wire(unavailable: &UnavailableInfo) -> Result<UnavailableWire, CodecError> {
    validate_stable_code(unavailable.reason())?;
    Ok(UnavailableWire {
        reason_code: unavailable.reason().to_string(),
        retryable: unavailable.retryable(),
        retry_after_seconds: unavailable
            .retry_after_ms()
            .map(milliseconds_to_seconds)
            .transpose()?,
    })
}

fn unavailable_from_wire(wire: UnavailableWire) -> Result<UnavailableInfo, CodecError> {
    validate_stable_code(&wire.reason_code)?;
    UnavailableInfo::new(
        wire.reason_code,
        wire.retryable,
        wire.retry_after_seconds
            .map(seconds_to_milliseconds)
            .transpose()?,
    )
    .map_err(CodecError::Domain)
}

const fn status_to_wire(status: ProcessingStatus) -> ProcessingStatusWire {
    match status {
        ProcessingStatus::Produced => ProcessingStatusWire::Produced,
        ProcessingStatus::Fallback => ProcessingStatusWire::Fallback,
        ProcessingStatus::Unavailable => ProcessingStatusWire::Unavailable,
    }
}

const fn status_from_wire(status: ProcessingStatusWire) -> ProcessingStatus {
    match status {
        ProcessingStatusWire::Produced => ProcessingStatus::Produced,
        ProcessingStatusWire::Fallback => ProcessingStatus::Fallback,
        ProcessingStatusWire::Unavailable => ProcessingStatus::Unavailable,
    }
}

const fn value_type_to_wire(value_type: FeatureValueType) -> FeatureValueTypeWire {
    match value_type {
        FeatureValueType::Number => FeatureValueTypeWire::Number,
        FeatureValueType::Text => FeatureValueTypeWire::String,
        FeatureValueType::Boolean => FeatureValueTypeWire::Boolean,
    }
}

const fn quality_to_wire(quality: SampleQuality) -> SampleQualityWire {
    match quality {
        SampleQuality::Good => SampleQualityWire::Good,
        SampleQuality::Uncertain => SampleQualityWire::Uncertain,
        SampleQuality::Substituted => SampleQualityWire::Substituted,
        SampleQuality::Missing => SampleQualityWire::Missing,
    }
}

const fn quality_from_wire(quality: SampleQualityWire) -> SampleQuality {
    match quality {
        SampleQualityWire::Good => SampleQuality::Good,
        SampleQualityWire::Uncertain => SampleQuality::Uncertain,
        SampleQualityWire::Substituted => SampleQuality::Substituted,
        SampleQualityWire::Missing => SampleQuality::Missing,
    }
}

const fn segment_kind_to_wire(kind: SegmentKind) -> SegmentKindWire {
    match kind {
        SegmentKind::History => SegmentKindWire::History,
        SegmentKind::FutureCovariates => SegmentKindWire::FutureCovariates,
        SegmentKind::StaticFeatures => SegmentKindWire::StaticFeatures,
    }
}

const fn segment_kind_from_wire(kind: SegmentKindWire) -> SegmentKind {
    match kind {
        SegmentKindWire::History => SegmentKind::History,
        SegmentKindWire::FutureCovariates => SegmentKind::FutureCovariates,
        SegmentKindWire::StaticFeatures => SegmentKind::StaticFeatures,
    }
}

const fn source_kind_to_wire(kind: SourceKind) -> SourceKindWire {
    match kind {
        SourceKind::History => SourceKindWire::History,
        SourceKind::Live => SourceKindWire::Live,
        SourceKind::HistoryAndLive => SourceKindWire::HistoryAndLive,
        SourceKind::Covariate => SourceKindWire::Covariate,
        SourceKind::Calendar => SourceKindWire::Calendar,
        SourceKind::Constant => SourceKindWire::Constant,
    }
}

const fn source_kind_from_wire(kind: SourceKindWire) -> SourceKind {
    match kind {
        SourceKindWire::History => SourceKind::History,
        SourceKindWire::Live => SourceKind::Live,
        SourceKindWire::HistoryAndLive => SourceKind::HistoryAndLive,
        SourceKindWire::Covariate => SourceKind::Covariate,
        SourceKindWire::Calendar => SourceKind::Calendar,
        SourceKindWire::Constant => SourceKind::Constant,
    }
}

fn parse_timestamp(value: &str) -> Result<TimestampMs, CodecError> {
    let bytes = value.as_bytes();
    if bytes.len() < 20
        || !value.ends_with('Z')
        || bytes.get(4) != Some(&b'-')
        || bytes.get(7) != Some(&b'-')
        || bytes.get(10) != Some(&b'T')
        || !bytes[..4].iter().all(u8::is_ascii_digit)
    {
        return Err(CodecError::Contract(
            "timestamp must be RFC 3339 UTC ending in Z",
        ));
    }
    let parsed = DateTime::parse_from_rfc3339(value)
        .map_err(|_| CodecError::Contract("timestamp is not valid RFC 3339"))?;
    if parsed.offset().local_minus_utc() != 0 || parsed.timestamp_subsec_nanos() % 1_000_000 != 0 {
        return Err(CodecError::Contract(
            "timestamp must be UTC with millisecond precision",
        ));
    }
    let milliseconds = u64::try_from(parsed.timestamp_millis())
        .map_err(|_| CodecError::Contract("timestamp is outside the supported range"))?;
    Ok(TimestampMs::new(milliseconds))
}

fn format_timestamp(timestamp: TimestampMs) -> Result<String, CodecError> {
    let milliseconds = i64::try_from(timestamp.get())
        .map_err(|_| CodecError::Contract("timestamp is outside the supported range"))?;
    let timestamp = DateTime::<Utc>::from_timestamp_millis(milliseconds).ok_or(
        CodecError::Contract("timestamp is outside the supported range"),
    )?;
    Ok(timestamp.to_rfc3339_opts(SecondsFormat::AutoSi, true))
}

fn seconds_to_milliseconds(seconds: u64) -> Result<u64, CodecError> {
    if seconds == 0 {
        return Err(CodecError::Contract("duration seconds must be positive"));
    }
    seconds_to_milliseconds_allow_zero(seconds)
}

fn seconds_to_milliseconds_allow_zero(seconds: u64) -> Result<u64, CodecError> {
    seconds
        .checked_mul(1_000)
        .ok_or(CodecError::Contract("duration exceeds the supported range"))
}

fn milliseconds_to_seconds(milliseconds: u64) -> Result<u64, CodecError> {
    if !milliseconds.is_multiple_of(1_000) {
        return Err(CodecError::Contract("wire durations require whole seconds"));
    }
    Ok(milliseconds / 1_000)
}

fn finite(value: f64) -> Result<f64, CodecError> {
    if value.is_finite() {
        Ok(value)
    } else {
        Err(CodecError::Contract("numeric values must be finite"))
    }
}

fn usize_to_u64(value: usize) -> Result<u64, CodecError> {
    u64::try_from(value).map_err(|_| CodecError::Contract("count exceeds the supported range"))
}

fn u64_to_usize(value: u64) -> Result<usize, CodecError> {
    usize::try_from(value).map_err(|_| CodecError::Contract("count exceeds the supported range"))
}

fn validate_identifier(value: &str) -> Result<(), CodecError> {
    let mut characters = value.chars();
    let valid = value.len() <= 256
        && characters
            .next()
            .is_some_and(|character| character.is_ascii_alphanumeric())
        && characters.all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | ':' | '-')
        });
    if valid {
        Ok(())
    } else {
        Err(CodecError::Contract("identifier is invalid"))
    }
}

fn validate_feature_name(value: &str) -> Result<(), CodecError> {
    validate_text(value, 128)
}

fn validate_unit(value: &str) -> Result<(), CodecError> {
    validate_text(value, 64)
}

fn validate_text(value: &str, max_bytes: usize) -> Result<(), CodecError> {
    if value.is_empty()
        || value.len() > max_bytes
        || value.chars().any(|character| character.is_control())
    {
        Err(CodecError::Contract("text field is invalid"))
    } else {
        Ok(())
    }
}

fn validate_uuid(value: &str) -> Result<(), CodecError> {
    let bytes = value.as_bytes();
    let valid = bytes.len() == 36
        && bytes.iter().enumerate().all(|(index, byte)| match index {
            8 | 13 | 18 | 23 => *byte == b'-',
            _ => byte.is_ascii_hexdigit(),
        })
        && matches!(bytes[14], b'1'..=b'8')
        && matches!(bytes[19].to_ascii_lowercase(), b'8' | b'9' | b'a' | b'b');
    if valid {
        Ok(())
    } else {
        Err(CodecError::Contract("request identifier must be a UUID"))
    }
}

fn validate_digest(value: &str) -> Result<(), CodecError> {
    let bytes = value.as_bytes();
    if bytes.len() == 71
        && value.starts_with("sha256:")
        && bytes[7..]
            .iter()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    {
        Ok(())
    } else {
        Err(CodecError::Contract("SHA-256 digest is invalid"))
    }
}

fn validate_stable_codes(values: &[String]) -> Result<(), CodecError> {
    if values.len() > MAX_WARNINGS {
        return Err(CodecError::Contract("warning count exceeds the limit"));
    }
    if contains_duplicates(values) {
        return Err(CodecError::Contract("stable codes must be unique"));
    }
    for value in values {
        validate_stable_code(value)?;
    }
    Ok(())
}

fn validate_stable_code(value: &str) -> Result<(), CodecError> {
    let bytes = value.as_bytes();
    if !bytes.is_empty()
        && bytes.len() <= 128
        && bytes[0].is_ascii_uppercase()
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || *byte == b'_')
    {
        Ok(())
    } else {
        Err(CodecError::Contract("stable code is invalid"))
    }
}

fn contains_duplicates<T: PartialEq>(values: &[T]) -> bool {
    values
        .iter()
        .enumerate()
        .any(|(index, value)| values[..index].contains(value))
}
