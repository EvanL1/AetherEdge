//! Bounded, read-only future-covariate snapshots for offline compositions.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

use aether_domain::{
    BindingIdentity, FeatureDefinition, FeatureRole, FeatureValue, FeatureValueType, SampleQuality,
    Segment, SegmentKind, Series, SourceKind, SourceProvenance, TimestampMs,
    is_semantic_source_ref,
};
use aether_ports::{
    CovariateSource, CovariateWindow, PortError, PortErrorKind, PortResult, SourcedSegment,
};
use async_trait::async_trait;
use serde::Deserialize;

const SNAPSHOT_SCHEMA: &str = "aether.covariate-snapshot.v1";
const QUARTER_HOUR: &str = "quarter_hour";
const QUARTER_HOUR_SOURCE: &str = "calendar.utc.quarter_hour";
const QUARTER_HOUR_MS: u64 = 15 * 60 * 1_000;
const QUARTERS_PER_DAY: u64 = 96;

fn invalid_data(message: &'static str) -> PortError {
    PortError::new(PortErrorKind::InvalidData, message)
}

fn rejected(message: &'static str) -> PortError {
    PortError::new(PortErrorKind::Rejected, message)
}

fn unavailable(message: &'static str) -> PortError {
    PortError::new(PortErrorKind::Unavailable, message)
}

fn permanent(message: &'static str) -> PortError {
    PortError::new(PortErrorKind::Permanent, message)
}

/// Hard limits applied before and after loading a covariate snapshot.
///
/// `max_features` and `max_samples` bound both each stored run and each
/// response. Calendar-derived fields count toward the response feature bound.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SnapshotCovariateLimits {
    max_file_bytes: usize,
    max_bindings: usize,
    max_runs_per_binding: usize,
    max_features: usize,
    max_samples: usize,
}

impl SnapshotCovariateLimits {
    /// Creates finite, non-zero snapshot and response limits.
    pub fn new(
        max_file_bytes: usize,
        max_bindings: usize,
        max_runs_per_binding: usize,
        max_features: usize,
        max_samples: usize,
    ) -> PortResult<Self> {
        let readable_bound = u64::try_from(max_file_bytes)
            .ok()
            .and_then(|bound| bound.checked_add(1))
            .is_some();
        if max_file_bytes == 0
            || max_bindings == 0
            || max_runs_per_binding == 0
            || max_features == 0
            || max_samples == 0
            || !readable_bound
        {
            return Err(rejected("snapshot limits must be finite and non-zero"));
        }
        Ok(Self {
            max_file_bytes,
            max_bindings,
            max_runs_per_binding,
            max_features,
            max_samples,
        })
    }

    /// Returns the maximum accepted file size.
    #[must_use]
    pub const fn max_file_bytes(self) -> usize {
        self.max_file_bytes
    }

    /// Returns the maximum number of commissioned bindings in one file.
    #[must_use]
    pub const fn max_bindings(self) -> usize {
        self.max_bindings
    }

    /// Returns the maximum number of forecast runs retained for one binding.
    #[must_use]
    pub const fn max_runs_per_binding(self) -> usize {
        self.max_runs_per_binding
    }

    /// Returns the maximum number of stored or returned features.
    #[must_use]
    pub const fn max_features(self) -> usize {
        self.max_features
    }

    /// Returns the maximum number of stored or returned samples.
    #[must_use]
    pub const fn max_samples(self) -> usize {
        self.max_samples
    }
}

