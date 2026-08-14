//! Industry-neutral, request-driven data-processing domain contracts.

use alloc::string::String;
use alloc::vec::Vec;

use crate::{DomainError, TimestampMs};

fn nonempty(value: impl Into<String>) -> Result<String, DomainError> {
    let value = value.into();
    if value.trim().is_empty() {
        return Err(DomainError::EmptyIdentifier);
    }
    Ok(value)
}

fn timestamps_are_strictly_increasing(timestamps: &[TimestampMs]) -> bool {
    timestamps.windows(2).all(|pair| pair[0] < pair[1])
}

fn strings_are_unique(values: &[String]) -> bool {
    values
        .iter()
        .enumerate()
        .all(|(index, value)| !values[..index].iter().any(|seen| seen == value))
}

fn is_sha256_digest(value: &str) -> bool {
    value.strip_prefix("sha256:").is_some_and(|hex| {
        hex.len() == 64
            && hex
                .as_bytes()
                .iter()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(byte))
    })
}

/// Identity of a declarative data-processing task.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TaskIdentity {
    id: String,
    revision: u32,
}

impl TaskIdentity {
    /// Creates a non-empty task identity with a positive revision.
    pub fn new(id: impl Into<String>, revision: u32) -> Result<Self, DomainError> {
        if revision == 0 {
            return Err(DomainError::ZeroRevision);
        }
        Ok(Self {
            id: nonempty(id)?,
            revision,
        })
    }

    /// Returns the stable logical task identifier.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Returns the task revision.
    #[must_use]
    pub const fn revision(&self) -> u32 {
        self.revision
    }
}

/// Identity of a commissioned task binding.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BindingIdentity {
    id: String,
    revision: u32,
}

impl BindingIdentity {
    /// Creates a non-empty binding identity with a positive revision.
    pub fn new(id: impl Into<String>, revision: u32) -> Result<Self, DomainError> {
        if revision == 0 {
            return Err(DomainError::ZeroRevision);
        }
        Ok(Self {
            id: nonempty(id)?,
            revision,
        })
    }

    /// Returns the stable logical binding identifier.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Returns the binding revision.
    #[must_use]
    pub const fn revision(&self) -> u32 {
        self.revision
    }
}

/// Typed data-processing task kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TaskKind {
    /// Produce a future time-indexed series.
    Forecast,
}

/// Role of a feature in a processing frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FeatureRole {
    /// Historical observation at or before the frame cutoff.
    History,
    /// Known-future covariate after the frame cutoff.
    FutureCovariate,
    /// Non-series context fixed for the execution.
    Static,
}

/// Logical scalar type accepted by a feature.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FeatureValueType {
    /// Finite floating-point number.
    Number,
    /// UTF-8 text.
    Text,
    /// Boolean value.
    Boolean,
}

/// Optional task-owned validation limits for a numeric feature.
#[derive(Debug, Clone, PartialEq)]
pub struct NumericFeatureConstraints {
    minimum: Option<f64>,
    maximum: Option<f64>,
    integer: bool,
}

impl Eq for NumericFeatureConstraints {}

impl NumericFeatureConstraints {
    /// Creates finite, ordered numeric limits.
    pub fn new(
        minimum: Option<f64>,
        maximum: Option<f64>,
        integer: bool,
    ) -> Result<Self, DomainError> {
        if minimum.is_some_and(|value| !value.is_finite())
            || maximum.is_some_and(|value| !value.is_finite())
            || minimum.zip(maximum).is_some_and(|(min, max)| min > max)
        {
            return Err(DomainError::InvalidFrameQuality);
        }
        Ok(Self {
            minimum,
            maximum,
            integer,
        })
    }

    /// Returns the inclusive minimum.
    #[must_use]
    pub const fn minimum(&self) -> Option<f64> {
        self.minimum
    }

    /// Returns the inclusive maximum.
    #[must_use]
    pub const fn maximum(&self) -> Option<f64> {
        self.maximum
    }

    /// Returns whether values must be mathematical integers.
    #[must_use]
    pub const fn integer(&self) -> bool {
        self.integer
    }

    /// Returns whether one finite value satisfies the limits.
    #[must_use]
    pub fn accepts(&self, value: f64) -> bool {
        value.is_finite()
            && self.minimum.is_none_or(|minimum| value >= minimum)
            && self.maximum.is_none_or(|maximum| value <= maximum)
            && (!self.integer || value % 1.0 == 0.0)
    }
}

/// Portable declaration of one semantically named input feature.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeatureDefinition {
    name: String,
    role: FeatureRole,
    value_type: FeatureValueType,
    unit: Option<String>,
    numeric_constraints: Option<NumericFeatureConstraints>,
}

impl FeatureDefinition {
    /// Creates a unitless feature declaration.
    pub fn new(
        name: impl Into<String>,
        role: FeatureRole,
        value_type: FeatureValueType,
    ) -> Result<Self, DomainError> {
        Ok(Self {
            name: nonempty(name)?,
            role,
            value_type,
            unit: None,
            numeric_constraints: None,
        })
    }

    /// Creates a numeric feature with an explicit engineering unit.
    pub fn numeric(
        name: impl Into<String>,
        role: FeatureRole,
        unit: impl Into<String>,
    ) -> Result<Self, DomainError> {
        Ok(Self {
            name: nonempty(name)?,
            role,
            value_type: FeatureValueType::Number,
            unit: Some(nonempty(unit)?),
            numeric_constraints: None,
        })
    }

    /// Adds task-owned numeric value limits.
    pub fn with_numeric_constraints(
        mut self,
        constraints: NumericFeatureConstraints,
    ) -> Result<Self, DomainError> {
        if self.value_type != FeatureValueType::Number {
            return Err(DomainError::FeatureTypeMismatch);
        }
        self.numeric_constraints = Some(constraints);
        Ok(self)
    }

    /// Returns the task-local feature name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the feature role.
    #[must_use]
    pub const fn role(&self) -> FeatureRole {
        self.role
    }

    /// Returns the feature value type.
    #[must_use]
    pub const fn value_type(&self) -> FeatureValueType {
        self.value_type
    }

    /// Returns the engineering unit for numeric values.
    #[must_use]
    pub fn unit(&self) -> Option<&str> {
        self.unit.as_deref()
    }

    /// Returns task-owned numeric limits when declared.
    #[must_use]
    pub const fn numeric_constraints(&self) -> Option<&NumericFeatureConstraints> {
        self.numeric_constraints.as_ref()
    }
}

/// Forecast target semantics used to validate input and output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForecastTarget {
    name: String,
    unit: String,
    sign_convention: String,
}

impl ForecastTarget {
    /// Creates target semantics.
    pub fn new(
        name: impl Into<String>,
        unit: impl Into<String>,
        sign_convention: impl Into<String>,
    ) -> Result<Self, DomainError> {
        Ok(Self {
            name: nonempty(name)?,
            unit: nonempty(unit)?,
            sign_convention: nonempty(sign_convention)?,
        })
    }

    /// Returns the target feature name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the canonical output unit.
    #[must_use]
    pub fn unit(&self) -> &str {
        &self.unit
    }

    /// Returns the canonical sign convention.
    #[must_use]
    pub fn sign_convention(&self) -> &str {
        &self.sign_convention
    }
}

/// Portable task policy required to assemble and validate a forecast.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistoryAggregation {
    /// Arithmetic mean of usable raw observations in each interval.
    Mean,
    /// Latest usable raw observation in each interval.
    Last,
    /// Sum of usable raw observations in each interval.
    Sum,
    /// Minimum usable raw observation in each interval.
    Min,
    /// Maximum usable raw observation in each interval.
    Max,
}

/// Task-owned handling of multiple raw rows with the same source timestamp.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistoryDuplicatePolicy {
    /// Keep the last ingested row at a timestamp before interval aggregation.
    Latest,
    /// Reject a source window containing duplicate timestamps.
    Reject,
}

/// Task-owned raw-history policy for one semantic feature.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryFeaturePolicy {
    feature: String,
    aggregation: HistoryAggregation,
    duplicate_policy: HistoryDuplicatePolicy,
}

