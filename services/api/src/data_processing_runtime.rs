//! Opt-in production composition root for Aether Data Processing.
//!
//! Configuration selects adapters and commissioned routes; no processor or
//! source client is constructed while the module is disabled.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use aether_application::{
    DataProcessingApplication, DataProcessingBinding, DataProcessingRoute, PointFeatureBinding,
    SafetyPolicy,
};
use aether_domain::{
    ArtifactSelector, BindingIdentity, DataProcessingTask, FallbackPolicy, FeatureDefinition,
    FeatureRole, FeatureValue, FeatureValueType, ForecastTarget, ForecastTaskSpec,
    HistoryAggregation, HistoryDuplicatePolicy, HistoryFeaturePolicy, InstanceId,
    NumericFeatureConstraints, PointAddress, PointId, PointKind, SampleQuality, StaticFeature,
    TaskIdentity, TaskKind,
};
use aether_http_data_processor::{BearerSecret, HttpDataProcessor, HttpDataProcessorConfig};
use aether_ports::{
    DataBoundary, DataProcessor, DataProcessorDescriptor, HistoryQuery, HistoryWindow, LiveState,
    PortError, PortErrorKind, PortResult, SourcedSegment,
};
use aether_sqlite_history_query::{
    CalendarFeature, SqliteHistoryFeatureRoute, SqliteHistoryQuery, SqliteHistoryQueryConfig,
};
use aether_store_local::{
    SnapshotCovariateLimits, SnapshotCovariateSource, SqliteAuditSink, SystemClock,
};
use anyhow::Context;
use serde::Deserialize;
use sqlx::SqlitePool;

use crate::config::GatewayConfig;
use crate::live_values::build_data_processing_live_state;

const MAX_RUNTIME_CONFIG_BYTES: u64 = 1024 * 1024;

/// Builds the optional application only after an explicit deployment opt-in.
pub async fn build_data_processing_application(
    database: &SqlitePool,
    gateway: &GatewayConfig,
) -> anyhow::Result<Option<Arc<DataProcessingApplication>>> {
    if !gateway.data_processing_enabled {
        return Ok(None);
    }
    let config = RuntimeConfig::load(&gateway.data_processing_config_path)?;
    let enabled = config
        .routes
        .into_iter()
        .filter(|route| route.enabled)
        .collect::<Vec<_>>();
    if enabled.is_empty() {
        anyhow::bail!("Data Processing is enabled but no route is enabled");
    }
    validate_sqlite_history_authority(database, &config.history.path).await?;

    let covariates = Arc::new(SnapshotCovariateSource::open(
        &config.covariates.path,
        SnapshotCovariateLimits::new(
            config.covariates.max_file_bytes,
            config.covariates.max_bindings,
            config.covariates.max_runs_per_binding,
            config.covariates.max_features,
            config.covariates.max_samples,
        )?,
    )?);

    let mut addresses = Vec::new();
    let mut history_routes = Vec::new();
    let mut commissioning_plans = Vec::new();
    let mut pending = Vec::with_capacity(enabled.len());
    for route in enabled {
        let task = route.task.into_domain()?;
        let (binding, binding_addresses) = route.binding.into_domain(&task)?;
        validate_route_source_coverage(&task, &binding, &route.history)?;
        validate_physical_history_routes(database, &task, &binding, &route.history).await?;
        commissioning_plans.push(PhysicalCommissioningPlan {
            task: task.clone(),
            binding: binding.clone(),
            history: route.history.clone(),
        });
        history_routes.extend(route.history.into_domain(&task, &binding)?);
        if route.processor.contract != task.processor_contract() {
            anyhow::bail!("processor and task contracts do not match");
        }
        if route.processor.requires_artifact && binding.artifact().is_none() {
            anyhow::bail!("processor requires a digest-pinned commissioned artifact");
        }
        addresses.extend(binding_addresses);
        let processor = route.processor.build()?;
        pending.push((
            task,
            binding,
            processor,
            route.deadline_ms,
            route.max_concurrency,
            route.remote_preapproved,
        ));
    }
    let sqlite_history = Arc::new(
        SqliteHistoryQuery::open(SqliteHistoryQueryConfig::new(
            &config.history.path,
            history_routes,
            config.history.max_raw_samples_per_feature,
        )?)
        .await?,
    );
    let history: Arc<dyn HistoryQuery> = Arc::new(AuthoritativeSqliteHistoryQuery {
        settings_database: database.clone(),
        configured_path: config.history.path.clone(),
        inner: sqlite_history,
        commissioning_plans,
    });
    let mut seen_addresses = std::collections::HashSet::new();
    addresses.retain(|address| seen_addresses.insert(*address));
    let live_state: Arc<dyn LiveState> =
        build_data_processing_live_state(database, gateway, &addresses).await?;
    let audit = Arc::new(SqliteAuditSink::initialize(database.clone()).await?);
    let routes = pending
        .into_iter()
        .map(
            |(task, binding, processor, deadline_ms, max_concurrency, remote_preapproved)| {
                let route = DataProcessingRoute::new(task, binding, processor, deadline_ms)?
                    .with_max_concurrency(max_concurrency)?;
                Ok(if remote_preapproved {
                    route.with_preapproved_remote_egress()
                } else {
                    route
                })
            },
        )
        .collect::<Result<Vec<_>, aether_application::ApplicationError>>()?;
    let application = DataProcessingApplication::new(
        routes,
        history,
        Some(covariates),
        live_state,
        audit,
        Arc::new(SystemClock),
        SafetyPolicy,
    )?;
    Ok(Some(Arc::new(application)))
}

