use std::path::{Path, PathBuf};

use aether_domain::{
    BindingIdentity, FeatureDefinition, FeatureRole, FeatureValueType, HistoryAggregation,
    HistoryDuplicatePolicy, TaskIdentity, is_semantic_source_ref,
};
use aether_ports::{PortError, PortErrorKind, PortResult};

const MAX_ROUTES: usize = 256;
const MAX_RAW_SAMPLES_PER_FEATURE: usize = 1_000_000;
const MAX_TEXT_BYTES: usize = 2_048;

/// Deterministic calendar value generated on the commissioned task grid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CalendarFeature {
    /// Zero-based 15-minute interval index in UTC, in `[0, 95]`.
    QuarterHourOfDay,
}

/// Physical source commissioned for one logical history feature.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SqliteHistoryFeatureSource {
    /// Existing `aether-history` SQLite series coordinates.
    Stored {
        series_key: String,
        point_id: String,
        source_ref: String,
        aggregation: HistoryAggregation,
        duplicate_policy: HistoryDuplicatePolicy,
    },
    /// A deterministic UTC calendar transform with no database read.
    Calendar {
        transform: CalendarFeature,
        source_ref: String,
    },
}

/// Binding-scoped, cadence-aware semantic feature mapping.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SqliteHistoryFeatureRoute {
    task: TaskIdentity,
    binding: BindingIdentity,
    definition: FeatureDefinition,
    cadence_ms: u64,
    source: SqliteHistoryFeatureSource,
}

impl SqliteHistoryFeatureRoute {
    /// Maps one numeric history feature to an existing SQLite series.
    #[allow(clippy::too_many_arguments)]
    pub fn stored(
        task: TaskIdentity,
        binding: BindingIdentity,
        definition: FeatureDefinition,
        cadence_ms: u64,
        aggregation: HistoryAggregation,
        duplicate_policy: HistoryDuplicatePolicy,
        series_key: impl Into<String>,
        point_id: impl Into<String>,
        source_ref: impl Into<String>,
    ) -> PortResult<Self> {
        Self::new(
            task,
            binding,
            definition,
            cadence_ms,
            SqliteHistoryFeatureSource::Stored {
                series_key: bounded_text(series_key, "SQLite history series key is invalid")?,
                point_id: bounded_text(point_id, "SQLite history point id is invalid")?,
                source_ref: semantic_ref(source_ref, "SQLite history source ref is invalid")?,
                aggregation,
                duplicate_policy,
            },
        )
    }

    /// Maps one numeric unitless history feature to a calendar transform.
    pub fn calendar(
        task: TaskIdentity,
        binding: BindingIdentity,
        definition: FeatureDefinition,
        cadence_ms: u64,
        transform: CalendarFeature,
        source_ref: impl Into<String>,
    ) -> PortResult<Self> {
        if definition.unit() != Some("1") {
            return Err(permanent(
                "calendar history features require the exact unit '1'",
            ));
        }
        Self::new(
            task,
            binding,
            definition,
            cadence_ms,
            SqliteHistoryFeatureSource::Calendar {
                transform,
                source_ref: semantic_ref(source_ref, "calendar source ref is invalid")?,
            },
        )
    }

    fn new(
        task: TaskIdentity,
        binding: BindingIdentity,
        definition: FeatureDefinition,
        cadence_ms: u64,
        source: SqliteHistoryFeatureSource,
    ) -> PortResult<Self> {
        if definition.role() != FeatureRole::History
            || definition.value_type() != FeatureValueType::Number
            || definition.unit().is_none()
            || cadence_ms == 0
        {
            return Err(permanent(
                "SQLite history routes require a numeric history feature and cadence",
            ));
        }
        Ok(Self {
            task,
            binding,
            definition,
            cadence_ms,
            source,
        })
    }

    pub(crate) const fn task(&self) -> &TaskIdentity {
        &self.task
    }

    pub(crate) const fn binding(&self) -> &BindingIdentity {
        &self.binding
    }

    pub(crate) const fn definition(&self) -> &FeatureDefinition {
        &self.definition
    }

    pub(crate) const fn cadence_ms(&self) -> u64 {
        self.cadence_ms
    }

    pub(crate) const fn source(&self) -> &SqliteHistoryFeatureSource {
        &self.source
    }