impl HistoryFeaturePolicy {
    /// Creates an explicit per-feature history policy.
    pub fn new(
        feature: impl Into<String>,
        aggregation: HistoryAggregation,
        duplicate_policy: HistoryDuplicatePolicy,
    ) -> Result<Self, DomainError> {
        Ok(Self {
            feature: nonempty(feature)?,
            aggregation,
            duplicate_policy,
        })
    }

    /// Returns the task-local feature name.
    #[must_use]
    pub fn feature(&self) -> &str {
        &self.feature
    }

    /// Returns interval aggregation for this feature.
    #[must_use]
    pub const fn aggregation(&self) -> HistoryAggregation {
        self.aggregation
    }

    /// Returns duplicate handling for this feature.
    #[must_use]
    pub const fn duplicate_policy(&self) -> HistoryDuplicatePolicy {
        self.duplicate_policy
    }
}

/// Portable task policy required to assemble and validate a forecast.
#[derive(Debug, Clone, PartialEq)]
pub struct ForecastTaskSpec {
    target: ForecastTarget,
    cadence_ms: u64,
    history_aggregation: HistoryAggregation,
    history_duplicate_policy: HistoryDuplicatePolicy,
    history_feature_policies: Vec<HistoryFeaturePolicy>,
    history_steps: usize,
    max_horizon_steps: usize,
    max_quantiles: usize,
    max_output_age_ms: u64,
    max_missing_ratio: f64,
    allowed_fallbacks: Vec<String>,
    fallback_policies: Vec<FallbackPolicy>,
    max_input_age_ms: Option<u64>,
    max_gap_ms: Option<u64>,
    require_future_issue_time: bool,
}

impl ForecastTaskSpec {
    /// Creates a validated forecast task policy.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        target: ForecastTarget,
        cadence_ms: u64,
        history_aggregation: HistoryAggregation,
        history_duplicate_policy: HistoryDuplicatePolicy,
        history_steps: usize,
        max_horizon_steps: usize,
        max_output_age_ms: u64,
        max_missing_ratio: f64,
        allowed_fallbacks: Vec<String>,
    ) -> Result<Self, DomainError> {
        if cadence_ms == 0 || history_steps == 0 || max_horizon_steps == 0 || max_output_age_ms == 0
        {
            return Err(DomainError::InvalidProcessingWindow);
        }
        if !max_missing_ratio.is_finite() || !(0.0..=1.0).contains(&max_missing_ratio) {
            return Err(DomainError::InvalidFrameQuality);
        }
        let allowed_fallbacks = allowed_fallbacks
            .into_iter()
            .map(nonempty)
            .collect::<Result<Vec<_>, _>>()?;
        if !strings_are_unique(&allowed_fallbacks) {
            return Err(DomainError::InvalidProcessingState);
        }

        Ok(Self {
            target,
            cadence_ms,
            history_aggregation,
            history_duplicate_policy,
            history_feature_policies: Vec::new(),
            history_steps,
            max_horizon_steps,
            max_quantiles: 0,
            max_output_age_ms,
            max_missing_ratio,
            allowed_fallbacks,
            fallback_policies: Vec::new(),
            max_input_age_ms: None,
            max_gap_ms: None,
            require_future_issue_time: false,
        })
    }

    /// Adds task-owned freshness and historical-gap limits.
    pub fn with_input_quality_limits(
        mut self,
        max_input_age_ms: u64,
        max_gap_ms: u64,
    ) -> Result<Self, DomainError> {
        if max_input_age_ms == 0 || max_gap_ms < self.cadence_ms {
            return Err(DomainError::InvalidFrameQuality);
        }
        self.max_input_age_ms = Some(max_input_age_ms);
        self.max_gap_ms = Some(max_gap_ms);
        Ok(self)
    }

    /// Adds complete acceptance policies for every named fallback.
    ///
    /// Once this builder is used, every allowed fallback must have exactly one
    /// policy, including its provider identity, source, and lifetime limits.
    pub fn with_fallback_policies(
        mut self,
        policies: Vec<FallbackPolicy>,
    ) -> Result<Self, DomainError> {
        if policies.len() != self.allowed_fallbacks.len()
            || policies.iter().any(|policy| {
                policy.max_output_age_ms > self.max_output_age_ms
                    || !self
                        .allowed_fallbacks
                        .iter()
                        .any(|strategy| strategy == &policy.strategy)
            })
            || policies.iter().enumerate().any(|(index, policy)| {
                policies[..index]
                    .iter()
                    .any(|seen| seen.strategy == policy.strategy)
            })
        {
            return Err(DomainError::InvalidProcessingState);
        }
        self.fallback_policies = policies;
        Ok(self)
    }

    /// Requires issue-time provenance for non-deterministic future inputs.
    #[must_use]
    pub const fn requiring_future_issue_time(mut self) -> Self {
        self.require_future_issue_time = true;
        self
    }

    /// Returns the target semantics.
    #[must_use]
    pub const fn target(&self) -> &ForecastTarget {
        &self.target
    }

    /// Returns the expected sample cadence.
    #[must_use]
    pub const fn cadence_ms(&self) -> u64 {
        self.cadence_ms
    }

    /// Returns the task-owned interval aggregation for stored history inputs.
    #[must_use]
    pub const fn history_aggregation(&self) -> HistoryAggregation {
        self.history_aggregation
    }

    /// Returns how duplicate raw source timestamps are resolved before aggregation.
    #[must_use]
    pub const fn history_duplicate_policy(&self) -> HistoryDuplicatePolicy {
        self.history_duplicate_policy
    }

    /// Overrides the default history policy with an exact per-feature policy set.
    pub fn with_history_feature_policies(
        mut self,
        policies: Vec<HistoryFeaturePolicy>,
    ) -> Result<Self, DomainError> {
        if policies.is_empty()
            || policies.iter().enumerate().any(|(index, policy)| {
                policies[..index]
                    .iter()
                    .any(|seen| seen.feature == policy.feature)
            })
        {
            return Err(DomainError::InvalidProcessingState);
        }
        self.history_feature_policies = policies;
        Ok(self)
    }

    /// Returns explicit per-feature policies, or an empty slice when defaults apply.
    #[must_use]
    pub fn history_feature_policies(&self) -> &[HistoryFeaturePolicy] {
        &self.history_feature_policies
    }

    /// Resolves the aggregation for one history feature.
    #[must_use]
    pub fn history_aggregation_for(&self, feature: &str) -> HistoryAggregation {
        self.history_feature_policies
            .iter()
            .find(|policy| policy.feature() == feature)
            .map_or(self.history_aggregation, HistoryFeaturePolicy::aggregation)
    }

    /// Resolves duplicate handling for one history feature.
    #[must_use]
    pub fn history_duplicate_policy_for(&self, feature: &str) -> HistoryDuplicatePolicy {
        self.history_feature_policies
            .iter()
            .find(|policy| policy.feature() == feature)
            .map_or(
                self.history_duplicate_policy,
                HistoryFeaturePolicy::duplicate_policy,
            )
    }

    /// Returns the required history length.
    #[must_use]
    pub const fn history_steps(&self) -> usize {
        self.history_steps
    }

    /// Returns the maximum accepted forecast horizon.
    #[must_use]
    pub const fn max_horizon_steps(&self) -> usize {
        self.max_horizon_steps
    }

    /// Allows at most this many requested quantile probabilities.
    pub fn with_max_quantiles(mut self, max_quantiles: usize) -> Result<Self, DomainError> {
        if max_quantiles == 0 || max_quantiles > 19 {
            return Err(DomainError::InvalidProcessingWindow);
        }
        self.max_quantiles = max_quantiles;
        Ok(self)
    }

    /// Returns the maximum supported requested quantiles; zero means unsupported.
    #[must_use]
    pub const fn max_quantiles(&self) -> usize {
        self.max_quantiles
    }

    /// Returns the maximum age of accepted output.
    #[must_use]
    pub const fn max_output_age_ms(&self) -> u64 {
        self.max_output_age_ms
    }

    /// Returns the maximum missing-cell ratio.
    #[must_use]
    pub const fn max_missing_ratio(&self) -> f64 {
        self.max_missing_ratio
    }

    /// Returns named fallback strategies allowed by the task.
    #[must_use]
    pub fn allowed_fallbacks(&self) -> &[String] {
        &self.allowed_fallbacks
    }

    /// Returns the complete acceptance policy for a named fallback, if set.
    #[must_use]
    pub fn fallback_policy(&self, strategy: &str) -> Option<&FallbackPolicy> {
        self.fallback_policies
            .iter()
            .find(|policy| policy.strategy == strategy)
    }

    /// Returns all complete fallback acceptance policies.
    #[must_use]
    pub fn fallback_policies(&self) -> &[FallbackPolicy] {
        &self.fallback_policies
    }

    /// Returns the maximum age of every non-deterministic input source.
    #[must_use]
    pub const fn max_input_age_ms(&self) -> Option<u64> {
        self.max_input_age_ms
    }

    /// Returns the largest permitted gap between historical timestamps.
    #[must_use]
    pub const fn max_gap_ms(&self) -> Option<u64> {
        self.max_gap_ms
    }

    /// Returns whether non-deterministic future inputs require issue time.
    #[must_use]
    pub const fn requires_future_issue_time(&self) -> bool {
        self.require_future_issue_time
    }
}