struct AuthoritativeSqliteHistoryQuery {
    settings_database: SqlitePool,
    configured_path: String,
    inner: Arc<dyn HistoryQuery>,
    commissioning_plans: Vec<PhysicalCommissioningPlan>,
}

struct PhysicalCommissioningPlan {
    task: DataProcessingTask,
    binding: DataProcessingBinding,
    history: HistoryConfig,
}

#[async_trait::async_trait]
impl HistoryQuery for AuthoritativeSqliteHistoryQuery {
    async fn query(&self, window: HistoryWindow) -> PortResult<SourcedSegment> {
        let plan = self
            .commissioning_plans
            .iter()
            .find(|plan| {
                plan.task.identity() == window.task() && plan.binding.identity() == window.binding()
            })
            .ok_or_else(|| {
                PortError::new(
                    PortErrorKind::Permanent,
                    "physical history commissioning plan is unavailable",
                )
            })?;
        self.validate_authority(plan).await?;
        let sourced = self.inner.query(window).await?;
        self.validate_authority(plan).await?;
        Ok(sourced)
    }
}

impl AuthoritativeSqliteHistoryQuery {
    async fn validate_authority(&self, plan: &PhysicalCommissioningPlan) -> PortResult<()> {
        validate_sqlite_history_authority(&self.settings_database, &self.configured_path)
            .await
            .map_err(|_| {
                PortError::new(
                    PortErrorKind::Unavailable,
                    "commissioned history authority is unavailable",
                )
            })?;
        validate_physical_history_routes(
            &self.settings_database,
            &plan.task,
            &plan.binding,
            &plan.history,
        )
        .await
        .map_err(|_| {
            PortError::new(
                PortErrorKind::Unavailable,
                "commissioned history authority is unavailable",
            )
        })
    }
}

async fn validate_sqlite_history_authority(
    database: &SqlitePool,
    configured_path: &str,
) -> anyhow::Result<()> {
    let rows = sqlx::query_as::<_, (String, String)>(
        "SELECT key, value FROM history_config \
         WHERE key IN ('storage_enabled', 'storage_backend', 'storage_url')",
    )
    .fetch_all(database)
    .await
    .context("load the authoritative history storage settings")?;
    let settings = rows
        .into_iter()
        .collect::<std::collections::HashMap<_, _>>();
    let enabled = settings
        .get("storage_enabled")
        .context("history storage_enabled setting is missing")?;
    let backend = settings
        .get("storage_backend")
        .context("history storage_backend setting is missing")?;
    let storage_url = settings
        .get("storage_url")
        .context("history storage_url setting is missing")?;
    if !enabled.eq_ignore_ascii_case("true") {
        anyhow::bail!("Data Processing requires enabled history storage");
    }
    if !backend.eq_ignore_ascii_case("sqlite") {
        anyhow::bail!("Data Processing v1 supports only the authoritative SQLite historian");
    }
    if configured_path.is_empty() || Path::new(storage_url) != Path::new(configured_path) {
        anyhow::bail!("Data Processing history path does not match the authoritative historian");
    }
    Ok(())
}

fn validate_route_source_coverage(
    task: &DataProcessingTask,
    binding: &DataProcessingBinding,
    history: &HistoryConfig,
) -> anyhow::Result<()> {
    let expected = task
        .features()
        .iter()
        .filter(|feature| feature.role() == FeatureRole::History)
        .collect::<Vec<_>>();
    if history.routes.len() != expected.len()
        || history.routes.iter().any(|route| {
            route.binding_id != binding.identity().id()
                || route.binding_revision != binding.identity().revision()
        })
        || expected.iter().any(|feature| {
            history
                .routes
                .iter()
                .filter(|route| route.feature == feature.name())
                .count()
                != 1
        })
    {
        anyhow::bail!("history mappings do not exactly cover task history features");
    }
    Ok(())
}

