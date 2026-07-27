//! In-memory logical sources for deterministic data-processing compositions.

use std::collections::BTreeMap;
use std::sync::Mutex;

use aether_domain::{
    BindingIdentity, FeatureDefinition, FeatureRole, Segment, SegmentKind, Series, SourceKind,
    SourceProvenance, TimestampMs,
};
use aether_ports::{
    CovariateSource, CovariateWindow, HistoryQuery, HistoryWindow, PortError, PortErrorKind,
    PortResult, SourcedSegment,
};
use async_trait::async_trait;

use crate::lock_error;

fn invalid_data(message: &'static str) -> PortError {
    PortError::new(PortErrorKind::InvalidData, message)
}

fn unavailable(message: &'static str) -> PortError {
    PortError::new(PortErrorKind::Unavailable, message)
}

fn not_found(message: &'static str) -> PortError {
    PortError::new(PortErrorKind::Permanent, message)
}

fn source_kind_is_valid(segment: SegmentKind, source_kind: SourceKind) -> bool {
    match segment {
        SegmentKind::History => matches!(
            source_kind,
            SourceKind::History
                | SourceKind::Live
                | SourceKind::HistoryAndLive
                | SourceKind::Calendar
        ),
        SegmentKind::FutureCovariates => {
            matches!(source_kind, SourceKind::Covariate | SourceKind::Calendar)
        },
        SegmentKind::StaticFeatures => false,
    }
}

fn validate_snapshot(source: &SourcedSegment, expected_segment: SegmentKind) -> PortResult<()> {
    let expected_role = match expected_segment {
        SegmentKind::History => FeatureRole::History,
        SegmentKind::FutureCovariates => FeatureRole::FutureCovariate,
        SegmentKind::StaticFeatures => {
            return Err(invalid_data(
                "time-series source cannot contain static-feature provenance",
            ));
        },
    };
    if source.segment().series().len() != source.provenance().len()
        || source
            .segment()
            .series()
            .iter()
            .zip(source.provenance())
            .any(|(series, provenance)| {
                series.definition().role() != expected_role
                    || provenance.segment() != expected_segment
                    || provenance.feature() != series.definition().name()
                    || !source_kind_is_valid(expected_segment, provenance.source_kind())
                    || (expected_segment == SegmentKind::History
                        && provenance.issued_at().is_some())
            })
    {
        return Err(invalid_data(
            "source snapshot feature roles and provenance must match exactly",
        ));
    }
    Ok(())
}

fn project_provenance(
    provenance: &SourceProvenance,
    expected_segment: SegmentKind,
    last_timestamp: TimestampMs,
) -> PortResult<SourceProvenance> {
    if expected_segment != SegmentKind::History {
        return Ok(provenance.clone());
    }
    let watermark = if provenance.watermark() > last_timestamp {
        last_timestamp
    } else {
        provenance.watermark()
    };
    SourceProvenance::new(
        provenance.segment(),
        provenance.feature(),
        provenance.source_kind(),
        provenance.source_ref(),
        watermark,
    )
    .map_err(|_| invalid_data("stored history provenance is invalid"))
}