/// Task-owned acceptance policy for a named fallback result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FallbackPolicy {
    strategy: String,
    version: String,
    source_feature: String,
    max_output_age_ms: u64,
}

impl FallbackPolicy {
    /// Creates a complete fallback acceptance policy.
    pub fn new(
        strategy: impl Into<String>,
        version: impl Into<String>,
        source_feature: impl Into<String>,
        max_output_age_ms: u64,
    ) -> Result<Self, DomainError> {
        if max_output_age_ms == 0 {
            return Err(DomainError::InvalidProcessingWindow);
        }
        Ok(Self {
            strategy: nonempty(strategy)?,
            version: nonempty(version)?,
            source_feature: nonempty(source_feature)?,
            max_output_age_ms,
        })
    }

    /// Returns the stable fallback strategy name.
    #[must_use]
    pub fn strategy(&self) -> &str {
        &self.strategy
    }

    /// Returns the expected fallback implementation version.
    #[must_use]
    pub fn version(&self) -> &str {
        &self.version
    }

    /// Returns the history feature on which the fallback must be based.
    #[must_use]
    pub fn source_feature(&self) -> &str {
        &self.source_feature
    }

    /// Returns the maximum lifetime of an accepted fallback result.
    #[must_use]
    pub const fn max_output_age_ms(&self) -> u64 {
        self.max_output_age_ms
    }
}

/// Declarative unit of data-processing work.
#[derive(Debug, Clone, PartialEq)]
pub struct DataProcessingTask {
    identity: TaskIdentity,
    processor_contract: String,
    features: Vec<FeatureDefinition>,
    forecast_spec: ForecastTaskSpec,
    remote_egress_allowed: bool,
}

impl DataProcessingTask {
    /// Creates a typed forecast task.
    pub fn forecast(
        identity: TaskIdentity,
        processor_contract: impl Into<String>,
        features: Vec<FeatureDefinition>,
        forecast_spec: ForecastTaskSpec,
    ) -> Result<Self, DomainError> {
        if features.is_empty() {
            return Err(DomainError::EmptyCollection);
        }
        if features.iter().enumerate().any(|(index, feature)| {
            features[..index]
                .iter()
                .any(|seen| seen.role() == feature.role() && seen.name() == feature.name())
        }) {
            return Err(DomainError::DuplicateFeature);
        }

        let target = features
            .iter()
            .find(|feature| {
                feature.role() == FeatureRole::History
                    && feature.name() == forecast_spec.target().name()
            })
            .ok_or(DomainError::FeatureTypeMismatch)?;
        if target.value_type() != FeatureValueType::Number
            || target.unit() != Some(forecast_spec.target().unit())
            || features.iter().any(|feature| {
                feature.role() != FeatureRole::History
                    && feature.name() == forecast_spec.target().name()
            })
        {
            return Err(DomainError::FeatureTypeMismatch);
        }
        let history_features = features
            .iter()
            .filter(|feature| feature.role() == FeatureRole::History)
            .collect::<Vec<_>>();
        if forecast_spec.fallback_policies().iter().any(|policy| {
            let source = history_features
                .iter()
                .find(|feature| feature.name() == policy.source_feature());
            source.is_none()
                || (policy.strategy() == "persistence"
                    && policy.source_feature() != forecast_spec.target().name())
        }) {
            return Err(DomainError::InvalidProcessingState);
        }
        if !forecast_spec.history_feature_policies().is_empty()
            && (forecast_spec.history_feature_policies().len() != history_features.len()
                || history_features.iter().any(|feature| {
                    forecast_spec
                        .history_feature_policies()
                        .iter()
                        .filter(|policy| policy.feature() == feature.name())
                        .count()
                        != 1
                }))
        {
            return Err(DomainError::InvalidProcessingState);
        }

        Ok(Self {
            identity,
            processor_contract: nonempty(processor_contract)?,
            features,
            forecast_spec,
            remote_egress_allowed: false,
        })
    }

    /// Explicitly allows a composition root to route complete frames remotely.
    ///
    /// Tasks are local-only by default. Deployment confirmation is still
    /// required independently at the application boundary.
    #[must_use]
    pub const fn allowing_remote_egress(mut self) -> Self {
        self.remote_egress_allowed = true;
        self
    }

    /// Returns the task identity.
    #[must_use]
    pub const fn identity(&self) -> &TaskIdentity {
        &self.identity
    }

    /// Returns the typed task kind.
    #[must_use]
    pub const fn kind(&self) -> TaskKind {
        TaskKind::Forecast
    }

    /// Returns the required processor contract.
    #[must_use]
    pub fn processor_contract(&self) -> &str {
        &self.processor_contract
    }

    /// Returns the declared input features.
    #[must_use]
    pub fn features(&self) -> &[FeatureDefinition] {
        &self.features
    }

    /// Returns the forecast task policy.
    #[must_use]
    pub const fn forecast_spec(&self) -> Option<&ForecastTaskSpec> {
        Some(&self.forecast_spec)
    }

    /// Returns whether this task permits a remote processor boundary.
    #[must_use]
    pub const fn remote_egress_allowed(&self) -> bool {
        self.remote_egress_allowed
    }
}

#[derive(Debug, Clone, PartialEq)]
enum FeatureValueInner {
    Number(f64),
    Text(String),
    Boolean(bool),
    Missing,
}

/// One typed feature value; missingness is explicit.
#[derive(Debug, Clone, PartialEq)]
pub struct FeatureValue(FeatureValueInner);

impl FeatureValue {
    /// Creates a finite numeric value.
    pub fn number(value: f64) -> Result<Self, DomainError> {
        if !value.is_finite() {
            return Err(DomainError::NonFiniteProcessingValue);
        }
        Ok(Self(FeatureValueInner::Number(value)))
    }

    /// Creates a text value.
    #[must_use]
    pub fn text(value: impl Into<String>) -> Self {
        Self(FeatureValueInner::Text(value.into()))
    }

    /// Creates a boolean value.
    #[must_use]
    pub const fn boolean(value: bool) -> Self {
        Self(FeatureValueInner::Boolean(value))
    }

    /// Creates an explicitly missing value.
    #[must_use]
    pub const fn missing() -> Self {
        Self(FeatureValueInner::Missing)
    }

    /// Returns the concrete type, or `None` for a missing value.
    #[must_use]
    pub const fn value_type(&self) -> Option<FeatureValueType> {
        match self.0 {
            FeatureValueInner::Number(_) => Some(FeatureValueType::Number),
            FeatureValueInner::Text(_) => Some(FeatureValueType::Text),
            FeatureValueInner::Boolean(_) => Some(FeatureValueType::Boolean),
            FeatureValueInner::Missing => None,
        }
    }