async fn validate_physical_history_routes(
    database: &SqlitePool,
    task: &DataProcessingTask,
    binding: &DataProcessingBinding,
    history: &HistoryConfig,
) -> anyhow::Result<()> {
    let specification = task
        .forecast_spec()
        .context("forecast task has no specification")?;
    for route in &history.routes {
        if route.feature == specification.target().name()
            && !matches!(&route.source, HistorySourceConfig::Stored { .. })
        {
            anyhow::bail!("forecast target history must resolve to a stored physical point");
        }
        let HistorySourceConfig::Stored {
            series_key,
            point_id,
            source_unit,
            expected_scale,
            expected_offset,
            source_sign_convention,
        } = &route.source
        else {
            continue;
        };
        if !expected_scale.is_finite() || !expected_offset.is_finite() {
            anyhow::bail!("history physical scale or offset is not finite");
        }
        let definition = task
            .features()
            .iter()
            .find(|feature| {
                feature.role() == FeatureRole::History && feature.name() == route.feature
            })
            .context("history route references an undeclared feature")?;
        if definition.unit() != Some(source_unit.as_str()) {
            anyhow::bail!("history source unit does not match the task feature unit");
        }
        let (instance_id, measurement_id) = parse_logical_history_series(series_key, point_id)?;
        let physical_routes = sqlx::query_as::<_, (Option<i64>, Option<String>, Option<i64>)>(
            "SELECT channel_id, channel_type, channel_point_id \
             FROM measurement_routing \
             WHERE instance_id = ? AND measurement_id = ? AND enabled = TRUE",
        )
        .bind(i64::from(instance_id))
        .bind(i64::from(measurement_id))
        .fetch_all(database)
        .await?;
        let [(Some(channel_id), Some(channel_type), Some(channel_point_id))] =
            physical_routes.as_slice()
        else {
            anyhow::bail!("history series must resolve to exactly one enabled physical route");
        };
        let (table, physical_kind) = match channel_type.as_str() {
            "T" => ("telemetry_points", PointKind::Telemetry),
            "S" => ("signal_points", PointKind::Status),
            _ => anyhow::bail!("history series resolves to an unsupported physical point kind"),
        };
        let protocol =
            sqlx::query_scalar::<_, String>("SELECT protocol FROM channels WHERE channel_id = ?")
                .bind(channel_id)
                .fetch_optional(database)
                .await?
                .context("history physical channel does not exist")?;
        if protocol.eq_ignore_ascii_case("virtual") {
            anyhow::bail!("virtual channels are not collected by the history service");
        }
        let query = format!(
            "SELECT unit, scale, offset FROM {table} WHERE channel_id = ? AND point_id = ?"
        );
        let physical = sqlx::query_as::<_, (Option<String>, Option<f64>, Option<f64>)>(&query)
            .bind(channel_id)
            .bind(channel_point_id)
            .fetch_optional(database)
            .await?
            .context("history physical point does not exist")?;
        let (Some(unit), Some(scale), Some(offset)) = physical else {
            anyhow::bail!("history physical point lacks unit, scale, or offset metadata");
        };
        if unit != *source_unit
            || scale.to_bits() != expected_scale.to_bits()
            || offset.to_bits() != expected_offset.to_bits()
        {
            anyhow::bail!("history physical metadata differs from the commissioned contract");
        }

        let point_mapping = binding
            .point_features()
            .iter()
            .find(|point| point.feature() == route.feature);
        if let Some(point_mapping) = point_mapping {
            let address = point_mapping.address();
            if address.instance_id().get() != instance_id
                || address.point_id().get() != measurement_id
                || address.kind() != physical_kind
            {
                anyhow::bail!("history and live-state mappings do not identify the same point");
            }
        }
        if route.feature == specification.target().name() {
            if point_mapping.is_none()
                || source_sign_convention.as_deref()
                    != Some(specification.target().sign_convention())
            {
                anyhow::bail!("forecast target source sign convention is not commissioned");
            }
        } else if source_sign_convention.is_some() {
            anyhow::bail!("non-target history source must not claim a target sign convention");
        }
    }
    Ok(())
}