impl Default for SnapshotCovariateLimits {
    fn default() -> Self {
        Self {
            max_file_bytes: 4 * 1024 * 1024,
            max_bindings: 256,
            max_runs_per_binding: 32,
            max_features: 64,
            max_samples: 4_096,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SnapshotWire {
    schema: String,
    bindings: Vec<BindingWire>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct BindingWire {
    id: String,
    revision: u32,
    runs: Vec<RunWire>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RunWire {
    issued_at_ms: u64,
    watermark_ms: u64,
    valid_times_ms: Vec<u64>,
    features: Vec<FeatureWire>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FeatureWire {
    name: String,
    value_type: ValueTypeWire,
    #[serde(default)]
    unit: Option<String>,
    source_ref: String,
    values: Vec<ScalarWire>,
    quality: Vec<QualityWire>,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ValueTypeWire {
    Number,
    String,
    Boolean,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum ScalarWire {
    Number(f64),
    String(String),
    Boolean(bool),
    Null,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum QualityWire {
    Good,
    Uncertain,
    Substituted,
    Missing,
}

#[derive(Debug)]
struct ForecastRun {
    issued_at: TimestampMs,
    watermark: TimestampMs,
    segment: Segment,
    source_refs: BTreeMap<String, String>,
}

impl ForecastRun {
    fn from_wire(wire: RunWire, limits: SnapshotCovariateLimits) -> PortResult<Self> {
        if wire.valid_times_ms.is_empty()
            || wire.valid_times_ms.len() > limits.max_samples
            || wire.features.is_empty()
            || wire.features.len() > limits.max_features
        {
            return Err(rejected("snapshot run exceeds its configured bounds"));
        }

        let issued_at = TimestampMs::new(wire.issued_at_ms);
        let watermark = TimestampMs::new(wire.watermark_ms);
        let timestamps = wire
            .valid_times_ms
            .into_iter()
            .map(TimestampMs::new)
            .collect::<Vec<_>>();
        if issued_at > watermark || timestamps.iter().any(|timestamp| *timestamp <= watermark) {
            return Err(invalid_data(
                "forecast issue, watermark, and valid times are inconsistent",
            ));
        }

        let sample_count = timestamps.len();
        let mut series = Vec::with_capacity(wire.features.len());
        let mut source_refs = BTreeMap::new();
        for feature in wire.features {
            let (name, values, source_ref) = parse_feature(feature, sample_count)?;
            if source_refs.insert(name, source_ref).is_some() {
                return Err(invalid_data(
                    "snapshot run contains duplicate feature names",
                ));
            }
            series.push(values);
        }
        let segment = Segment::new(timestamps, series)
            .map_err(|_| invalid_data("snapshot run is not a valid aligned segment"))?;

        Ok(Self {
            issued_at,
            watermark,
            segment,
            source_refs,
        })
    }
}

fn parse_feature(wire: FeatureWire, sample_count: usize) -> PortResult<(String, Series, String)> {
    if wire.name == QUARTER_HOUR {
        return Err(invalid_data(
            "deterministic calendar features must not be stored in a snapshot",
        ));
    }
    if !is_semantic_source_ref(&wire.source_ref) {
        return Err(invalid_data(
            "snapshot forecast features require a source reference",
        ));
    }
    if wire.values.len() != sample_count || wire.quality.len() != sample_count {
        return Err(invalid_data(
            "snapshot values and quality must match the valid-time grid",
        ));
    }

    let definition = match wire.value_type {
        ValueTypeWire::Number => {
            let unit = wire
                .unit
                .ok_or_else(|| invalid_data("numeric snapshot features require a unit"))?;
            FeatureDefinition::numeric(&wire.name, FeatureRole::FutureCovariate, unit)
        },
        ValueTypeWire::String => {
            if wire.unit.is_some() {
                return Err(invalid_data(
                    "string snapshot features must not declare a unit",
                ));
            }
            FeatureDefinition::new(
                &wire.name,
                FeatureRole::FutureCovariate,
                FeatureValueType::Text,
            )
        },
        ValueTypeWire::Boolean => {
            if wire.unit.is_some() {
                return Err(invalid_data(
                    "boolean snapshot features must not declare a unit",
                ));
            }
            FeatureDefinition::new(
                &wire.name,
                FeatureRole::FutureCovariate,
                FeatureValueType::Boolean,
            )
        },
    }
    .map_err(|_| invalid_data("snapshot feature definition is invalid"))?;
    let name = definition.name().to_string();
    let values = wire
        .values
        .into_iter()
        .map(scalar_value)
        .collect::<PortResult<Vec<_>>>()?;
    let quality = wire.quality.into_iter().map(sample_quality).collect();
    let series = Series::new(definition, values, quality)
        .map_err(|_| invalid_data("snapshot feature values violate their declared type"))?;
    Ok((name, series, wire.source_ref))
}

fn scalar_value(value: ScalarWire) -> PortResult<FeatureValue> {
    match value {
        ScalarWire::Number(value) => FeatureValue::number(value)
            .map_err(|_| invalid_data("snapshot contains a non-finite numeric value")),
        ScalarWire::String(value) => Ok(FeatureValue::text(value)),
        ScalarWire::Boolean(value) => Ok(FeatureValue::boolean(value)),
        ScalarWire::Null => Ok(FeatureValue::missing()),
    }
}

const fn sample_quality(quality: QualityWire) -> SampleQuality {
    match quality {
        QualityWire::Good => SampleQuality::Good,
        QualityWire::Uncertain => SampleQuality::Uncertain,
        QualityWire::Substituted => SampleQuality::Substituted,
        QualityWire::Missing => SampleQuality::Missing,
    }
}

#[derive(Debug)]
struct LoadedSnapshot {
    runs: BTreeMap<BindingIdentity, Vec<ForecastRun>>,
}

impl LoadedSnapshot {
    fn from_wire(snapshot: SnapshotWire, limits: SnapshotCovariateLimits) -> PortResult<Self> {
        if snapshot.schema != SNAPSHOT_SCHEMA {
            return Err(invalid_data("covariate snapshot schema is unsupported"));
        }
        if snapshot.bindings.len() > limits.max_bindings {
            return Err(rejected("snapshot exceeds its binding bound"));
        }

        let mut runs = BTreeMap::new();
        for binding in snapshot.bindings {
            if binding.runs.is_empty() || binding.runs.len() > limits.max_runs_per_binding {
                return Err(rejected("snapshot exceeds its run bound"));
            }
            let identity = BindingIdentity::new(binding.id, binding.revision)
                .map_err(|_| invalid_data("snapshot binding identity is invalid"))?;
            let mut binding_runs = binding
                .runs
                .into_iter()
                .map(|run| ForecastRun::from_wire(run, limits))
                .collect::<PortResult<Vec<_>>>()?;
            binding_runs.sort_by_key(|run| run.issued_at);
            if binding_runs
                .windows(2)
                .any(|pair| pair[0].issued_at == pair[1].issued_at)
            {
                return Err(invalid_data(
                    "snapshot contains duplicate issue times for one binding",
                ));
            }
            if runs.insert(identity, binding_runs).is_some() {
                return Err(invalid_data(
                    "snapshot contains duplicate binding identities",
                ));
            }
        }
        Ok(Self { runs })
    }

    fn select_run(&self, window: &CovariateWindow) -> PortResult<&ForecastRun> {
        let runs = self
            .runs
            .get(window.binding())
            .ok_or_else(|| permanent("covariate snapshot binding not found"))?;
        let run = runs
            .iter()
            .rev()
            .find(|run| run.issued_at <= window.as_of())
            .ok_or_else(|| unavailable("no forecast run was issued by the requested cutoff"))?;
        if run.watermark > window.as_of() {
            return Err(invalid_data(
                "selected forecast watermark is after the requested cutoff",
            ));
        }
        Ok(run)
    }
}

/// Reloading, file-backed [`CovariateSource`] for zero-service edge hosts.
///
/// Construction retains only a path and hard limits, so an optional snapshot
/// may be absent while the host starts. Every forecast resolution reads and
/// fully validates the currently published file on a blocking worker. An
/// atomic file replacement is therefore visible on the next request, while a
/// missing or invalid replacement fails closed without reusing stale data.
#[derive(Debug)]
pub struct SnapshotCovariateSource {
    path: PathBuf,
    limits: SnapshotCovariateLimits,
}

impl SnapshotCovariateSource {
    /// Configures a reloadable `aether.covariate-snapshot.v1` JSON path.
    ///
    /// The file need not exist yet. File availability and contents are checked
    /// by each [`CovariateSource::resolve`] call.
    pub fn open(path: impl AsRef<Path>, limits: SnapshotCovariateLimits) -> PortResult<Self> {
        let path = path.as_ref();
        if path.as_os_str().is_empty() {
            return Err(permanent("covariate snapshot path must not be empty"));
        }
        Ok(Self {
            path: path.to_path_buf(),
            limits,
        })
    }
}

fn load_snapshot(path: &Path, limits: SnapshotCovariateLimits) -> PortResult<LoadedSnapshot> {
    let bytes = read_bounded(path, limits.max_file_bytes)?;
    let snapshot: SnapshotWire = serde_json::from_slice(&bytes)
        .map_err(|_| invalid_data("covariate snapshot JSON is invalid"))?;
    LoadedSnapshot::from_wire(snapshot, limits)
}

fn read_bounded(path: &Path, max_file_bytes: usize) -> PortResult<Vec<u8>> {
    let metadata = path
        .metadata()
        .map_err(|_| unavailable("covariate snapshot file is unavailable"))?;
    if !metadata.is_file() {
        return Err(permanent("covariate snapshot path is not a file"));
    }
    if metadata.len() > max_file_bytes as u64 {
        return Err(rejected("covariate snapshot exceeds its file-size bound"));
    }

    let file =
        File::open(path).map_err(|_| unavailable("covariate snapshot file is unavailable"))?;
    let read_limit = u64::try_from(max_file_bytes)
        .ok()
        .and_then(|bound| bound.checked_add(1))
        .ok_or_else(|| rejected("covariate snapshot file-size bound is invalid"))?;
    let capacity = usize::try_from(metadata.len()).map_or(max_file_bytes, |length| length);
    let mut bytes = Vec::with_capacity(capacity.min(max_file_bytes));
    file.take(read_limit)
        .read_to_end(&mut bytes)
        .map_err(|_| unavailable("covariate snapshot file could not be read"))?;
    if bytes.len() > max_file_bytes {
        return Err(rejected("covariate snapshot exceeds its file-size bound"));
    }
    Ok(bytes)
}

fn exact_grid(
    window: &CovariateWindow,
    limits: SnapshotCovariateLimits,
) -> PortResult<Vec<TimestampMs>> {
    if window.features().len() > limits.max_features || window.max_samples() > limits.max_samples {
        return Err(rejected("covariate response exceeds its configured bounds"));
    }
    window
        .features()
        .len()
        .checked_mul(window.max_samples())
        .ok_or_else(|| rejected("covariate response cell count overflows"))?;

    let sample_count = u64::try_from(window.max_samples())
        .map_err(|_| rejected("covariate response sample bound is unsupported"))?;
    let span = window.end().get() - window.start().get();
    if !span.is_multiple_of(sample_count) {
        return Err(rejected(
            "covariate window does not define an exact regular grid",
        ));
    }
    let cadence = span / sample_count;
    if cadence == 0 {
        return Err(rejected(
            "covariate window does not define an exact regular grid",
        ));
    }

    (0..sample_count)
        .map(|index| {
            cadence
                .checked_mul(index)
                .and_then(|offset| window.start().get().checked_add(offset))
                .map(TimestampMs::new)
                .ok_or_else(|| rejected("covariate valid-time grid overflows"))
        })
        .collect()
}

fn selected_indexes(
    run: &ForecastRun,
    window: &CovariateWindow,
    expected: &[TimestampMs],
) -> PortResult<Vec<usize>> {
    let indexes = run
        .segment
        .timestamps()
        .iter()
        .enumerate()
        .filter_map(|(index, timestamp)| {
            (*timestamp >= window.start() && *timestamp < window.end()).then_some(index)
        })
        .collect::<Vec<_>>();
    let actual = indexes
        .iter()
        .map(|index| run.segment.timestamps()[*index])
        .collect::<Vec<_>>();
    if actual != expected {
        return Err(invalid_data(
            "selected forecast run does not match the requested valid-time grid",
        ));
    }
    Ok(indexes)
}

fn calendar_series(
    definition: &FeatureDefinition,
    timestamps: &[TimestampMs],
) -> PortResult<Series> {
    if definition.name() != QUARTER_HOUR
        || definition.value_type() != FeatureValueType::Number
        || definition.unit() != Some("1")
    {
        return Err(invalid_data(
            "calendar feature type and unit do not match its deterministic definition",
        ));
    }
    let values = timestamps
        .iter()
        .map(|timestamp| {
            let quarter = (timestamp.get() / QUARTER_HOUR_MS) % QUARTERS_PER_DAY;
            FeatureValue::number(quarter as f64)
                .map_err(|_| invalid_data("calendar feature generation failed"))
        })
        .collect::<PortResult<Vec<_>>>()?;
    Series::new(
        definition.clone(),
        values,
        vec![SampleQuality::Good; timestamps.len()],
    )
    .map_err(|_| invalid_data("calendar feature generation failed"))
}

fn project_forecast_series(
    run: &ForecastRun,
    requested: &FeatureDefinition,
    indexes: &[usize],
) -> PortResult<(Series, SourceProvenance)> {
    let stored = run
        .segment
        .series()
        .iter()
        .find(|series| series.definition().name() == requested.name())
        .ok_or_else(|| unavailable("requested forecast feature is unavailable"))?;
    if stored.definition().role() != requested.role()
        || stored.definition().value_type() != requested.value_type()
        || stored.definition().unit() != requested.unit()
    {
        return Err(invalid_data(
            "requested forecast feature type or unit does not match the selected run",
        ));
    }
    let values = indexes
        .iter()
        .map(|index| stored.values()[*index].clone())
        .collect();
    let quality = indexes
        .iter()
        .map(|index| stored.quality()[*index])
        .collect();
    let series = Series::new(requested.clone(), values, quality)
        .map_err(|_| invalid_data("selected forecast feature is invalid"))?;
    let source_ref = run
        .source_refs
        .get(requested.name())
        .ok_or_else(|| invalid_data("selected forecast feature has no source reference"))?;
    let provenance = SourceProvenance::new(
        SegmentKind::FutureCovariates,
        requested.name(),
        SourceKind::Covariate,
        Some(source_ref),
        run.watermark,
    )
    .and_then(|source| source.with_issued_at(run.issued_at))
    .map_err(|_| invalid_data("selected forecast provenance is invalid"))?;
    Ok((series, provenance))
}

#[async_trait]
impl CovariateSource for SnapshotCovariateSource {
    async fn resolve(&self, window: CovariateWindow) -> PortResult<SourcedSegment> {
        let timestamps = exact_grid(&window, self.limits)?;
        let needs_forecast = window
            .features()
            .iter()
            .any(|feature| feature.name() != QUARTER_HOUR);
        let loaded = if needs_forecast {
            let path = self.path.clone();
            let limits = self.limits;
            let runtime = tokio::runtime::Handle::try_current()
                .map_err(|_| permanent("covariate snapshot resolution requires a Tokio runtime"))?;
            Some(
                runtime
                    .spawn_blocking(move || load_snapshot(&path, limits))
                    .await
                    .map_err(|_| unavailable("covariate snapshot reload task failed"))??,
            )
        } else {
            None
        };
        let selected = if needs_forecast {
            let snapshot = loaded
                .as_ref()
                .ok_or_else(|| invalid_data("covariate snapshot reload is missing"))?;
            let run = snapshot.select_run(&window)?;
            Some((run, selected_indexes(run, &window, &timestamps)?))
        } else {
            None
        };

        let mut series = Vec::with_capacity(window.features().len());
        let mut provenance = Vec::with_capacity(window.features().len());
        for requested in window.features() {
            if requested.name() == QUARTER_HOUR {
                series.push(calendar_series(requested, &timestamps)?);
                provenance.push(
                    SourceProvenance::new(
                        SegmentKind::FutureCovariates,
                        requested.name(),
                        SourceKind::Calendar,
                        Some(QUARTER_HOUR_SOURCE),
                        window.as_of(),
                    )
                    .map_err(|_| invalid_data("calendar provenance is invalid"))?,
                );
            } else {
                let (run, indexes) = selected
                    .as_ref()
                    .ok_or_else(|| invalid_data("forecast run selection is missing"))?;
                let (projected, source) = project_forecast_series(run, requested, indexes)?;
                series.push(projected);
                provenance.push(source);
            }
        }
        let segment = Segment::new(timestamps, series)
            .map_err(|_| invalid_data("resolved covariate segment is invalid"))?;
        SourcedSegment::new(segment, provenance)
    }
}