    /// Returns whether the value is missing.
    #[must_use]
    pub const fn is_missing(&self) -> bool {
        matches!(self.0, FeatureValueInner::Missing)
    }

    /// Returns a numeric value when present and numeric.
    #[must_use]
    pub const fn as_number(&self) -> Option<f64> {
        match self.0 {
            FeatureValueInner::Number(value) => Some(value),
            _ => None,
        }
    }

    /// Returns a text value when present and textual.
    #[must_use]
    pub fn as_text(&self) -> Option<&str> {
        match &self.0 {
            FeatureValueInner::Text(value) => Some(value),
            _ => None,
        }
    }

    /// Returns a boolean value when present and boolean.
    #[must_use]
    pub const fn as_boolean(&self) -> Option<bool> {
        match self.0 {
            FeatureValueInner::Boolean(value) => Some(value),
            _ => None,
        }
    }
}

/// Per-sample input quality.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SampleQuality {
    /// Direct, accepted value.
    Good,
    /// Present value with reduced confidence.
    Uncertain,
    /// Value filled by a declared substitution.
    Substituted,
    /// No usable value.
    Missing,
}

fn validate_sample(
    definition: &FeatureDefinition,
    value: &FeatureValue,
    quality: SampleQuality,
) -> Result<(), DomainError> {
    if value
        .value_type()
        .is_some_and(|value_type| value_type != definition.value_type())
    {
        return Err(DomainError::FeatureTypeMismatch);
    }
    if value.is_missing() != (quality == SampleQuality::Missing) {
        return Err(DomainError::InvalidSampleQuality);
    }
    if definition
        .numeric_constraints()
        .zip(value.as_number())
        .is_some_and(|(constraints, value)| !constraints.accepts(value))
    {
        return Err(DomainError::InvalidFrameQuality);
    }
    Ok(())
}

/// One aligned feature series.
#[derive(Debug, Clone, PartialEq)]
pub struct Series {
    definition: FeatureDefinition,
    values: Vec<FeatureValue>,
    quality: Vec<SampleQuality>,
}

impl Series {
    /// Creates a series with one quality flag per value.
    pub fn new(
        definition: FeatureDefinition,
        values: Vec<FeatureValue>,
        quality: Vec<SampleQuality>,
    ) -> Result<Self, DomainError> {
        if values.len() != quality.len() {
            return Err(DomainError::ArrayLengthMismatch);
        }
        for (value, quality) in values.iter().zip(&quality) {
            validate_sample(&definition, value, *quality)?;
        }
        Ok(Self {
            definition,
            values,
            quality,
        })
    }

    /// Returns the feature definition.
    #[must_use]
    pub const fn definition(&self) -> &FeatureDefinition {
        &self.definition
    }

    /// Returns aligned values.
    #[must_use]
    pub fn values(&self) -> &[FeatureValue] {
        &self.values
    }

    /// Returns aligned quality flags.
    #[must_use]
    pub fn quality(&self) -> &[SampleQuality] {
        &self.quality
    }

    /// Returns the number of samples.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.values.len()
    }

    /// Returns whether the series has no samples.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.values.is_empty()
    }
}

/// One timestamp grid with one or more aligned series.
#[derive(Debug, Clone, PartialEq)]
pub struct Segment {
    timestamps: Vec<TimestampMs>,
    series: Vec<Series>,
}

impl Segment {
    /// Creates a non-empty segment with strictly increasing timestamps.
    pub fn new(timestamps: Vec<TimestampMs>, series: Vec<Series>) -> Result<Self, DomainError> {
        if timestamps.is_empty() || series.is_empty() {
            return Err(DomainError::EmptyCollection);
        }
        if !timestamps_are_strictly_increasing(&timestamps) {
            return Err(DomainError::TimestampsNotStrictlyIncreasing);
        }
        if series.iter().any(|values| values.len() != timestamps.len()) {
            return Err(DomainError::ArrayLengthMismatch);
        }
        if series.iter().enumerate().any(|(index, values)| {
            series[..index]
                .iter()
                .any(|seen| seen.definition().name() == values.definition().name())
        }) {
            return Err(DomainError::DuplicateFeature);
        }
        Ok(Self { timestamps, series })
    }

    /// Returns the shared timestamp grid.
    #[must_use]
    pub fn timestamps(&self) -> &[TimestampMs] {
        &self.timestamps
    }

    /// Returns the aligned series.
    #[must_use]
    pub fn series(&self) -> &[Series] {
        &self.series
    }

    /// Returns the number of timestamps.
    #[must_use]
    pub const fn sample_count(&self) -> usize {
        self.timestamps.len()
    }
}

/// Computes the longest interval without a usable observation in any series.
///
/// History uses interval-end labels, so the left boundary is one cadence
/// before the first label and the right boundary is the final label. Missing
/// cells at either boundary therefore participate in the same gap policy as
/// missing cells inside the window.
#[must_use]
pub fn maximum_observation_gap(history: &Segment, cadence_ms: u64) -> u64 {
    let Some(first_label) = history.timestamps().first().copied() else {
        return cadence_ms;
    };
    let Some(last_label) = history.timestamps().last().copied() else {
        return cadence_ms;
    };
    let window_start = first_label.get().saturating_sub(cadence_ms);
    history.series().iter().fold(cadence_ms, |maximum, series| {
        let (last_usable, series_maximum) = history
            .timestamps()
            .iter()
            .zip(series.values())
            .zip(series.quality())
            .filter_map(|((timestamp, value), quality)| {
                (*quality != SampleQuality::Missing && !value.is_missing()).then_some(*timestamp)
            })
            .fold(
                (TimestampMs::new(window_start), cadence_ms),
                |(previous, maximum), timestamp| {
                    let gap = timestamp.get().saturating_sub(previous.get());
                    (timestamp, maximum.max(gap))
                },
            );
        maximum.max(series_maximum.max(last_label.get().saturating_sub(last_usable.get())))
    })
}

/// One non-series feature fixed for an execution.
#[derive(Debug, Clone, PartialEq)]
pub struct StaticFeature {
    definition: FeatureDefinition,
    value: FeatureValue,
    quality: SampleQuality,
}

impl StaticFeature {
    /// Creates a validated static feature.
    pub fn new(
        definition: FeatureDefinition,
        value: FeatureValue,
        quality: SampleQuality,
    ) -> Result<Self, DomainError> {
        if definition.role() != FeatureRole::Static {
            return Err(DomainError::FeatureTypeMismatch);
        }
        validate_sample(&definition, &value, quality)?;
        Ok(Self {
            definition,
            value,
            quality,
        })
    }

    /// Returns the feature definition.
    #[must_use]
    pub const fn definition(&self) -> &FeatureDefinition {
        &self.definition
    }

    /// Returns the feature value.
    #[must_use]
    pub const fn value(&self) -> &FeatureValue {
        &self.value
    }

    /// Returns the quality flag.
    #[must_use]
    pub const fn quality(&self) -> SampleQuality {
        self.quality
    }
}

/// Aggregate input quality calculated by Aether.
#[derive(Debug, Clone, PartialEq)]
pub struct FrameQuality {
    input_watermark: TimestampMs,
    missing_ratio: f64,
    max_gap_ms: u64,
    live_tail_included: bool,
    substituted_samples: usize,
}

impl FrameQuality {
    /// Creates aggregate frame quality.
    pub fn new(
        input_watermark: TimestampMs,
        missing_ratio: f64,
        max_gap_ms: u64,
        live_tail_included: bool,
        substituted_samples: usize,
    ) -> Result<Self, DomainError> {
        if !missing_ratio.is_finite() || !(0.0..=1.0).contains(&missing_ratio) {
            return Err(DomainError::InvalidFrameQuality);
        }
        Ok(Self {
            input_watermark,
            missing_ratio,
            max_gap_ms,
            live_tail_included,
            substituted_samples,
        })
    }

    /// Returns the newest observation considered.
    #[must_use]
    pub const fn input_watermark(&self) -> TimestampMs {
        self.input_watermark
    }