fn parse_logical_history_series(series_key: &str, point_id: &str) -> anyhow::Result<(u32, u32)> {
    let parts = series_key.split(':').collect::<Vec<_>>();
    let ["inst", instance_id, "M"] = parts.as_slice() else {
        anyhow::bail!("stored history series key must use inst:<u32>:M");
    };
    let instance_id = instance_id
        .parse::<u32>()
        .context("stored history series instance id is invalid")?;
    let point_id = point_id
        .parse::<u32>()
        .context("stored history point id is invalid")?;
    Ok((instance_id, point_id))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RuntimeConfig {
    schema: String,
    history: HistoryTransportConfig,
    covariates: CovariateFileConfig,
    routes: Vec<RouteConfig>,
}

impl RuntimeConfig {
    fn load(path: impl AsRef<Path>) -> anyhow::Result<Self> {
        let path = path.as_ref();
        let metadata = std::fs::metadata(path).with_context(|| {
            format!("read Data Processing config metadata at {}", path.display())
        })?;
        if metadata.len() == 0 || metadata.len() > MAX_RUNTIME_CONFIG_BYTES {
            anyhow::bail!("Data Processing runtime config exceeds its size bound");
        }
        let contents = std::fs::read_to_string(path)
            .with_context(|| format!("read Data Processing config at {}", path.display()))?;
        let value: Self = serde_yml::from_str(&contents)
            .context("parse strict Data Processing runtime config")?;
        if value.schema != "aether.data-processing-runtime.v1" || value.routes.is_empty() {
            anyhow::bail!("Data Processing runtime schema or route set is invalid");
        }
        Ok(value)
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct HistoryTransportConfig {
    path: String,
    max_raw_samples_per_feature: usize,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CovariateFileConfig {
    path: String,
    max_file_bytes: usize,
    max_bindings: usize,
    max_runs_per_binding: usize,
    max_features: usize,
    max_samples: usize,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RouteConfig {
    enabled: bool,
    deadline_ms: u64,
    max_concurrency: usize,
    #[serde(default)]
    remote_preapproved: bool,
    task: TaskConfig,
    binding: BindingConfig,
    history: HistoryConfig,
    processor: ProcessorConfig,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TaskConfig {
    id: String,
    revision: u32,
    processor_contract: String,
    target: TargetConfig,
    cadence_seconds: u64,
    history_aggregation: AggregationConfig,
    history_duplicate_policy: DuplicatePolicyConfig,
    history_steps: usize,
    max_horizon_steps: usize,
    max_quantiles: usize,
    max_output_age_seconds: u64,
    max_missing_ratio: f64,
    max_input_age_seconds: u64,
    max_gap_seconds: u64,
    require_future_issue_time: bool,
    #[serde(default)]
    remote_egress_allowed: bool,
    features: Vec<FeatureConfig>,
    fallbacks: Vec<FallbackConfig>,
}

impl TaskConfig {
    fn into_domain(self) -> anyhow::Result<DataProcessingTask> {
        let cadence_ms = seconds_to_ms(self.cadence_seconds, "task cadence")?;
        let history_policies = self
            .features
            .iter()
            .filter_map(FeatureConfig::history_policy)
            .collect::<anyhow::Result<Vec<_>>>()?;
        let features = self
            .features
            .into_iter()
            .map(FeatureConfig::into_domain)
            .collect::<anyhow::Result<Vec<_>>>()?;
        let fallback_names = self
            .fallbacks
            .iter()
            .map(|fallback| fallback.strategy.clone())
            .collect::<Vec<_>>();
        let fallback_policies = self
            .fallbacks
            .into_iter()
            .map(|fallback| {
                FallbackPolicy::new(
                    fallback.strategy,
                    fallback.version,
                    fallback.source_feature,
                    seconds_to_ms(fallback.max_output_age_seconds, "fallback lifetime")?,
                )
                .domain_context("invalid fallback policy")
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        let mut specification = ForecastTaskSpec::new(
            ForecastTarget::new(
                self.target.name,
                self.target.unit,
                self.target.sign_convention,
            )
            .domain_context("invalid forecast target")?,
            cadence_ms,
            self.history_aggregation.into_domain(),
            self.history_duplicate_policy.into_domain(),
            self.history_steps,
            self.max_horizon_steps,
            seconds_to_ms(self.max_output_age_seconds, "output lifetime")?,
            self.max_missing_ratio,
            fallback_names,
        )
        .domain_context("invalid forecast specification")?
        .with_input_quality_limits(
            seconds_to_ms(self.max_input_age_seconds, "input age")?,
            seconds_to_ms(self.max_gap_seconds, "input gap")?,
        )
        .domain_context("invalid forecast input quality policy")?
        .with_fallback_policies(fallback_policies)
        .domain_context("invalid forecast fallback policies")?;
        specification = specification
            .with_history_feature_policies(history_policies)
            .domain_context("invalid per-feature history policies")?;
        if self.max_quantiles > 0 {
            specification = specification
                .with_max_quantiles(self.max_quantiles)
                .domain_context("invalid forecast quantile policy")?;
        }
        if self.require_future_issue_time {
            specification = specification.requiring_future_issue_time();
        }
        let mut task = DataProcessingTask::forecast(
            TaskIdentity::new(self.id, self.revision).domain_context("invalid task identity")?,
            self.processor_contract,
            features,
            specification,
        )
        .domain_context("invalid data-processing task")?;
        if self.remote_egress_allowed {
            task = task.allowing_remote_egress();
        }
        Ok(task)
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TargetConfig {
    name: String,
    unit: String,
    sign_convention: String,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum FeatureRoleConfig {
    History,
    FutureCovariate,
    Static,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum FeatureTypeConfig {
    Number,
    String,
    Boolean,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum AggregationConfig {
    Mean,
    Last,
    Sum,
    Min,
    Max,
}

impl AggregationConfig {
    const fn into_domain(self) -> HistoryAggregation {
        match self {
            Self::Mean => HistoryAggregation::Mean,
            Self::Last => HistoryAggregation::Last,
            Self::Sum => HistoryAggregation::Sum,
            Self::Min => HistoryAggregation::Min,
            Self::Max => HistoryAggregation::Max,
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum DuplicatePolicyConfig {
    Latest,
    Reject,
}

impl DuplicatePolicyConfig {
    const fn into_domain(self) -> HistoryDuplicatePolicy {
        match self {
            Self::Latest => HistoryDuplicatePolicy::Latest,
            Self::Reject => HistoryDuplicatePolicy::Reject,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FeatureConfig {
    name: String,
    role: FeatureRoleConfig,
    value_type: FeatureTypeConfig,
    unit: Option<String>,
    history_aggregation: Option<AggregationConfig>,
    history_duplicate_policy: Option<DuplicatePolicyConfig>,
    constraints: Option<NumericConstraintsConfig>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct NumericConstraintsConfig {
    minimum: Option<f64>,
    maximum: Option<f64>,
    #[serde(default)]
    integer: bool,
}

impl FeatureConfig {
    fn history_policy(&self) -> Option<anyhow::Result<HistoryFeaturePolicy>> {
        match self.role {
            FeatureRoleConfig::History => Some(
                self.history_aggregation
                    .zip(self.history_duplicate_policy)
                    .context("history feature requires aggregation and duplicate policy")
                    .and_then(|(aggregation, duplicate_policy)| {
                        HistoryFeaturePolicy::new(
                            self.name.clone(),
                            aggregation.into_domain(),
                            duplicate_policy.into_domain(),
                        )
                        .domain_context("invalid history feature policy")
                    }),
            ),
            FeatureRoleConfig::FutureCovariate | FeatureRoleConfig::Static => None,
        }
    }

    fn into_domain(self) -> anyhow::Result<FeatureDefinition> {
        let role = match self.role {
            FeatureRoleConfig::History => FeatureRole::History,
            FeatureRoleConfig::FutureCovariate => FeatureRole::FutureCovariate,
            FeatureRoleConfig::Static => FeatureRole::Static,
        };
        let value_type = match self.value_type {
            FeatureTypeConfig::Number => FeatureValueType::Number,
            FeatureTypeConfig::String => FeatureValueType::Text,
            FeatureTypeConfig::Boolean => FeatureValueType::Boolean,
        };
        if role != FeatureRole::History
            && (self.history_aggregation.is_some() || self.history_duplicate_policy.is_some())
        {
            anyhow::bail!("only history features may declare history source policies");
        }
        if value_type == FeatureValueType::Number {
            let mut definition = FeatureDefinition::numeric(
                self.name,
                role,
                self.unit.context("numeric runtime feature requires unit")?,
            )
            .domain_context("invalid numeric feature")?;
            if let Some(constraints) = self.constraints {
                definition = definition
                    .with_numeric_constraints(
                        NumericFeatureConstraints::new(
                            constraints.minimum,
                            constraints.maximum,
                            constraints.integer,
                        )
                        .domain_context("invalid numeric limits")?,
                    )
                    .domain_context("invalid constrained numeric feature")?;
            }
            Ok(definition)
        } else {
            if self.unit.is_some() || self.constraints.is_some() {
                anyhow::bail!("non-numeric runtime feature must omit unit and numeric limits");
            }
            FeatureDefinition::new(self.name, role, value_type)
                .domain_context("invalid non-numeric feature")
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FallbackConfig {
    strategy: String,
    version: String,
    source_feature: String,
    max_output_age_seconds: u64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct BindingConfig {
    id: String,
    revision: u32,
    commissioned: bool,
    #[serde(default)]
    remote_egress_allowed: bool,
    point_features: Vec<PointFeatureConfig>,
    #[serde(default)]
    static_features: Vec<StaticFeatureConfig>,
    artifact: Option<ArtifactConfig>,
}

impl BindingConfig {
    fn into_domain(
        self,
        task: &DataProcessingTask,
    ) -> anyhow::Result<(DataProcessingBinding, Vec<PointAddress>)> {
        if !self.commissioned {
            anyhow::bail!("runtime binding is not commissioned");
        }
        let points = self
            .point_features
            .into_iter()
            .map(PointFeatureConfig::into_domain)
            .collect::<anyhow::Result<Vec<_>>>()?;
        let addresses = points.iter().map(PointFeatureBinding::address).collect();
        let expected_static = task
            .features()
            .iter()
            .filter(|feature| feature.role() == FeatureRole::Static)
            .collect::<Vec<_>>();
        if self.static_features.len() != expected_static.len()
            || expected_static.iter().any(|definition| {
                self.static_features
                    .iter()
                    .filter(|configured| configured.feature == definition.name())
                    .count()
                    != 1
            })
        {
            anyhow::bail!("runtime static values do not exactly cover task static features");
        }
        let static_features = self
            .static_features
            .into_iter()
            .map(|configured| {
                let definition = expected_static
                    .iter()
                    .find(|definition| definition.name() == configured.feature)
                    .copied()
                    .context("runtime static value references an undeclared feature")?;
                configured.into_domain(definition)
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        let mut binding = DataProcessingBinding::new(
            BindingIdentity::new(self.id, self.revision)
                .domain_context("invalid binding identity")?,
            points,
        )?
        .with_static_features(static_features);
        if let Some(artifact) = self.artifact {
            let selector =
                ArtifactSelector::new(artifact.kind, artifact.family, Some(&artifact.version))
                    .domain_context("invalid artifact selector")?
                    .with_digest(artifact.artifact_digest)
                    .domain_context("invalid artifact digest pin")?;
            binding = binding.with_artifact(selector);
        }
        if self.remote_egress_allowed {
            binding = binding.allowing_remote_egress();
        }
        Ok((binding, addresses))
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct StaticFeatureConfig {
    feature: String,
    value: serde_yml::Value,
    quality: QualityConfig,
}

impl StaticFeatureConfig {
    fn into_domain(self, definition: &FeatureDefinition) -> anyhow::Result<StaticFeature> {
        let value = if self.value.is_null() {
            FeatureValue::missing()
        } else {
            match definition.value_type() {
                FeatureValueType::Number => FeatureValue::number(
                    self.value
                        .as_f64()
                        .context("numeric static value is invalid")?,
                )
                .domain_context("numeric static value is invalid")?,
                FeatureValueType::Text => FeatureValue::text(
                    self.value
                        .as_str()
                        .context("text static value is invalid")?,
                ),
                FeatureValueType::Boolean => FeatureValue::boolean(
                    self.value
                        .as_bool()
                        .context("boolean static value is invalid")?,
                ),
            }
        };
        StaticFeature::new(definition.clone(), value, self.quality.into_domain())
            .domain_context("runtime static value violates its task definition")
    }
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum QualityConfig {
    Good,
    Uncertain,
    Substituted,
    Missing,
}

impl QualityConfig {
    const fn into_domain(self) -> SampleQuality {
        match self {
            Self::Good => SampleQuality::Good,
            Self::Uncertain => SampleQuality::Uncertain,
            Self::Substituted => SampleQuality::Substituted,
            Self::Missing => SampleQuality::Missing,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PointFeatureConfig {
    feature: String,
    instance_id: u32,
    point_id: u32,
    kind: PointKindConfig,
    #[serde(default)]
    live_tail: bool,
}

impl PointFeatureConfig {
    fn into_domain(self) -> anyhow::Result<PointFeatureBinding> {
        let kind = match self.kind {
            PointKindConfig::Telemetry => PointKind::Telemetry,
            PointKindConfig::Status => PointKind::Status,
        };
        let point = PointFeatureBinding::new(
            self.feature,
            PointAddress::new(
                InstanceId::new(self.instance_id),
                kind,
                PointId::new(self.point_id),
            ),
        )?;
        Ok(if self.live_tail {
            point.with_live_tail()
        } else {
            point
        })
    }
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum PointKindConfig {
    Telemetry,
    Status,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ArtifactConfig {
    kind: String,
    family: String,
    version: String,
    artifact_digest: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct HistoryConfig {
    routes: Vec<HistoryRouteConfig>,
}

impl HistoryConfig {
    fn into_domain(
        self,
        task: &DataProcessingTask,
        binding: &DataProcessingBinding,
    ) -> anyhow::Result<Vec<SqliteHistoryFeatureRoute>> {
        let specification = task
            .forecast_spec()
            .context("forecast task has no specification")?;
        self.routes
            .into_iter()
            .map(|route| route.into_domain(task, binding, specification.cadence_ms()))
            .collect()
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct HistoryRouteConfig {
    binding_id: String,
    binding_revision: u32,
    feature: String,
    source_ref: String,
    source: HistorySourceConfig,
}

impl HistoryRouteConfig {
    fn into_domain(
        self,
        task: &DataProcessingTask,
        binding: &DataProcessingBinding,
        cadence_ms: u64,
    ) -> anyhow::Result<SqliteHistoryFeatureRoute> {
        if self.binding_id != binding.identity().id()
            || self.binding_revision != binding.identity().revision()
        {
            anyhow::bail!("history route binding identity does not match its route");
        }
        let definition = task
            .features()
            .iter()
            .find(|definition| {
                definition.role() == FeatureRole::History && definition.name() == self.feature
            })
            .cloned()
            .context("history route feature is not declared by the task")?;
        let specification = task
            .forecast_spec()
            .context("forecast task has no specification")?;
        let aggregation = specification.history_aggregation_for(definition.name());
        let duplicate_policy = specification.history_duplicate_policy_for(definition.name());
        match self.source {
            HistorySourceConfig::Stored {
                series_key,
                point_id,
                source_unit: _,
                expected_scale: _,
                expected_offset: _,
                source_sign_convention: _,
            } => Ok(SqliteHistoryFeatureRoute::stored(
                task.identity().clone(),
                binding.identity().clone(),
                definition,
                cadence_ms,
                aggregation,
                duplicate_policy,
                series_key,
                point_id,
                self.source_ref,
            )?),
            HistorySourceConfig::QuarterHourOfDay => Ok(SqliteHistoryFeatureRoute::calendar(
                task.identity().clone(),
                binding.identity().clone(),
                definition,
                cadence_ms,
                CalendarFeature::QuarterHourOfDay,
                self.source_ref,
            )?),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum HistorySourceConfig {
    Stored {
        series_key: String,
        point_id: String,
        source_unit: String,
        expected_scale: f64,
        expected_offset: f64,
        source_sign_convention: Option<String>,
    },
    QuarterHourOfDay,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProcessorConfig {
    endpoint: String,
    id: String,
    version: String,
    contract: String,
    requires_artifact: bool,
    boundary: BoundaryConfig,
    max_frame_cells: usize,
    max_request_bytes: usize,
    connect_timeout_ms: u64,
    request_timeout_ms: u64,
    max_response_bytes: usize,
    bearer_token_env: Option<String>,
}

impl ProcessorConfig {
    fn build(self) -> anyhow::Result<Arc<dyn DataProcessor>> {
        let boundary = match self.boundary {
            BoundaryConfig::Local => DataBoundary::Local,
            BoundaryConfig::Remote => DataBoundary::Remote,
        };
        let descriptor = DataProcessorDescriptor::new(
            self.id,
            self.version,
            vec![TaskKind::Forecast],
            vec![self.contract],
            boundary,
            self.max_frame_cells,
            self.max_request_bytes,
        )?;
        let mut config = HttpDataProcessorConfig::new(
            self.endpoint,
            descriptor,
            Duration::from_millis(self.connect_timeout_ms),
            Duration::from_millis(self.request_timeout_ms),
            self.max_response_bytes,
        )?;
        if let Some(environment) = self.bearer_token_env {
            let value = std::env::var(&environment).with_context(|| {
                format!("processor bearer environment {environment} is missing")
            })?;
            config = config.with_bearer_secret(BearerSecret::new(value)?);
        }
        Ok(Arc::new(HttpDataProcessor::new(config)?))
    }
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum BoundaryConfig {
    Local,
    Remote,
}

fn seconds_to_ms(value: u64, label: &'static str) -> anyhow::Result<u64> {
    value
        .checked_mul(1_000)
        .with_context(|| format!("{label} overflows milliseconds"))
}

trait DomainResultExt<T> {
    fn domain_context(self, context: &'static str) -> anyhow::Result<T>;
}

impl<T> DomainResultExt<T> for Result<T, aether_domain::DomainError> {
    fn domain_context(self, context: &'static str) -> anyhow::Result<T> {
        self.map_err(|error| anyhow::anyhow!("{context}: {error}"))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    use aether_domain::{FeatureRole, TimestampMs};
    use aether_ports::{
        HistoryQuery, HistoryWindow, PortError, PortErrorKind, PortResult, SourcedSegment,
    };

    use super::{
        AuthoritativeSqliteHistoryQuery, HistoryAggregation, HistoryDuplicatePolicy,
        HistorySourceConfig, PhysicalCommissioningPlan, RuntimeConfig,
        validate_physical_history_routes, validate_route_source_coverage,
        validate_sqlite_history_authority,
    };

    const RUNTIME_FIXTURE: &str = include_str!("../tests/fixtures/data-processing-runtime.yaml");

    struct RecordingHistory {
        calls: Arc<AtomicUsize>,
    }

    #[async_trait::async_trait]
    impl HistoryQuery for RecordingHistory {
        async fn query(&self, _window: HistoryWindow) -> PortResult<SourcedSegment> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Err(PortError::new(
                PortErrorKind::Permanent,
                "inner history was called",
            ))
        }
    }

    #[test]
    fn runtime_config_rejects_unknown_fields_and_uncommissioned_bindings() {
        let invalid = "schema: aether.data-processing-runtime.v1\nunknown: true\nroutes: []\n";
        assert!(serde_yml::from_str::<RuntimeConfig>(invalid).is_err());
    }

    #[test]
    fn runtime_fixture_parses_into_the_exact_task_policy() {
        let mut config: RuntimeConfig =
            serde_yml::from_str(RUNTIME_FIXTURE).expect("fixture parses");
        let mut route = config.routes.remove(0);
        route.processor.bearer_token_env = None;
        let task = route.task.into_domain().expect("task is representable");
        let (binding, _) = route
            .binding
            .into_domain(&task)
            .expect("binding is representable");
        let specification = task.forecast_spec().expect("forecast policy exists");

        assert_eq!(
            specification.history_aggregation(),
            HistoryAggregation::Mean
        );
        assert_eq!(
            specification.history_duplicate_policy(),
            HistoryDuplicatePolicy::Latest
        );
        assert_eq!(specification.max_quantiles(), 0);
        assert_eq!(route.processor.id, "example-forecast-processor");
        assert_eq!(
            specification.history_aggregation_for("precipitation"),
            HistoryAggregation::Sum
        );
        assert_eq!(
            specification.history_aggregation_for("signal"),
            HistoryAggregation::Mean
        );
        assert!(
            binding
                .artifact()
                .and_then(|value| value.digest())
                .is_some()
        );
        validate_route_source_coverage(&task, &binding, &route.history)
            .expect("every history feature is mapped exactly once");
        route
            .history
            .into_domain(&task, &binding)
            .expect("SQLite routes match the task policy");
        let processor = route
            .processor
            .build()
            .expect("processor endpoint and limits are composable");
        assert_eq!(processor.descriptor().id(), "example-forecast-processor");
    }

    #[tokio::test]
    async fn data_processing_history_must_be_the_enabled_authoritative_sqlite_file() {
        let pool = sqlx::SqlitePool::connect("sqlite::memory:")
            .await
            .expect("in-memory database opens");
        sqlx::query("CREATE TABLE history_config (key TEXT PRIMARY KEY, value TEXT NOT NULL)")
            .execute(&pool)
            .await
            .expect("history authority table is created");
        for (key, value) in [
            ("storage_enabled", "true"),
            ("storage_backend", "sqlite"),
            ("storage_url", "/data/aether-history.db"),
        ] {
            sqlx::query("INSERT INTO history_config (key, value) VALUES (?, ?)")
                .bind(key)
                .bind(value)
                .execute(&pool)
                .await
                .expect("history authority setting is inserted");
        }

        validate_sqlite_history_authority(&pool, "/data/aether-history.db")
            .await
            .expect("matching SQLite authority is accepted");

        sqlx::query(
            "UPDATE history_config SET value = 'timescaledb' WHERE key = 'storage_backend'",
        )
        .execute(&pool)
        .await
        .expect("backend setting changes");
        assert!(
            validate_sqlite_history_authority(&pool, "/data/aether-history.db")
                .await
                .is_err()
        );
        sqlx::query("UPDATE history_config SET value = 'sqlite' WHERE key = 'storage_backend'")
            .execute(&pool)
            .await
            .expect("backend setting is restored");
        assert!(
            validate_sqlite_history_authority(&pool, "/data/stale-history.db")
                .await
                .is_err()
        );

        sqlx::query("UPDATE history_config SET value = 'false' WHERE key = 'storage_enabled'")
            .execute(&pool)
            .await
            .expect("storage is disabled");
        assert!(
            validate_sqlite_history_authority(&pool, "/data/aether-history.db")
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn physical_history_commissioning_rejects_unit_drift() {
        let pool = sqlx::SqlitePool::connect("sqlite::memory:")
            .await
            .expect("in-memory database opens");
        sqlx::query("CREATE TABLE history_config (key TEXT PRIMARY KEY, value TEXT NOT NULL)")
            .execute(&pool)
            .await
            .expect("history authority table is created");
        for (key, value) in [
            ("storage_enabled", "true"),
            ("storage_backend", "sqlite"),
            ("storage_url", "/data/aether-history.db"),
        ] {
            sqlx::query("INSERT INTO history_config (key, value) VALUES (?, ?)")
                .bind(key)
                .bind(value)
                .execute(&pool)
                .await
                .expect("history authority setting is inserted");
        }
        sqlx::query(
            "CREATE TABLE measurement_routing (\
                instance_id INTEGER, measurement_id INTEGER, channel_id INTEGER, \
                channel_type TEXT, channel_point_id INTEGER, enabled BOOLEAN)",
        )
        .execute(&pool)
        .await
        .expect("routing table is created");
        sqlx::query(
            "CREATE TABLE telemetry_points (\
                channel_id INTEGER, point_id INTEGER, unit TEXT, scale REAL, offset REAL)",
        )
        .execute(&pool)
        .await
        .expect("point table is created");
        sqlx::query("CREATE TABLE channels (channel_id INTEGER PRIMARY KEY, protocol TEXT)")
            .execute(&pool)
            .await
            .expect("channel table is created");
        for channel in [10_i64, 20_i64] {
            sqlx::query("INSERT INTO channels VALUES (?, 'modbus_tcp')")
                .bind(channel)
                .execute(&pool)
                .await
                .expect("channel is inserted");
        }
        for (instance, measurement, channel, unit) in [
            (1001_i64, 1_i64, 10_i64, "1"),
            (2001, 1, 20, "Cel"),
            (2001, 2, 20, "%"),
            (2001, 3, 20, "mm"),
        ] {
            sqlx::query("INSERT INTO measurement_routing VALUES (?, ?, ?, 'T', ?, TRUE)")
                .bind(instance)
                .bind(measurement)
                .bind(channel)
                .bind(measurement)
                .execute(&pool)
                .await
                .expect("route is inserted");
            sqlx::query("INSERT INTO telemetry_points VALUES (?, ?, ?, 1.0, 0.0)")
                .bind(channel)
                .bind(measurement)
                .bind(unit)
                .execute(&pool)
                .await
                .expect("point is inserted");
        }

        let mut config: RuntimeConfig =
            serde_yml::from_str(RUNTIME_FIXTURE).expect("fixture parses");
        let route = config.routes.remove(0);
        let task = route.task.into_domain().expect("task is valid");
        let (binding, _) = route.binding.into_domain(&task).expect("binding is valid");
        validate_physical_history_routes(&pool, &task, &binding, &route.history)
            .await
            .expect("commissioned metadata matches");

        let mut target_from_calendar = route.history.clone();
        target_from_calendar
            .routes
            .iter_mut()
            .find(|history_route| history_route.feature == "signal")
            .expect("signal history route exists")
            .source = HistorySourceConfig::QuarterHourOfDay;
        assert!(
            validate_physical_history_routes(&pool, &task, &binding, &target_from_calendar)
                .await
                .is_err(),
            "a forecast target cannot bypass physical identity and sign commissioning"
        );

        let calls = Arc::new(AtomicUsize::new(0));
        let guarded = AuthoritativeSqliteHistoryQuery {
            settings_database: pool.clone(),
            configured_path: "/data/aether-history.db".to_string(),
            inner: Arc::new(RecordingHistory {
                calls: calls.clone(),
            }),
            commissioning_plans: vec![PhysicalCommissioningPlan {
                task: task.clone(),
                binding: binding.clone(),
                history: route.history.clone(),
            }],
        };
        let signal = task
            .features()
            .iter()
            .find(|feature| feature.role() == FeatureRole::History && feature.name() == "signal")
            .cloned()
            .expect("signal feature is commissioned");
        let guarded_window = HistoryWindow::new(
            task.identity().clone(),
            binding.identity().clone(),
            vec![signal],
            TimestampMs::new(1_000),
            TimestampMs::new(2_000),
            1,
            HistoryAggregation::Mean,
            HistoryDuplicatePolicy::Latest,
        )
        .expect("guarded window is valid");
        let inner_error = guarded
            .query(guarded_window.clone())
            .await
            .expect_err("matching physical authority delegates to the history adapter");
        assert_eq!(inner_error.kind(), PortErrorKind::Permanent);
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        sqlx::query(
            "UPDATE telemetry_points SET unit = 'Cel' WHERE channel_id = 10 AND point_id = 1",
        )
        .execute(&pool)
        .await
        .expect("fixture is mutated");
        assert!(
            validate_physical_history_routes(&pool, &task, &binding, &route.history)
                .await
                .is_err()
        );
        let drift_error = guarded
            .query(guarded_window)
            .await
            .expect_err("online physical metadata drift fails before stale history is read");
        assert_eq!(drift_error.kind(), PortErrorKind::Unavailable);
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        sqlx::query(
            "UPDATE telemetry_points SET unit = '1' WHERE channel_id = 10 AND point_id = 1",
        )
        .execute(&pool)
        .await
        .expect("unit fixture is restored");
        sqlx::query("UPDATE channels SET protocol = 'virtual' WHERE channel_id = 10")
            .execute(&pool)
            .await
            .expect("channel fixture is mutated");
        assert!(
            validate_physical_history_routes(&pool, &task, &binding, &route.history)
                .await
                .is_err()
        );
    }
}