fn select_window(
    source: &SourcedSegment,
    features: &[FeatureDefinition],
    start: TimestampMs,
    end: TimestampMs,
    cutoff: Option<TimestampMs>,
    max_samples: usize,
    expected_segment: SegmentKind,
) -> PortResult<SourcedSegment> {
    let selected_indexes: Vec<usize> = source
        .segment()
        .timestamps()
        .iter()
        .enumerate()
        .filter_map(|(index, timestamp)| {
            (*timestamp >= start
                && *timestamp < end
                && cutoff.is_none_or(|cutoff| *timestamp <= cutoff))
            .then_some(index)
        })
        .collect();
    if selected_indexes.is_empty() {
        return Err(unavailable("no samples exist in the requested window"));
    }
    if selected_indexes.len() > max_samples {
        return Err(PortError::new(
            PortErrorKind::Rejected,
            "requested window exceeds its sample bound",
        ));
    }

    let timestamps = selected_indexes
        .iter()
        .map(|index| source.segment().timestamps()[*index])
        .collect();
    let mut selected_series = Vec::with_capacity(features.len());
    let mut provenance = Vec::with_capacity(features.len());
    for requested in features {
        let stored = source
            .segment()
            .series()
            .iter()
            .find(|series| series.definition().name() == requested.name())
            .ok_or_else(|| unavailable("requested logical feature is unavailable"))?;
        if stored.definition() != requested {
            return Err(invalid_data(
                "requested feature type, role, or unit does not match stored data",
            ));
        }
        let values = selected_indexes
            .iter()
            .map(|index| stored.values()[*index].clone())
            .collect();
        let quality = selected_indexes
            .iter()
            .map(|index| stored.quality()[*index])
            .collect();
        selected_series.push(
            Series::new(requested.clone(), values, quality)
                .map_err(|_| invalid_data("stored series violates its domain contract"))?,
        );
        let stored_provenance = source
            .provenance()
            .iter()
            .find(|entry| entry.feature() == requested.name())
            .ok_or_else(|| invalid_data("stored feature has no provenance"))?;
        let last_index = selected_indexes
            .last()
            .copied()
            .ok_or_else(|| invalid_data("selected window has no final sample"))?;
        provenance.push(project_provenance(
            stored_provenance,
            expected_segment,
            source.segment().timestamps()[last_index],
        )?);
    }

    let segment = Segment::new(timestamps, selected_series)
        .map_err(|_| invalid_data("selected source window is invalid"))?;
    SourcedSegment::new(segment, provenance)
}

/// Replaceable in-memory implementation of [`HistoryQuery`].
#[derive(Debug, Default)]
pub struct MemoryHistoryQuery {
    data: Mutex<BTreeMap<BindingIdentity, SourcedSegment>>,
}

impl MemoryHistoryQuery {
    /// Creates an empty logical history source.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Replaces the complete logical history snapshot for one binding.
    pub fn replace(&self, binding: BindingIdentity, data: SourcedSegment) -> PortResult<()> {
        validate_snapshot(&data, SegmentKind::History)?;
        self.data
            .lock()
            .map_err(|_| lock_error("data-processing history"))?
            .insert(binding, data);
        Ok(())
    }
}

#[async_trait]
impl HistoryQuery for MemoryHistoryQuery {
    async fn query(&self, window: HistoryWindow) -> PortResult<SourcedSegment> {
        let data = self
            .data
            .lock()
            .map_err(|_| lock_error("data-processing history"))?;
        let source = data
            .get(window.binding())
            .ok_or_else(|| not_found("history binding not found"))?;
        select_window(
            source,
            window.features(),
            window.start(),
            window.end(),
            Some(window.cutoff()),
            window.max_samples(),
            SegmentKind::History,
        )
    }
}

/// Replaceable in-memory implementation of [`CovariateSource`].
#[derive(Debug, Default)]
pub struct MemoryCovariateSource {
    data: Mutex<BTreeMap<BindingIdentity, SourcedSegment>>,
}

impl MemoryCovariateSource {
    /// Creates an empty future-covariate source.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Replaces the complete known-future snapshot for one binding.
    pub fn replace(&self, binding: BindingIdentity, data: SourcedSegment) -> PortResult<()> {
        validate_snapshot(&data, SegmentKind::FutureCovariates)?;
        self.data
            .lock()
            .map_err(|_| lock_error("data-processing covariates"))?
            .insert(binding, data);
        Ok(())
    }
}

#[async_trait]
impl CovariateSource for MemoryCovariateSource {
    async fn resolve(&self, window: CovariateWindow) -> PortResult<SourcedSegment> {
        let data = self
            .data
            .lock()
            .map_err(|_| lock_error("data-processing covariates"))?;
        let source = data
            .get(window.binding())
            .ok_or_else(|| not_found("covariate binding not found"))?;
        if source.provenance().iter().any(|entry| {
            entry.watermark() > window.as_of()
                || entry
                    .issued_at()
                    .is_some_and(|issued| issued > window.as_of())
        }) {
            return Err(invalid_data(
                "covariate source was not available at the requested cutoff",
            ));
        }
        select_window(
            source,
            window.features(),
            window.start(),
            window.end(),
            None,
            window.max_samples(),
            SegmentKind::FutureCovariates,
        )
    }
}