    /// Returns the missing-cell ratio.
    #[must_use]
    pub const fn missing_ratio(&self) -> f64 {
        self.missing_ratio
    }

    /// Returns the largest observed gap.
    #[must_use]
    pub const fn max_gap_ms(&self) -> u64 {
        self.max_gap_ms
    }

    /// Returns whether live state contributed a tail sample.
    #[must_use]
    pub const fn live_tail_included(&self) -> bool {
        self.live_tail_included
    }

    /// Returns the number of substituted samples.
    #[must_use]
    pub const fn substituted_samples(&self) -> usize {
        self.substituted_samples
    }
}

/// Segment containing a source contribution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SegmentKind {
    /// Historical observations.
    History,
    /// Known-future covariates.
    FutureCovariates,
    /// Static context.
    StaticFeatures,
}

/// Logical source category, never a storage implementation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceKind {
    /// Stored historical observations.
    History,
    /// Current read-only live state.
    Live,
    /// A merged historical window and live tail.
    HistoryAndLive,
    /// An external or calculated covariate source.
    Covariate,
    /// Deterministic calendar calculation.
    Calendar,
    /// Commissioned constant.
    Constant,
}

/// Returns whether a source reference is a redaction-safe logical name.
///
/// Source references deliberately exclude URLs, filesystem paths, SQL text,
/// and physical channel coordinates. Concrete adapters keep those details in
/// composition-root configuration instead of processor-facing provenance.
#[must_use]
pub fn is_semantic_source_ref(value: &str) -> bool {
    let mut bytes = value.bytes();
    value.len() <= 2_048
        && bytes
            .next()
            .is_some_and(|byte| byte.is_ascii_alphanumeric())
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

/// Per-feature source watermark and optional forecast issue cut.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceProvenance {
    segment: SegmentKind,
    feature: String,
    source_kind: SourceKind,
    source_ref: Option<String>,
    watermark: TimestampMs,
    issued_at: Option<TimestampMs>,
}

impl SourceProvenance {
    /// Creates source provenance without a separate issue timestamp.
    pub fn new(
        segment: SegmentKind,
        feature: impl Into<String>,
        source_kind: SourceKind,
        source_ref: Option<&str>,
        watermark: TimestampMs,
    ) -> Result<Self, DomainError> {
        let source_is_valid = match segment {
            SegmentKind::History => matches!(
                source_kind,
                SourceKind::History
                    | SourceKind::Live
                    | SourceKind::HistoryAndLive
                    | SourceKind::Calendar
            ),
            SegmentKind::FutureCovariates => matches!(
                source_kind,
                SourceKind::Covariate | SourceKind::Calendar | SourceKind::Constant
            ),
            SegmentKind::StaticFeatures => source_kind == SourceKind::Constant,
        };
        if !source_is_valid {
            return Err(DomainError::InvalidProcessingState);
        }
        if source_ref.is_some_and(|value| !is_semantic_source_ref(value)) {
            return Err(DomainError::InvalidProcessingState);
        }
        let source_ref = source_ref.map(nonempty).transpose()?;
        Ok(Self {
            segment,
            feature: nonempty(feature)?,
            source_kind,
            source_ref,
            watermark,
            issued_at: None,
        })
    }

    /// Adds the issue time of an externally issued forecast.
    pub fn with_issued_at(mut self, issued_at: TimestampMs) -> Result<Self, DomainError> {
        if self.segment != SegmentKind::FutureCovariates
            || self.source_kind != SourceKind::Covariate
            || issued_at > self.watermark
        {
            return Err(DomainError::InvalidProcessingWindow);
        }
        self.issued_at = Some(issued_at);
        Ok(self)
    }

    /// Returns the frame segment.
    #[must_use]
    pub const fn segment(&self) -> SegmentKind {
        self.segment
    }

    /// Returns the task-local feature name.
    #[must_use]
    pub fn feature(&self) -> &str {
        &self.feature
    }

    /// Returns the source category.
    #[must_use]
    pub const fn source_kind(&self) -> SourceKind {
        self.source_kind
    }

    /// Returns the redaction-safe logical source reference.
    #[must_use]
    pub fn source_ref(&self) -> Option<&str> {
        self.source_ref.as_deref()
    }

    /// Returns the source watermark.
    #[must_use]
    pub const fn watermark(&self) -> TimestampMs {
        self.watermark
    }

    /// Returns the external forecast issue time, when applicable.
    #[must_use]
    pub const fn issued_at(&self) -> Option<TimestampMs> {
        self.issued_at
    }
}

/// Complete, immutable processor input assembled by Aether.
#[derive(Debug, Clone, PartialEq)]
pub struct ProcessingFrame {
    as_of: TimestampMs,
    cadence_ms: u64,
    history: Segment,
    future_covariates: Option<Segment>,
    static_features: Vec<StaticFeature>,
    quality: FrameQuality,
    provenance: Vec<SourceProvenance>,
}

impl ProcessingFrame {
    /// Creates a frame and validates cutoff and feature-role invariants.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        as_of: TimestampMs,
        cadence_ms: u64,
        history: Segment,
        future_covariates: Option<Segment>,
        static_features: Vec<StaticFeature>,
        quality: FrameQuality,
        provenance: Vec<SourceProvenance>,
    ) -> Result<Self, DomainError> {
        if cadence_ms == 0
            || history.timestamps().last().copied() != Some(as_of)
            || future_covariates
                .as_ref()
                .and_then(|segment| segment.timestamps().first())
                .is_some_and(|time| as_of.get().checked_add(cadence_ms) != Some(time.get()))
            || quality.input_watermark() > as_of
            || provenance.iter().any(|source| {
                source.watermark() > as_of || source.issued_at().is_some_and(|time| time > as_of)
            })
        {
            return Err(DomainError::InvalidProcessingWindow);
        }
        if history
            .series()
            .iter()
            .any(|series| series.definition().role() != FeatureRole::History)
            || future_covariates.as_ref().is_some_and(|segment| {
                segment
                    .series()
                    .iter()
                    .any(|series| series.definition().role() != FeatureRole::FutureCovariate)
            })
        {
            return Err(DomainError::FeatureTypeMismatch);
        }
        if static_features.iter().enumerate().any(|(index, feature)| {
            static_features[..index]
                .iter()
                .any(|seen| seen.definition().name() == feature.definition().name())
        }) {
            return Err(DomainError::DuplicateFeature);
        }
        let expected_provenance_count = history
            .series()
            .len()
            .saturating_add(
                future_covariates
                    .as_ref()
                    .map_or(0, |segment| segment.series().len()),
            )
            .saturating_add(static_features.len());
        let missing_or_duplicate_provenance =
            history.series().iter().any(|series| {
                provenance
                    .iter()
                    .filter(|source| {
                        source.segment() == SegmentKind::History
                            && source.feature() == series.definition().name()
                    })
                    .count()
                    != 1
            }) || future_covariates.as_ref().is_some_and(|segment| {
                segment.series().iter().any(|series| {
                    provenance
                        .iter()
                        .filter(|source| {
                            source.segment() == SegmentKind::FutureCovariates
                                && source.feature() == series.definition().name()
                        })
                        .count()
                        != 1
                })
            }) || static_features.iter().any(|feature| {
                provenance
                    .iter()
                    .filter(|source| {
                        source.segment() == SegmentKind::StaticFeatures
                            && source.feature() == feature.definition().name()
                    })
                    .count()
                    != 1
            });
        if provenance.len() != expected_provenance_count || missing_or_duplicate_provenance {
            return Err(DomainError::InvalidProcessingState);
        }
        let newest_actual_watermark = provenance
            .iter()
            .filter(|source| {
                !matches!(
                    source.source_kind(),
                    SourceKind::Calendar | SourceKind::Constant
                )
            })
            .map(SourceProvenance::watermark)
            .max();
        if newest_actual_watermark != Some(quality.input_watermark()) {
            return Err(DomainError::InvalidFrameQuality);
        }

        Ok(Self {
            as_of,
            cadence_ms,
            history,
            future_covariates,
            static_features,
            quality,
            provenance,
        })
    }

    /// Returns the observation cutoff.
    #[must_use]
    pub const fn as_of(&self) -> TimestampMs {
        self.as_of
    }

    /// Returns the aligned sample cadence.
    #[must_use]
    pub const fn cadence_ms(&self) -> u64 {
        self.cadence_ms
    }

    /// Returns historical observations.
    #[must_use]
    pub const fn history(&self) -> &Segment {
        &self.history
    }

    /// Returns known-future covariates.
    #[must_use]
    pub const fn future_covariates(&self) -> Option<&Segment> {
        self.future_covariates.as_ref()
    }

    /// Returns static execution context.
    #[must_use]
    pub fn static_features(&self) -> &[StaticFeature] {
        &self.static_features
    }

    /// Returns aggregate quality.
    #[must_use]
    pub const fn quality(&self) -> &FrameQuality {
        &self.quality
    }

    /// Returns source provenance.
    #[must_use]
    pub fn provenance(&self) -> &[SourceProvenance] {
        &self.provenance
    }

    /// Returns the total number of time-indexed samples across frame segments.
    #[must_use]
    pub fn sample_count(&self) -> usize {
        let history = self.history.sample_count();
        let future = self
            .future_covariates
            .as_ref()
            .map_or(0, Segment::sample_count);
        history.saturating_add(future)
    }

    /// Returns the aggregate scalar-cell count across all frame features.
    #[must_use]
    pub fn cell_count(&self) -> usize {
        let history = self
            .history
            .series()
            .iter()
            .fold(0usize, |count, series| count.saturating_add(series.len()));
        let future = self.future_covariates.as_ref().map_or(0, |segment| {
            segment
                .series()
                .iter()
                .fold(0usize, |count, series| count.saturating_add(series.len()))
        });
        history
            .saturating_add(future)
            .saturating_add(self.static_features.len())
    }
}

