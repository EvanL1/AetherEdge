use std::time::Duration;

use aether_domain::{
    BindingIdentity, FeatureDefinition, FeatureRole, FeatureValueType, TaskIdentity,
    is_semantic_source_ref,
};
use aether_ports::{PortError, PortErrorKind, PortResult};
use reqwest::{Client, Url, redirect::Policy};

const MAX_TIMEOUT_MS: u64 = 30_000;
const MAX_RESPONSE_BYTES: usize = 16 * 1024 * 1024;
const MAX_STORED_SERIES: usize = 20;

/// Deterministic calendar value generated on the requested time grid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CalendarFeature {
    /// Zero-based 15-minute interval index in UTC, in `[0, 95]`.
    QuarterHourOfDay,
}

/// Commissioned source for one task-local history feature.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HistoryFeatureSource {
    /// Existing aether-history series coordinates, never sent to a processor.
    Stored {
        series_key: String,
        point_id: String,
        source_ref: String,
    },
    /// Deterministic UTC calendar transform.
    Calendar {
        transform: CalendarFeature,
        source_ref: String,
    },
}

/// Binding-scoped, cadence-aware semantic feature route.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryFeatureRoute {
    task: TaskIdentity,
    binding: BindingIdentity,
    definition: FeatureDefinition,
    cadence_ms: u64,
    source: HistoryFeatureSource,
}

impl HistoryFeatureRoute {
    /// Maps one numeric history feature to an existing aether-history series.
    pub fn stored(
        task: TaskIdentity,
        binding: BindingIdentity,
        definition: FeatureDefinition,
        cadence_ms: u64,
        series_key: impl Into<String>,
        point_id: impl Into<String>,
        source_ref: impl Into<String>,
    ) -> PortResult<Self> {
        Self::new(
            task,
            binding,
            definition,
            cadence_ms,
            HistoryFeatureSource::Stored {
                series_key: nonempty(series_key, "series key")?,
                point_id: nonempty(point_id, "point id")?,
                source_ref: semantic_ref(source_ref)?,
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
            return Err(invalid(
                "calendar history features require the exact unit '1'",
            ));
        }
        Self::new(
            task,
            binding,
            definition,
            cadence_ms,
            HistoryFeatureSource::Calendar {
                transform,
                source_ref: semantic_ref(source_ref)?,
            },
        )
    }

    fn new(
        task: TaskIdentity,
        binding: BindingIdentity,
        definition: FeatureDefinition,
        cadence_ms: u64,
        source: HistoryFeatureSource,
    ) -> PortResult<Self> {
        if definition.role() != FeatureRole::History
            || definition.value_type() != FeatureValueType::Number
            || definition.unit().is_none()
            || cadence_ms == 0
        {
            return Err(invalid(
                "HTTP history routes require a numeric history feature and cadence",
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

    pub(crate) const fn source(&self) -> &HistoryFeatureSource {
        &self.source
    }
}

/// Validated transport and mapping policy for [`crate::HttpHistoryQuery`].
pub struct HttpHistoryQueryConfig {
    endpoint: Url,
    routes: Vec<HistoryFeatureRoute>,
    timeout: Duration,
    max_response_bytes: usize,
}

impl HttpHistoryQueryConfig {
    /// Creates a loopback-only, bounded history configuration.
    pub fn new(
        endpoint: &str,
        routes: Vec<HistoryFeatureRoute>,
        timeout_ms: u64,
        max_response_bytes: usize,
    ) -> PortResult<Self> {
        let endpoint = Url::parse(endpoint).map_err(|_| invalid("history endpoint is invalid"))?;
        let host_is_loopback = url_host_is_loopback(&endpoint);
        let stored_count = routes
            .iter()
            .filter(|route| matches!(route.source, HistoryFeatureSource::Stored { .. }))
            .count();
        if endpoint.scheme() != "http"
            || !host_is_loopback
            || !endpoint.username().is_empty()
            || endpoint.password().is_some()
            || endpoint.query().is_some()
            || endpoint.fragment().is_some()
            || endpoint.path() != "/hisApi/data/batch-query"
            || routes.is_empty()
            || stored_count > MAX_STORED_SERIES
            || timeout_ms == 0
            || timeout_ms > MAX_TIMEOUT_MS
            || max_response_bytes == 0
            || max_response_bytes > MAX_RESPONSE_BYTES
            || routes.iter().enumerate().any(|(index, route)| {
                routes[..index].iter().any(|seen| {
                    seen.task == route.task
                        && seen.binding == route.binding
                        && (seen.definition.name() == route.definition.name()
                            || seen.cadence_ms != route.cadence_ms
                            || physical_source_matches(&seen.source, &route.source))
                })
            })
        {
            return Err(invalid("history endpoint, mappings, or limits are unsafe"));
        }
        Ok(Self {
            endpoint,
            routes,
            timeout: Duration::from_millis(timeout_ms),
            max_response_bytes,
        })
    }

    pub(crate) fn build_client(&self) -> PortResult<Client> {
        Client::builder()
            .redirect(Policy::none())
            .no_proxy()
            .timeout(self.timeout)
            .build()
            .map_err(|_| PortError::new(PortErrorKind::Permanent, "history client setup failed"))
    }

    pub(crate) const fn endpoint(&self) -> &Url {
        &self.endpoint
    }

    pub(crate) fn routes(&self) -> &[HistoryFeatureRoute] {
        &self.routes
    }

    pub(crate) const fn max_response_bytes(&self) -> usize {
        self.max_response_bytes
    }
}

fn url_host_is_loopback(endpoint: &Url) -> bool {
    endpoint.host_str().is_some_and(|host| {
        let ip_literal = host
            .strip_prefix('[')
            .and_then(|value| value.strip_suffix(']'))
            .unwrap_or(host);
        host.eq_ignore_ascii_case("localhost")
            || ip_literal
                .parse::<std::net::IpAddr>()
                .is_ok_and(|ip| ip.is_loopback())
    })
}

fn physical_source_matches(left: &HistoryFeatureSource, right: &HistoryFeatureSource) -> bool {
    match (left, right) {
        (
            HistoryFeatureSource::Stored {
                series_key: left_series,
                point_id: left_point,
                ..
            },
            HistoryFeatureSource::Stored {
                series_key: right_series,
                point_id: right_point,
                ..
            },
        ) => left_series == right_series && left_point == right_point,
        (
            HistoryFeatureSource::Calendar {
                transform: left, ..
            },
            HistoryFeatureSource::Calendar {
                transform: right, ..
            },
        ) => left == right,
        _ => false,
    }
}

fn nonempty(value: impl Into<String>, label: &'static str) -> PortResult<String> {
    let value = value.into();
    if value.trim().is_empty() || value.chars().any(char::is_control) {
        return Err(invalid(label));
    }
    Ok(value)
}

fn semantic_ref(value: impl Into<String>) -> PortResult<String> {
    let value = nonempty(value, "source ref")?;
    if !is_semantic_source_ref(&value) {
        return Err(invalid("source ref"));
    }
    Ok(value)
}

fn invalid(message: &'static str) -> PortError {
    PortError::new(PortErrorKind::InvalidData, message)
}
