//! Validated conversion of the bundled load-forecast asset into Aether types.

use aether_sdk::domain::{
    DataProcessingTask, FallbackPolicy, FeatureDefinition, FeatureRole, ForecastTarget,
    ForecastTaskSpec, HistoryAggregation, HistoryDuplicatePolicy, HistoryFeaturePolicy,
    NumericFeatureConstraints, TaskIdentity,
};
use serde::Deserialize;

use crate::EnergyGatewayError;

const LOAD_TASK_ASSET: &str = "packs/energy/data-processing/tasks/site-load-forecast.yaml";
const MILLISECONDS_PER_SECOND: u64 = 1_000;

/// Safe, typed task contract and route limits loaded from the bundled energy pack.
///
/// Loading this value does not enable a task, commission a binding, select an
/// artifact, or authorize remote egress. A composition root must still provide
/// a commissioned [`aether_sdk::application::DataProcessingBinding`] and a
/// processor that satisfies these limits.
#[derive(Debug, Clone, PartialEq)]
pub struct LoadForecastContract {
    task: DataProcessingTask,
    deadline_ms: u64,
    max_attempts: u32,
    max_request_bytes: usize,
    max_input_age_ms: u64,
    max_gap_ms: u64,
    fallback_max_expires_after_ms: u64,
    maximum_frame_samples: usize,
    remote_egress_allowed: bool,
}

impl LoadForecastContract {
    pub(crate) fn from_yaml(contents: &str) -> Result<Self, EnergyGatewayError> {
        let asset: LoadForecastTaskAsset =
            serde_yml::from_str(contents).map_err(|error| EnergyGatewayError::InvalidAsset {
                asset: LOAD_TASK_ASSET,
                message: error.to_string(),
            })?;
        asset.into_contract()
    }

    /// Returns the portable data-processing task assembled from the YAML contract.
    #[must_use]
    pub const fn task(&self) -> &DataProcessingTask {
        &self.task
    }

    /// Consumes the loaded contract and returns its portable task.
    #[must_use]
    pub fn into_task(self) -> DataProcessingTask {
        self.task
    }

    /// Returns the frame-assembly and processor-work deadline declared by the bundled task.
    #[must_use]
    pub const fn deadline_ms(&self) -> u64 {
        self.deadline_ms
    }

    /// Returns the single-attempt execution bound.
    #[must_use]
    pub const fn max_attempts(&self) -> u32 {
        self.max_attempts
    }

    /// Returns the maximum encoded processor request size.
    #[must_use]
    pub const fn max_request_bytes(&self) -> usize {
        self.max_request_bytes
    }

    /// Returns the maximum age of the newest accepted input observation.
    #[must_use]
    pub const fn max_input_age_ms(&self) -> u64 {
        self.max_input_age_ms
    }

    /// Returns the largest accepted gap between historical samples.
    #[must_use]
    pub const fn max_gap_ms(&self) -> u64 {
        self.max_gap_ms
    }

    /// Returns the maximum lifetime of the approved persistence fallback.
    #[must_use]
    pub const fn fallback_max_expires_after_ms(&self) -> u64 {
        self.fallback_max_expires_after_ms
    }

    /// Returns the maximum number of time-series cells in a full task frame.
    #[must_use]
    pub const fn maximum_frame_samples(&self) -> usize {
        self.maximum_frame_samples
    }