/// Typed forecast execution options supplied by an application caller.
#[derive(Debug, Clone, PartialEq)]
pub struct ForecastOptions {
    horizon_steps: usize,
    quantiles: Vec<f64>,
}

impl ForecastOptions {
    /// Creates forecast options.
    pub fn new(horizon_steps: usize, quantiles: Vec<f64>) -> Result<Self, DomainError> {
        if horizon_steps == 0 {
            return Err(DomainError::InvalidProcessingWindow);
        }
        if quantiles
            .iter()
            .any(|value| !value.is_finite() || *value <= 0.0 || *value >= 1.0)
            || quantiles.windows(2).any(|pair| pair[0] >= pair[1])
        {
            return Err(DomainError::InvalidQuantile);
        }
        Ok(Self {
            horizon_steps,
            quantiles,
        })
    }

    /// Returns the requested number of points.
    #[must_use]
    pub const fn horizon_steps(&self) -> usize {
        self.horizon_steps
    }

    /// Returns requested quantile probabilities.
    #[must_use]
    pub fn quantiles(&self) -> &[f64] {
        &self.quantiles
    }
}

/// Closed set of typed processing options.
#[derive(Debug, Clone, PartialEq)]
pub enum ProcessingOptions {
    /// Forecast execution options.
    Forecast(ForecastOptions),
}

/// Application-side request before any source data or processor is selected.
#[derive(Debug, Clone, PartialEq)]
pub struct ProcessTaskRequest {
    task: TaskIdentity,
    binding: BindingIdentity,
    as_of: TimestampMs,
    options: ProcessingOptions,
}

impl ProcessTaskRequest {
    /// Creates an application request containing only caller-selected semantics.
    #[must_use]
    pub const fn new(
        task: TaskIdentity,
        binding: BindingIdentity,
        as_of: TimestampMs,
        options: ProcessingOptions,
    ) -> Self {
        Self {
            task,
            binding,
            as_of,
            options,
        }
    }

    /// Returns the task identity.
    #[must_use]
    pub const fn task(&self) -> &TaskIdentity {
        &self.task
    }

    /// Returns the commissioned binding.
    #[must_use]
    pub const fn binding(&self) -> &BindingIdentity {
        &self.binding
    }

    /// Returns the requested observation cutoff.
    #[must_use]
    pub const fn as_of(&self) -> TimestampMs {
        self.as_of
    }

    /// Returns typed task options.
    #[must_use]
    pub const fn options(&self) -> &ProcessingOptions {
        &self.options
    }
}

/// Optional artifact selection passed to a processor without activating it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactSelector {
    kind: String,
    family: String,
    version: Option<String>,
    digest: Option<String>,
}

impl ArtifactSelector {
    /// Creates a generic artifact selector.
    pub fn new(
        kind: impl Into<String>,
        family: impl Into<String>,
        version: Option<&str>,
    ) -> Result<Self, DomainError> {
        Ok(Self {
            kind: nonempty(kind)?,
            family: nonempty(family)?,
            version: version.map(nonempty).transpose()?,
            digest: None,
        })
    }

    /// Pins the selector to one approved immutable artifact digest.
    pub fn with_digest(mut self, digest: impl Into<String>) -> Result<Self, DomainError> {
        let digest = digest.into();
        if !is_sha256_digest(&digest) {
            return Err(DomainError::InvalidProcessingState);
        }
        self.digest = Some(digest);
        Ok(self)
    }

    /// Returns the artifact kind.
    #[must_use]
    pub fn kind(&self) -> &str {
        &self.kind
    }

    /// Returns the artifact family.
    #[must_use]
    pub fn family(&self) -> &str {
        &self.family
    }

    /// Returns a requested version, or `None` for the configured active version.
    #[must_use]
    pub fn version(&self) -> Option<&str> {
        self.version.as_deref()
    }

    /// Returns an approved immutable digest, when the route pins one.
    #[must_use]
    pub fn digest(&self) -> Option<&str> {
        self.digest.as_deref()
    }
}

/// Complete processor-side request assembled and governed by Aether.
#[derive(Debug, Clone, PartialEq)]
pub struct DataProcessingRequest {
    request_id: String,
    task: TaskIdentity,
    binding: BindingIdentity,
    frame: ProcessingFrame,
    submitted_at: TimestampMs,
    deadline: TimestampMs,
    processor_contract: String,
    artifact_selector: Option<ArtifactSelector>,
    input_digest: String,
    options: ProcessingOptions,
}

impl DataProcessingRequest {
    /// Creates a complete processor request with an absolute deadline.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        request_id: impl Into<String>,
        task: TaskIdentity,
        binding: BindingIdentity,
        frame: ProcessingFrame,
        submitted_at: TimestampMs,
        deadline: TimestampMs,
        processor_contract: impl Into<String>,
        artifact_selector: Option<ArtifactSelector>,
        input_digest: impl Into<String>,
        options: ProcessingOptions,
    ) -> Result<Self, DomainError> {
        if deadline <= submitted_at || deadline <= frame.as_of() {
            return Err(DomainError::InvalidProcessingWindow);
        }
        Ok(Self {
            request_id: nonempty(request_id)?,
            task,
            binding,
            frame,
            submitted_at,
            deadline,
            processor_contract: nonempty(processor_contract)?,
            artifact_selector,
            input_digest: nonempty(input_digest)?,
            options,
        })
    }

    /// Returns the request correlation identifier.
    #[must_use]
    pub fn request_id(&self) -> &str {
        &self.request_id
    }

    /// Returns the task identity.
    #[must_use]
    pub const fn task(&self) -> &TaskIdentity {
        &self.task
    }

    /// Returns the binding identity.
    #[must_use]
    pub const fn binding(&self) -> &BindingIdentity {
        &self.binding
    }

    /// Returns the complete immutable input frame.
    #[must_use]
    pub const fn frame(&self) -> &ProcessingFrame {
        &self.frame
    }

    /// Returns when Aether submitted the request.
    #[must_use]
    pub const fn submitted_at(&self) -> TimestampMs {
        self.submitted_at
    }

    /// Returns the absolute processing deadline.
    #[must_use]
    pub const fn deadline(&self) -> TimestampMs {
        self.deadline
    }

    /// Returns the selected processor contract.
    #[must_use]
    pub fn processor_contract(&self) -> &str {
        &self.processor_contract
    }

    /// Returns the optional artifact selector.
    #[must_use]
    pub const fn artifact_selector(&self) -> Option<&ArtifactSelector> {
        self.artifact_selector.as_ref()
    }

    /// Returns the canonical task/frame/options digest.
    #[must_use]
    pub fn input_digest(&self) -> &str {
        &self.input_digest
    }

    /// Returns typed processing options.
    #[must_use]
    pub const fn options(&self) -> &ProcessingOptions {
        &self.options
    }
}