    /// Returns the task-owned aggregation commissioned for a stored route.
    #[must_use]
    pub const fn aggregation(&self) -> Option<HistoryAggregation> {
        match &self.source {
            SqliteHistoryFeatureSource::Stored { aggregation, .. } => Some(*aggregation),
            SqliteHistoryFeatureSource::Calendar { .. } => None,
        }
    }

    /// Returns task-owned duplicate handling commissioned for a stored route.
    #[must_use]
    pub const fn duplicate_policy(&self) -> Option<HistoryDuplicatePolicy> {
        match &self.source {
            SqliteHistoryFeatureSource::Stored {
                duplicate_policy, ..
            } => Some(*duplicate_policy),
            SqliteHistoryFeatureSource::Calendar { .. } => None,
        }
    }
}

/// Validated path, mapping, and raw-read limits for [`crate::SqliteHistoryQuery`].
#[derive(Debug, Clone)]
pub struct SqliteHistoryQueryConfig {
    database_path: PathBuf,
    routes: Vec<SqliteHistoryFeatureRoute>,
    max_raw_samples_per_feature: usize,
}

impl SqliteHistoryQueryConfig {
    /// Creates a bounded read-only SQLite history configuration.
    pub fn new(
        database_path: impl AsRef<Path>,
        routes: Vec<SqliteHistoryFeatureRoute>,
        max_raw_samples_per_feature: usize,
    ) -> PortResult<Self> {
        let database_path = database_path.as_ref();
        if database_path.as_os_str().is_empty()
            || routes.is_empty()
            || routes.len() > MAX_ROUTES
            || max_raw_samples_per_feature == 0
            || max_raw_samples_per_feature > MAX_RAW_SAMPLES_PER_FEATURE
            || routes_have_duplicate_logical_keys(&routes)
            || routes_have_duplicate_physical_keys_within_task(&routes)
        {
            return Err(permanent(
                "SQLite history path, mappings, or limits are invalid",
            ));
        }
        Ok(Self {
            database_path: database_path.to_path_buf(),
            routes,
            max_raw_samples_per_feature,
        })
    }

    pub(crate) fn database_path(&self) -> &Path {
        &self.database_path
    }

    pub(crate) fn routes(&self) -> &[SqliteHistoryFeatureRoute] {
        &self.routes
    }

    pub(crate) const fn max_raw_samples_per_feature(&self) -> usize {
        self.max_raw_samples_per_feature
    }
}

fn routes_have_duplicate_logical_keys(routes: &[SqliteHistoryFeatureRoute]) -> bool {
    routes.iter().enumerate().any(|(index, route)| {
        routes[..index].iter().any(|seen| {
            seen.task == route.task
                && seen.binding == route.binding
                && seen.definition.name() == route.definition.name()
        })
    })
}

fn routes_have_duplicate_physical_keys_within_task(routes: &[SqliteHistoryFeatureRoute]) -> bool {
    routes.iter().enumerate().any(|(index, route)| {
        routes[..index].iter().any(|seen| {
            seen.task == route.task
                && seen.binding == route.binding
                && physical_source_matches(&seen.source, &route.source)
        })
    })
}

fn physical_source_matches(
    left: &SqliteHistoryFeatureSource,
    right: &SqliteHistoryFeatureSource,
) -> bool {
    match (left, right) {
        (
            SqliteHistoryFeatureSource::Stored {
                series_key: left_series,
                point_id: left_point,
                ..
            },
            SqliteHistoryFeatureSource::Stored {
                series_key: right_series,
                point_id: right_point,
                ..
            },
        ) => left_series == right_series && left_point == right_point,
        (
            SqliteHistoryFeatureSource::Calendar {
                transform: left, ..
            },
            SqliteHistoryFeatureSource::Calendar {
                transform: right, ..
            },
        ) => left == right,
        _ => false,
    }
}

fn bounded_text(value: impl Into<String>, message: &'static str) -> PortResult<String> {
    let value = value.into();
    if value.trim().is_empty()
        || value.len() > MAX_TEXT_BYTES
        || value.chars().any(char::is_control)
    {
        Err(permanent(message))
    } else {
        Ok(value)
    }
}

fn semantic_ref(value: impl Into<String>, message: &'static str) -> PortResult<String> {
    let value = bounded_text(value, message)?;
    if !is_semantic_source_ref(&value) {
        return Err(permanent(message));
    }
    Ok(value)
}

fn permanent(message: &'static str) -> PortError {
    PortError::new(PortErrorKind::Permanent, message)
}