    /// Returns whether this bundled route policy permits remote processor egress.
    #[must_use]
    pub const fn remote_egress_allowed(&self) -> bool {
        self.remote_egress_allowed
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct LoadForecastTaskAsset {
    schema: String,
    id: String,
    revision: u32,
    enabled: bool,
    kind: String,
    processor_contract: String,
    description: String,
    target: TargetAsset,
    frame: FrameAsset,
    inputs: InputsAsset,
    alignment: AlignmentAsset,
    quality: QualityAsset,
    artifact_policy: ArtifactPolicyAsset,
    fallback: FallbackAsset,
    execution: ExecutionAsset,
    governance: GovernanceAsset,
    output: OutputAsset,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TargetAsset {
    name: String,
    semantic_point: String,
    value_type: String,
    unit: String,
    sign_convention: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FrameAsset {
    cadence_seconds: u64,
    history_steps: usize,
    horizon_steps: usize,
    timezone: String,
    live_tail: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct InputsAsset {
    history: Vec<FeatureAsset>,
    future_covariates: Vec<FeatureAsset>,
    static_features: Vec<serde_yml::Value>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FeatureAsset {
    name: String,
    value_type: String,
    unit: String,
    required: bool,
    #[serde(default)]
    known_ahead: Option<bool>,
    #[serde(default)]
    range: Option<RangeAsset>,
    #[serde(default)]
    interval_semantics: Option<String>,
    source: SourceAsset,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RangeAsset {
    minimum: Option<f64>,
    maximum: Option<f64>,
    integer: Option<bool>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SourceAsset {
    kind: String,
    #[serde(default)]
    instance_ref: Option<String>,
    #[serde(default)]
    point_ref: Option<String>,
    #[serde(default)]
    dataset_ref: Option<String>,
    #[serde(default)]
    field: Option<String>,
    #[serde(default)]
    transform: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AlignmentAsset {
    aggregation: String,
    timestamp_semantics: String,
    duplicate_policy: String,
    feature_policies: Vec<HistoryPolicyAsset>,
    gap_policy: String,
    missing_policy: String,
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct HistoryPolicyAsset {
    name: String,
    aggregation: String,
    duplicate_policy: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct QualityAsset {
    max_input_age_seconds: u64,
    max_gap_seconds: u64,
    max_missing_ratio: f64,
    require_input_watermark: bool,
    require_covariate_issue_time_not_after_as_of: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ArtifactPolicyAsset {
    required: bool,
    allowed_kinds: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FallbackAsset {
    allowed: bool,
    strategies: Vec<FallbackStrategyAsset>,
    synthetic_zero_baseline: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FallbackStrategyAsset {
    name: String,
    version: String,
    source_feature: String,
    max_expires_after_seconds: u64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExecutionAsset {
    deadline_ms: u64,
    max_attempts: u32,
    correlation_key: String,
    max_request_bytes: usize,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct GovernanceAsset {
    data_classification: String,
    remote_egress: String,
    device_effect: String,
    audit_raw_values: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct OutputAsset {
    schema: String,
    value_type: String,
    unit: String,
    sign_convention: String,
    cadence_seconds: u64,
    timestamp_semantics: String,
    max_points: usize,
    max_quantiles: usize,
    expires_after_seconds: u64,
    publication: Vec<String>,
    retention: String,
}

#[derive(Clone, Copy)]
struct ExpectedFeature {
    name: &'static str,
    unit: &'static str,
    range: Option<(Option<f64>, Option<f64>, Option<bool>)>,
    interval_semantics: Option<&'static str>,
    source: ExpectedSource,
}

#[derive(Clone, Copy)]
enum ExpectedSource {
    Measurement {
        instance_ref: &'static str,
        point_ref: &'static str,
    },
    Covariate {
        dataset_ref: &'static str,
        field: &'static str,
    },
    Calendar {
        transform: &'static str,
    },
}

const HISTORY_FEATURES: [ExpectedFeature; 5] = [
    ExpectedFeature {
        name: "load",
        unit: "kW",
        range: None,
        interval_semantics: None,
        source: ExpectedSource::Measurement {
            instance_ref: "site_load",
            point_ref: "active_power",
        },
    },
    ExpectedFeature {
        name: "temp_avg",
        unit: "Cel",
        range: None,
        interval_semantics: None,
        source: ExpectedSource::Covariate {
            dataset_ref: "weather.observed",
            field: "air_temperature",
        },
    },
    ExpectedFeature {
        name: "humidity",
        unit: "%",
        range: Some((Some(0.0), Some(100.0), None)),
        interval_semantics: None,
        source: ExpectedSource::Covariate {
            dataset_ref: "weather.observed",
            field: "relative_humidity",
        },
    },
    ExpectedFeature {
        name: "rain",
        unit: "mm",
        range: Some((Some(0.0), None, None)),
        interval_semantics: Some("accumulation_over_cadence"),
        source: ExpectedSource::Covariate {
            dataset_ref: "weather.observed",
            field: "precipitation",
        },
    },
    ExpectedFeature {
        name: "quarter_hour",
        unit: "1",
        range: Some((Some(0.0), Some(95.0), Some(true))),
        interval_semantics: None,
        source: ExpectedSource::Calendar {
            transform: "quarter_hour_of_day_zero_based",
        },
    },
];

const FUTURE_FEATURES: [ExpectedFeature; 4] = [
    ExpectedFeature {
        name: "temp_avg",
        unit: "Cel",
        range: None,
        interval_semantics: None,
        source: ExpectedSource::Covariate {
            dataset_ref: "weather.nwp",
            field: "air_temperature",
        },
    },
    ExpectedFeature {
        name: "humidity",
        unit: "%",
        range: Some((Some(0.0), Some(100.0), None)),
        interval_semantics: None,
        source: ExpectedSource::Covariate {
            dataset_ref: "weather.nwp",
            field: "relative_humidity",
        },
    },
    ExpectedFeature {
        name: "rain",
        unit: "mm",
        range: Some((Some(0.0), None, None)),
        interval_semantics: Some("accumulation_over_cadence"),
        source: ExpectedSource::Covariate {
            dataset_ref: "weather.nwp",
            field: "precipitation",
        },
    },
    ExpectedFeature {
        name: "quarter_hour",
        unit: "1",
        range: Some((Some(0.0), Some(95.0), Some(true))),
        interval_semantics: None,
        source: ExpectedSource::Calendar {
            transform: "quarter_hour_of_day_zero_based",
        },
    },
];

impl LoadForecastTaskAsset {
    fn into_contract(self) -> Result<LoadForecastContract, EnergyGatewayError> {
        require(
            self.schema == "aether.data-processing-task"
                && self.id == "energy.site-load-forecast"
                && self.revision == 1
                && !self.enabled
                && self.kind == "forecast"
                && self.processor_contract == "aether.data-processing.forecast"
                && !self.description.trim().is_empty(),
            "load task identity, schema, or safe disabled default is invalid",
        )?;
        require(
            self.target.name == "load"
                && self.target.semantic_point == "site.load.active_power"
                && self.target.value_type == "number"
                && self.target.unit == "kW"
                && self.target.sign_convention == "positive_consumption",
            "load forecast target semantics are invalid",
        )?;
        require(
            self.frame.cadence_seconds == 900
                && self.frame.history_steps == 672
                && self.frame.horizon_steps == 288
                && self.frame.timezone == "UTC"
                && self.frame.live_tail == "forbidden",
            "load forecast frame policy is invalid",
        )?;
        validate_features(&self.inputs.history, &HISTORY_FEATURES, false)?;
        validate_features(&self.inputs.future_covariates, &FUTURE_FEATURES, true)?;
        require(
            self.inputs.static_features.is_empty(),
            "load forecast must not acquire undeclared static features",
        )?;
        require(
            self.alignment.aggregation == "mean"
                && self.alignment.timestamp_semantics == "interval_end"
                && self.alignment.duplicate_policy == "latest"
                && self.alignment.feature_policies
                    == [
                        HistoryPolicyAsset {
                            name: "load".to_string(),
                            aggregation: "mean".to_string(),
                            duplicate_policy: "latest".to_string(),
                        },
                        HistoryPolicyAsset {
                            name: "temp_avg".to_string(),
                            aggregation: "mean".to_string(),
                            duplicate_policy: "latest".to_string(),
                        },
                        HistoryPolicyAsset {
                            name: "humidity".to_string(),
                            aggregation: "mean".to_string(),
                            duplicate_policy: "latest".to_string(),
                        },
                        HistoryPolicyAsset {
                            name: "rain".to_string(),
                            aggregation: "sum".to_string(),
                            duplicate_policy: "latest".to_string(),
                        },
                        HistoryPolicyAsset {
                            name: "quarter_hour".to_string(),
                            aggregation: "last".to_string(),
                            duplicate_policy: "reject".to_string(),
                        },
                    ]
                && self.alignment.gap_policy == "reject"
                && self.alignment.missing_policy == "reject",
            "load forecast alignment policy is invalid",
        )?;
        require(
            self.quality.max_input_age_seconds == 900
                && self.quality.max_gap_seconds == 1_800
                && self.quality.max_missing_ratio == 0.0
                && self.quality.require_input_watermark
                && self.quality.require_covariate_issue_time_not_after_as_of,
            "load forecast quality policy is invalid",
        )?;
        require(
            !self.artifact_policy.required && self.artifact_policy.allowed_kinds == ["model"],
            "load forecast artifact policy is invalid",
        )?;
        require(
            self.fallback.allowed
                && self.fallback.synthetic_zero_baseline == "forbidden"
                && self.fallback.strategies.len() == 1,
            "load forecast fallback policy is invalid",
        )?;
        let fallback = self
            .fallback
            .strategies
            .first()
            .ok_or_else(|| unsafe_pack("load forecast persistence fallback is absent"))?;
        require(
            fallback.name == "persistence"
                && fallback.version == "1"
                && fallback.source_feature == "load"
                && fallback.max_expires_after_seconds == 1_800,
            "load forecast persistence fallback is invalid",
        )?;
        require(
            self.execution.deadline_ms == 5_000
                && self.execution.max_attempts == 1
                && self.execution.correlation_key == "input_digest"
                && self.execution.max_request_bytes == 4_194_304,
            "load forecast execution limits are invalid",
        )?;
        require(
            self.governance.data_classification == "operational_telemetry"
                && self.governance.remote_egress == "denied"
                && self.governance.device_effect == "none"
                && !self.governance.audit_raw_values,
            "load forecast governance defaults are unsafe",
        )?;
        require(
            self.output.schema == "aether.data-processing.output.forecast"
                && self.output.value_type == "number"
                && self.output.unit == self.target.unit
                && self.output.sign_convention == self.target.sign_convention
                && self.output.cadence_seconds == self.frame.cadence_seconds
                && self.output.timestamp_semantics == "interval_end"
                && self.output.max_points == self.frame.horizon_steps
                && self.output.max_quantiles == 0
                && self.output.expires_after_seconds == 3_600
                && self.output.publication == ["query_response"]
                && self.output.retention == "none",
            "load forecast output contract is invalid",
        )?;

        let cadence_ms = seconds_to_ms(self.frame.cadence_seconds, "forecast cadence")?;
        let max_output_age_ms = seconds_to_ms(self.output.expires_after_seconds, "output age")?;
        let max_input_age_ms = seconds_to_ms(self.quality.max_input_age_seconds, "input age")?;
        let max_gap_ms = seconds_to_ms(self.quality.max_gap_seconds, "input gap")?;
        let fallback_max_expires_after_ms = seconds_to_ms(
            fallback.max_expires_after_seconds,
            "fallback output lifetime",
        )?;
        let maximum_frame_samples = self
            .inputs
            .history
            .len()
            .checked_mul(self.frame.history_steps)
            .and_then(|history| {
                self.inputs
                    .future_covariates
                    .len()
                    .checked_mul(self.frame.horizon_steps)
                    .and_then(|future| history.checked_add(future))
            })
            .ok_or_else(|| unsafe_pack("load forecast frame bound overflows"))?;
        let features = self
            .inputs
            .history
            .iter()
            .map(|feature| numeric_feature(feature, FeatureRole::History))
            .chain(
                self.inputs
                    .future_covariates
                    .iter()
                    .map(|feature| numeric_feature(feature, FeatureRole::FutureCovariate)),
            )
            .collect::<Result<Vec<_>, _>>()?;
        let target = ForecastTarget::new(
            self.target.name,
            self.target.unit,
            self.target.sign_convention,
        )
        .map_err(|_| unsafe_pack("load forecast target cannot be represented"))?;
        let allowed_fallbacks = self
            .fallback
            .strategies
            .iter()
            .map(|strategy| strategy.name.clone())
            .collect();
        let forecast_spec = ForecastTaskSpec::new(
            target,
            cadence_ms,
            HistoryAggregation::Mean,
            HistoryDuplicatePolicy::Latest,
            self.frame.history_steps,
            self.frame.horizon_steps,
            max_output_age_ms,
            self.quality.max_missing_ratio,
            allowed_fallbacks,
        )
        .and_then(|spec| spec.with_input_quality_limits(max_input_age_ms, max_gap_ms))
        .and_then(|spec| {
            spec.with_history_feature_policies(vec![
                HistoryFeaturePolicy::new(
                    "load",
                    HistoryAggregation::Mean,
                    HistoryDuplicatePolicy::Latest,
                )?,
                HistoryFeaturePolicy::new(
                    "temp_avg",
                    HistoryAggregation::Mean,
                    HistoryDuplicatePolicy::Latest,
                )?,
                HistoryFeaturePolicy::new(
                    "humidity",
                    HistoryAggregation::Mean,
                    HistoryDuplicatePolicy::Latest,
                )?,
                HistoryFeaturePolicy::new(
                    "rain",
                    HistoryAggregation::Sum,
                    HistoryDuplicatePolicy::Latest,
                )?,
                HistoryFeaturePolicy::new(
                    "quarter_hour",
                    HistoryAggregation::Last,
                    HistoryDuplicatePolicy::Reject,
                )?,
            ])
        })
        .and_then(|spec| {
            spec.with_fallback_policies(vec![FallbackPolicy::new(
                fallback.name.clone(),
                fallback.version.clone(),
                fallback.source_feature.clone(),
                fallback_max_expires_after_ms,
            )?])
        })
        .map(ForecastTaskSpec::requiring_future_issue_time)
        .map_err(|_| unsafe_pack("load forecast policy cannot be represented"))?;
        let identity = TaskIdentity::new(self.id, self.revision)
            .map_err(|_| unsafe_pack("load forecast identity cannot be represented"))?;
        let task = DataProcessingTask::forecast(
            identity,
            self.processor_contract,
            features,
            forecast_spec,
        )
        .map_err(|_| unsafe_pack("load forecast task cannot be represented"))?;

        Ok(LoadForecastContract {
            task,
            deadline_ms: self.execution.deadline_ms,
            max_attempts: self.execution.max_attempts,
            max_request_bytes: self.execution.max_request_bytes,
            max_input_age_ms,
            max_gap_ms,
            fallback_max_expires_after_ms,
            maximum_frame_samples,
            remote_egress_allowed: false,
        })
    }
}

fn validate_features(
    actual: &[FeatureAsset],
    expected: &[ExpectedFeature],
    known_ahead: bool,
) -> Result<(), EnergyGatewayError> {
    require(
        actual.len() == expected.len(),
        "load forecast feature count is invalid",
    )?;
    for (feature, expected) in actual.iter().zip(expected) {
        require(
            feature.name == expected.name
                && feature.value_type == "number"
                && feature.unit == expected.unit
                && feature.required
                && feature.known_ahead == known_ahead.then_some(true)
                && feature.interval_semantics.as_deref() == expected.interval_semantics,
            "load forecast feature name, type, unit, or required policy is invalid",
        )?;
        let actual_range = feature
            .range
            .as_ref()
            .map(|range| (range.minimum, range.maximum, range.integer));
        require(
            actual_range == expected.range,
            "load forecast feature range is invalid",
        )?;
        validate_source(&feature.source, expected.source)?;
    }
    Ok(())
}

fn validate_source(
    actual: &SourceAsset,
    expected: ExpectedSource,
) -> Result<(), EnergyGatewayError> {
    let valid = match expected {
        ExpectedSource::Measurement {
            instance_ref,
            point_ref,
        } => {
            actual.kind == "measurement"
                && actual.instance_ref.as_deref() == Some(instance_ref)
                && actual.point_ref.as_deref() == Some(point_ref)
                && actual.dataset_ref.is_none()
                && actual.field.is_none()
                && actual.transform.is_none()
        },
        ExpectedSource::Covariate { dataset_ref, field } => {
            actual.kind == "covariate"
                && actual.dataset_ref.as_deref() == Some(dataset_ref)
                && actual.field.as_deref() == Some(field)
                && actual.instance_ref.is_none()
                && actual.point_ref.is_none()
                && actual.transform.is_none()
        },
        ExpectedSource::Calendar { transform } => {
            actual.kind == "calendar"
                && actual.transform.as_deref() == Some(transform)
                && actual.instance_ref.is_none()
                && actual.point_ref.is_none()
                && actual.dataset_ref.is_none()
                && actual.field.is_none()
        },
    };
    require(valid, "load forecast logical feature source is invalid")
}

fn numeric_feature(
    feature: &FeatureAsset,
    role: FeatureRole,
) -> Result<FeatureDefinition, EnergyGatewayError> {
    let mut definition = FeatureDefinition::numeric(&feature.name, role, &feature.unit)
        .map_err(|_| unsafe_pack("load forecast feature cannot be represented"))?;
    if let Some(range) = &feature.range {
        definition = definition
            .with_numeric_constraints(
                NumericFeatureConstraints::new(
                    range.minimum,
                    range.maximum,
                    range.integer.unwrap_or(false),
                )
                .map_err(|_| unsafe_pack("load forecast feature range is invalid"))?,
            )
            .map_err(|_| unsafe_pack("load forecast feature constraints are invalid"))?;
    }
    Ok(definition)
}

fn seconds_to_ms(seconds: u64, field: &'static str) -> Result<u64, EnergyGatewayError> {
    seconds
        .checked_mul(MILLISECONDS_PER_SECOND)
        .ok_or_else(|| unsafe_pack(&format!("{field} overflows milliseconds")))
}

fn require(condition: bool, message: &'static str) -> Result<(), EnergyGatewayError> {
    if condition {
        Ok(())
    } else {
        Err(unsafe_pack(message))
    }
}

fn unsafe_pack(message: &str) -> EnergyGatewayError {
    EnergyGatewayError::UnsafePack(message.to_string())
}