/// Identity and contract of the processor that produced a response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessorProvenance {
    id: String,
    version: String,
    contract: String,
}

impl ProcessorProvenance {
    /// Creates processor provenance.
    pub fn new(
        id: impl Into<String>,
        version: impl Into<String>,
        contract: impl Into<String>,
    ) -> Result<Self, DomainError> {
        Ok(Self {
            id: nonempty(id)?,
            version: nonempty(version)?,
            contract: nonempty(contract)?,
        })
    }

    /// Returns the processor identifier.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Returns the processor version.
    #[must_use]
    pub fn version(&self) -> &str {
        &self.version
    }

    /// Returns the implemented processor contract.
    #[must_use]
    pub fn contract(&self) -> &str {
        &self.contract
    }
}

/// Immutable artifact actually used by a processor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactProvenance {
    kind: String,
    family: String,
    version: String,
    digest: String,
}

impl ArtifactProvenance {
    /// Creates artifact provenance.
    pub fn new(
        kind: impl Into<String>,
        family: impl Into<String>,
        version: impl Into<String>,
        digest: impl Into<String>,
    ) -> Result<Self, DomainError> {
        let digest = digest.into();
        if !is_sha256_digest(&digest) {
            return Err(DomainError::InvalidProcessingState);
        }
        Ok(Self {
            kind: nonempty(kind)?,
            family: nonempty(family)?,
            version: nonempty(version)?,
            digest,
        })
    }

    /// Returns the artifact kind.
    #[must_use]
    pub fn kind(&self) -> &str {
        &self.kind
    }

    /// Returns the artifact family.
    #[must_use]
    pub fn family(&self) -> &str {
        &self.family
    }

    /// Returns the actual artifact version.
    #[must_use]
    pub fn version(&self) -> &str {
        &self.version
    }

    /// Returns the immutable artifact digest.
    #[must_use]
    pub fn digest(&self) -> &str {
        &self.digest
    }
}

/// One forecast quantile.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ForecastQuantile {
    probability: f64,
    value: f64,
}

impl ForecastQuantile {
    /// Creates a finite quantile strictly inside `(0, 1)`.
    pub fn new(probability: f64, value: f64) -> Result<Self, DomainError> {
        if !probability.is_finite() || probability <= 0.0 || probability >= 1.0 {
            return Err(DomainError::InvalidQuantile);
        }
        if !value.is_finite() {
            return Err(DomainError::NonFiniteProcessingValue);
        }
        Ok(Self { probability, value })
    }

    /// Returns the quantile probability.
    #[must_use]
    pub const fn probability(self) -> f64 {
        self.probability
    }

    /// Returns the quantile value.
    #[must_use]
    pub const fn value(self) -> f64 {
        self.value
    }
}

/// One point in a forecast output.
#[derive(Debug, Clone, PartialEq)]
pub struct ForecastPoint {
    timestamp: TimestampMs,
    value: f64,
    quantiles: Vec<ForecastQuantile>,
}

impl ForecastPoint {
    /// Creates a forecast point with ordered quantiles.
    pub fn new(
        timestamp: TimestampMs,
        value: f64,
        quantiles: Vec<ForecastQuantile>,
    ) -> Result<Self, DomainError> {
        if !value.is_finite() {
            return Err(DomainError::NonFiniteProcessingValue);
        }
        if quantiles.windows(2).any(|pair| {
            pair[0].probability() >= pair[1].probability() || pair[0].value() > pair[1].value()
        }) {
            return Err(DomainError::InvalidQuantile);
        }
        Ok(Self {
            timestamp,
            value,
            quantiles,
        })
    }

    /// Returns the forecast timestamp.
    #[must_use]
    pub const fn timestamp(&self) -> TimestampMs {
        self.timestamp
    }

    /// Returns the primary estimate.
    #[must_use]
    pub const fn value(&self) -> f64 {
        self.value
    }

    /// Returns optional quantiles.
    #[must_use]
    pub fn quantiles(&self) -> &[ForecastQuantile] {
        &self.quantiles
    }
}

/// Typed forecast processor output.
#[derive(Debug, Clone, PartialEq)]
pub struct ForecastOutput {
    target: String,
    unit: String,
    sign_convention: String,
    cadence_ms: u64,
    points: Vec<ForecastPoint>,
}

impl ForecastOutput {
    /// Creates a non-empty forecast with a regular, ordered timestamp grid.
    pub fn new(
        target: impl Into<String>,
        unit: impl Into<String>,
        sign_convention: impl Into<String>,
        cadence_ms: u64,
        points: Vec<ForecastPoint>,
    ) -> Result<Self, DomainError> {
        if cadence_ms == 0 || points.is_empty() {
            return Err(DomainError::InvalidProcessingWindow);
        }
        if points
            .windows(2)
            .any(|pair| pair[0].timestamp() >= pair[1].timestamp())
        {
            return Err(DomainError::TimestampsNotStrictlyIncreasing);
        }
        if points
            .windows(2)
            .any(|pair| pair[1].timestamp().get() - pair[0].timestamp().get() != cadence_ms)
        {
            return Err(DomainError::InvalidProcessingWindow);
        }
        Ok(Self {
            target: nonempty(target)?,
            unit: nonempty(unit)?,
            sign_convention: nonempty(sign_convention)?,
            cadence_ms,
            points,
        })
    }

    /// Returns the forecast target.
    #[must_use]
    pub fn target(&self) -> &str {
        &self.target
    }

    /// Returns the forecast unit.
    #[must_use]
    pub fn unit(&self) -> &str {
        &self.unit
    }

    /// Returns the target sign convention.
    #[must_use]
    pub fn sign_convention(&self) -> &str {
        &self.sign_convention
    }

    /// Returns the output cadence.
    #[must_use]
    pub const fn cadence_ms(&self) -> u64 {
        self.cadence_ms
    }

    /// Returns forecast points.
    #[must_use]
    pub fn points(&self) -> &[ForecastPoint] {
        &self.points
    }
}

/// Closed set of typed processor outputs.
#[derive(Debug, Clone, PartialEq)]
pub enum ProcessingOutput {
    /// Time-indexed forecast output.
    Forecast(ForecastOutput),
}

/// Explicit processor response status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessingStatus {
    /// The selected processor produced normal output.
    Produced,
    /// An approved named fallback produced output.
    Fallback,
    /// No usable output could be produced.
    Unavailable,
}

/// Metadata required when a fallback produced output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FallbackInfo {
    strategy: String,
    strategy_version: String,
    reason: String,
    source_feature: String,
    based_on_data_through: TimestampMs,
}

impl FallbackInfo {
    /// Creates fallback metadata.
    pub fn new(
        strategy: impl Into<String>,
        strategy_version: impl Into<String>,
        reason: impl Into<String>,
        source_feature: impl Into<String>,
        based_on_data_through: TimestampMs,
    ) -> Result<Self, DomainError> {
        Ok(Self {
            strategy: nonempty(strategy)?,
            strategy_version: nonempty(strategy_version)?,
            reason: nonempty(reason)?,
            source_feature: nonempty(source_feature)?,
            based_on_data_through,
        })
    }

    /// Returns the named fallback strategy.
    #[must_use]
    pub fn strategy(&self) -> &str {
        &self.strategy
    }

    /// Returns the fallback strategy version.
    #[must_use]
    pub fn strategy_version(&self) -> &str {
        &self.strategy_version
    }

    /// Returns the stable fallback reason.
    #[must_use]
    pub fn reason(&self) -> &str {
        &self.reason
    }

    /// Returns the history feature used to produce the fallback.
    #[must_use]
    pub fn source_feature(&self) -> &str {
        &self.source_feature
    }

    /// Returns the newest observation used by the fallback.
    #[must_use]
    pub const fn based_on_data_through(&self) -> TimestampMs {
        self.based_on_data_through
    }
}

/// Metadata required when no usable output exists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnavailableInfo {
    reason: String,
    retryable: bool,
    retry_after_ms: Option<u64>,
}

impl UnavailableInfo {
    /// Creates unavailable metadata.
    pub fn new(
        reason: impl Into<String>,
        retryable: bool,
        retry_after_ms: Option<u64>,
    ) -> Result<Self, DomainError> {
        if retry_after_ms.is_some_and(|delay| delay == 0)
            || (!retryable && retry_after_ms.is_some())
        {
            return Err(DomainError::InvalidProcessingState);
        }
        Ok(Self {
            reason: nonempty(reason)?,
            retryable,
            retry_after_ms,
        })
    }

    /// Returns the stable reason.
    #[must_use]
    pub fn reason(&self) -> &str {
        &self.reason
    }

    /// Returns whether retry after an external change may succeed.
    #[must_use]
    pub const fn retryable(&self) -> bool {
        self.retryable
    }

    /// Returns a suggested bounded retry delay.
    #[must_use]
    pub const fn retry_after_ms(&self) -> Option<u64> {
        self.retry_after_ms
    }
}

/// Untrusted response returned by a data processor.
#[derive(Debug, Clone, PartialEq)]
pub struct ProcessingResult {
    request_id: String,
    task: TaskIdentity,
    binding: BindingIdentity,
    input_digest: String,
    status: ProcessingStatus,
    processor: ProcessorProvenance,
    artifact: Option<ArtifactProvenance>,
    input_watermark: TimestampMs,
    produced_at: TimestampMs,
    expires_at: Option<TimestampMs>,
    output: Option<ProcessingOutput>,
    fallback: Option<FallbackInfo>,
    unavailable: Option<UnavailableInfo>,
    warnings: Vec<String>,
}

impl ProcessingResult {
    /// Creates an untrusted result while enforcing status-specific structure.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        request_id: impl Into<String>,
        task: TaskIdentity,
        binding: BindingIdentity,
        input_digest: impl Into<String>,
        status: ProcessingStatus,
        processor: ProcessorProvenance,
        artifact: Option<ArtifactProvenance>,
        input_watermark: TimestampMs,
        produced_at: TimestampMs,
        expires_at: Option<TimestampMs>,
        output: Option<ProcessingOutput>,
        fallback: Option<FallbackInfo>,
        unavailable: Option<UnavailableInfo>,
    ) -> Result<Self, DomainError> {
        let structure_is_valid = match status {
            ProcessingStatus::Produced => {
                output.is_some()
                    && expires_at.is_some()
                    && fallback.is_none()
                    && unavailable.is_none()
            },
            ProcessingStatus::Fallback => {
                output.is_some()
                    && expires_at.is_some()
                    && fallback.is_some()
                    && unavailable.is_none()
            },
            ProcessingStatus::Unavailable => {
                output.is_none()
                    && expires_at.is_none()
                    && fallback.is_none()
                    && unavailable.is_some()
            },
        };
        if !structure_is_valid {
            return Err(DomainError::InvalidProcessingState);
        }
        if input_watermark > produced_at
            || expires_at.is_some_and(|expiry| expiry <= produced_at)
            || fallback
                .as_ref()
                .is_some_and(|info| info.based_on_data_through() > input_watermark)
        {
            return Err(DomainError::InvalidProcessingWindow);
        }

        Ok(Self {
            request_id: nonempty(request_id)?,
            task,
            binding,
            input_digest: nonempty(input_digest)?,
            status,
            processor,
            artifact,
            input_watermark,
            produced_at,
            expires_at,
            output,
            fallback,
            unavailable,
            warnings: Vec::new(),
        })
    }

    /// Adds stable warning codes to a result.
    pub fn with_warnings(mut self, warnings: Vec<String>) -> Result<Self, DomainError> {
        let warnings = warnings
            .into_iter()
            .map(nonempty)
            .collect::<Result<Vec<_>, _>>()?;
        if !strings_are_unique(&warnings) {
            return Err(DomainError::InvalidProcessingState);
        }
        self.warnings = warnings;
        Ok(self)
    }

    /// Returns the request identifier.
    #[must_use]
    pub fn request_id(&self) -> &str {
        &self.request_id
    }

    /// Returns the task identity.
    #[must_use]
    pub const fn task(&self) -> &TaskIdentity {
        &self.task
    }

    /// Returns the binding identity.
    #[must_use]
    pub const fn binding(&self) -> &BindingIdentity {
        &self.binding
    }

    /// Returns the echoed input digest.
    #[must_use]
    pub fn input_digest(&self) -> &str {
        &self.input_digest
    }

    /// Returns the explicit processor status.
    #[must_use]
    pub const fn status(&self) -> ProcessingStatus {
        self.status
    }

    /// Returns processor provenance.
    #[must_use]
    pub const fn processor(&self) -> &ProcessorProvenance {
        &self.processor
    }

    /// Returns artifact provenance when applicable.
    #[must_use]
    pub const fn artifact(&self) -> Option<&ArtifactProvenance> {
        self.artifact.as_ref()
    }

    /// Returns the newest accepted input observation.
    #[must_use]
    pub const fn input_watermark(&self) -> TimestampMs {
        self.input_watermark
    }

    /// Returns when processing completed.
    #[must_use]
    pub const fn produced_at(&self) -> TimestampMs {
        self.produced_at
    }

    /// Returns the result expiry for usable output.
    #[must_use]
    pub const fn expires_at(&self) -> Option<TimestampMs> {
        self.expires_at
    }

    /// Returns typed output when available.
    #[must_use]
    pub const fn output(&self) -> Option<&ProcessingOutput> {
        self.output.as_ref()
    }

    /// Returns fallback metadata when used.
    #[must_use]
    pub const fn fallback(&self) -> Option<&FallbackInfo> {
        self.fallback.as_ref()
    }

    /// Returns unavailable metadata when no output exists.
    #[must_use]
    pub const fn unavailable(&self) -> Option<&UnavailableInfo> {
        self.unavailable.as_ref()
    }

    /// Returns stable warning codes.
    #[must_use]
    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }
}

/// Aether-accepted derived data retaining the original processor response.
#[derive(Debug, Clone, PartialEq)]
pub struct DerivedData {
    result_id: String,
    accepted_at: TimestampMs,
    frame_quality: FrameQuality,
    result: ProcessingResult,
}

impl DerivedData {
    /// Accepts a usable, unexpired processing result after application validation.
    pub fn accept(
        result_id: impl Into<String>,
        accepted_at: TimestampMs,
        frame_quality: FrameQuality,
        result: ProcessingResult,
    ) -> Result<Self, DomainError> {
        if result.status() == ProcessingStatus::Unavailable
            || result
                .expires_at()
                .is_none_or(|expires_at| accepted_at >= expires_at)
            || frame_quality.input_watermark() != result.input_watermark()
        {
            return Err(DomainError::InvalidProcessingState);
        }
        Ok(Self {
            result_id: nonempty(result_id)?,
            accepted_at,
            frame_quality,
            result,
        })
    }

    /// Returns the Aether-owned result identifier.
    #[must_use]
    pub fn result_id(&self) -> &str {
        &self.result_id
    }

    /// Returns when Aether accepted the result.
    #[must_use]
    pub const fn accepted_at(&self) -> TimestampMs {
        self.accepted_at
    }

    /// Returns the Aether-computed input quality accepted with this result.
    #[must_use]
    pub const fn frame_quality(&self) -> &FrameQuality {
        &self.frame_quality
    }

    /// Returns the validated processor result and typed output.
    #[must_use]
    pub const fn result(&self) -> &ProcessingResult {
        &self.result
    }
}
