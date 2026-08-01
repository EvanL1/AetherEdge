//! Governed frame assembly and request-driven data processing.

use std::sync::Arc;

use crate::{
    ApplicationError, PROCESS_DATA_CAPABILITY, PROCESSOR_HEALTH_CAPABILITY, RequestContext,
    SafetyPolicy, TASKS_LIST_CAPABILITY,
};
use aether_data_processing::{
    compute_input_digest, encode_request, encode_result, validate_commissioned_route_contract,
};
use aether_domain::{
    ArtifactSelector, BindingIdentity, DataProcessingRequest, DataProcessingTask, DerivedData,
    FeatureDefinition, FeatureRole, FeatureValue, FeatureValueType, ForecastOptions,
    HistoryAggregation, HistoryFeaturePolicy, PointAddress, PointQuality, ProcessTaskRequest,
    ProcessingFrame, ProcessingOptions, ProcessingOutput, ProcessingResult, ProcessingStatus,
    SampleQuality, Segment, SegmentKind, Series, SourceKind, SourceProvenance, StaticFeature,
    TaskIdentity, TimestampMs, maximum_observation_gap,
};
use aether_ports::{
    AuditOutcome, AuditRecord, AuditSink, Clock, CovariateSource, CovariateWindow, DataBoundary,
    DataProcessor, HistoryQuery, HistoryWindow, LiveState, PortError, PortErrorKind,
    ProcessorHealth,
};
use uuid::Uuid;

/// Maximum time reserved after processing work for a mandatory terminal audit.
pub const DATA_PROCESSING_AUDIT_FINALIZATION_TIMEOUT_MS: u64 = 1_000;

const MAX_AUDIT_DURATION: std::time::Duration =
    std::time::Duration::from_millis(DATA_PROCESSING_AUDIT_FINALIZATION_TIMEOUT_MS);

/// One commissioned task-to-processor route selected by a composition root.
pub struct DataProcessingRoute {
    task: DataProcessingTask,
    binding: DataProcessingBinding,
    processor: Arc<dyn DataProcessor>,
    deadline_ms: u64,
    max_concurrency: usize,
    concurrency: Arc<tokio::sync::Semaphore>,
    remote_egress_preapproved: bool,
}

impl DataProcessingRoute {
    /// Creates a route only when the processor advertises the task contract and finite limits.
    pub fn new(
        task: DataProcessingTask,
        binding: DataProcessingBinding,
        processor: Arc<dyn DataProcessor>,
        deadline_ms: u64,
    ) -> Result<Self, ApplicationError> {
        let descriptor = processor.descriptor();
        validate_commissioned_route_contract(
            &task,
            binding.identity(),
            binding.artifact(),
            descriptor.id(),
            descriptor.version(),
            descriptor.supported_contracts(),
        )
        .map_err(|_| {
            ApplicationError::InvalidProcessingConfiguration(
                "route cannot be represented by the v1 processor contract".to_string(),
            )
        })?;
        let maximum_frame_cells = maximum_task_cell_count(&task)?;
        if deadline_ms == 0
            || !descriptor.supports(task.kind())
            || !descriptor.supports_contract(task.processor_contract())
            || descriptor.max_frame_samples() < maximum_frame_cells
            || (descriptor.data_boundary() == DataBoundary::Remote
                && (!task.remote_egress_allowed() || !binding.remote_egress_allowed()))
        {
            return Err(ApplicationError::InvalidProcessingConfiguration(
                "processor capabilities or route limits do not satisfy the task".to_string(),
            ));
        }
        validate_commissioned_binding(&task, &binding)?;
        Ok(Self {
            task,
            binding,
            processor,
            deadline_ms,
            max_concurrency: 1,
            concurrency: Arc::new(tokio::sync::Semaphore::new(1)),
            remote_egress_preapproved: false,
        })
    }

    /// Sets the hard bound for concurrent frame assembly and processor calls.
    pub fn with_max_concurrency(
        mut self,
        max_concurrency: usize,
    ) -> Result<Self, ApplicationError> {
        if max_concurrency == 0 || max_concurrency > 64 {
            return Err(ApplicationError::InvalidProcessingConfiguration(
                "route concurrency must be in 1..=64".to_string(),
            ));
        }
        self.max_concurrency = max_concurrency;
        self.concurrency = Arc::new(tokio::sync::Semaphore::new(max_concurrency));
        Ok(self)
    }

    /// Marks a remote route as pre-approved by deployment egress policy.
    #[must_use]
    pub fn with_preapproved_remote_egress(mut self) -> Self {
        self.remote_egress_preapproved = true;
        self
    }
}

/// One commissioned semantic feature resolved to a read-only Aether point.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PointFeatureBinding {
    feature: String,
    address: PointAddress,
    live_tail: bool,
}

impl PointFeatureBinding {
    /// Resolves one feature to a telemetry or status point.
    pub fn new(
        feature: impl Into<String>,
        address: PointAddress,
    ) -> Result<Self, ApplicationError> {
        let feature = feature.into();
        if feature.trim().is_empty() || address.kind().is_writable() {
            return Err(ApplicationError::InvalidProcessingConfiguration(
                "point feature bindings require a name and a read-only point".to_string(),
            ));
        }
        Ok(Self {
            feature,
            address,
            live_tail: false,
        })
    }

    /// Allows an exactly cadence-aligned live sample to complete the history tail.
    #[must_use]
    pub const fn with_live_tail(mut self) -> Self {
        self.live_tail = true;
        self
    }

    /// Returns the task-local feature name.
    #[must_use]
    pub fn feature(&self) -> &str {
        &self.feature
    }

    /// Returns the read-only live point address.
    #[must_use]
    pub const fn address(&self) -> PointAddress {
        self.address
    }

    /// Returns whether this feature participates in the transactional mapped live-tail merge.
    #[must_use]
    pub const fn live_tail(&self) -> bool {
        self.live_tail
    }
}

/// Site-commissioned task inputs and approved artifact selection.
#[derive(Debug, Clone, PartialEq)]
pub struct DataProcessingBinding {
    identity: BindingIdentity,
    point_features: Vec<PointFeatureBinding>,
    static_features: Vec<StaticFeature>,
    artifact: Option<ArtifactSelector>,
    remote_egress_allowed: bool,
}

impl DataProcessingBinding {
    /// Creates a commissioned binding with at least one semantic point mapping.
    pub fn new(
        identity: BindingIdentity,
        point_features: Vec<PointFeatureBinding>,
    ) -> Result<Self, ApplicationError> {
        if point_features.is_empty()
            || point_features.iter().enumerate().any(|(index, binding)| {
                point_features[..index]
                    .iter()
                    .any(|seen| seen.feature == binding.feature || seen.address == binding.address)
            })
        {
            return Err(ApplicationError::InvalidProcessingConfiguration(
                "commissioned point mappings must be non-empty and unique".to_string(),
            ));
        }
        Ok(Self {
            identity,
            point_features,
            static_features: Vec::new(),
            artifact: None,
            remote_egress_allowed: false,
        })
    }

    /// Adds commissioned constants declared by the task.
    #[must_use]
    pub fn with_static_features(mut self, static_features: Vec<StaticFeature>) -> Self {
        self.static_features = static_features;
        self
    }

    /// Selects an approved processor artifact without activating it.
    #[must_use]
    pub fn with_artifact(mut self, artifact: ArtifactSelector) -> Self {
        self.artifact = Some(artifact);
        self
    }

    /// Explicitly permits this commissioned binding to cross a remote boundary.
    #[must_use]
    pub const fn allowing_remote_egress(mut self) -> Self {
        self.remote_egress_allowed = true;
        self
    }

    /// Returns the commissioned identity and revision.
    #[must_use]
    pub const fn identity(&self) -> &BindingIdentity {
        &self.identity
    }

    /// Returns whether this site binding permits remote processor egress.
    #[must_use]
    pub const fn remote_egress_allowed(&self) -> bool {
        self.remote_egress_allowed
    }

    /// Returns semantic read-only point mappings.
    #[must_use]
    pub fn point_features(&self) -> &[PointFeatureBinding] {
        &self.point_features
    }

    /// Returns commissioned static values.
    #[must_use]
    pub fn static_features(&self) -> &[StaticFeature] {
        &self.static_features
    }

    /// Returns the approved artifact selector.
    #[must_use]
    pub const fn artifact(&self) -> Option<&ArtifactSelector> {
        self.artifact.as_ref()
    }
}

/// Machine-readable task and route discovery entry.
#[derive(Debug, Clone, PartialEq)]
pub struct DataProcessingTaskSummary {
    task: TaskIdentity,
    definition: DataProcessingTask,
    binding: BindingIdentity,
    artifact: Option<ArtifactSelector>,
    processor_id: String,
    processor_version: String,
    processor_contract: String,
    data_boundary: DataBoundary,
    deadline_ms: u64,
    max_concurrency: usize,
    max_frame_samples: usize,
    max_request_bytes: usize,
}

impl DataProcessingTaskSummary {
    /// Returns the task identity.
    #[must_use]
    pub const fn task(&self) -> &TaskIdentity {
        &self.task
    }

    /// Returns the complete semantic task definition without physical source coordinates.
    #[must_use]
    pub const fn definition(&self) -> &DataProcessingTask {
        &self.definition
    }

    /// Returns the commissioned binding identity.
    #[must_use]
    pub const fn binding(&self) -> &BindingIdentity {
        &self.binding
    }

    /// Returns the approved artifact selector, including its optional digest pin.
    #[must_use]
    pub const fn artifact(&self) -> Option<&ArtifactSelector> {
        self.artifact.as_ref()
    }

    /// Returns the selected processor identity.
    #[must_use]
    pub fn processor_id(&self) -> &str {
        &self.processor_id
    }

    /// Returns the commissioned processor implementation version.
    #[must_use]
    pub fn processor_version(&self) -> &str {
        &self.processor_version
    }

    /// Returns the exact processor wire contract.
    #[must_use]
    pub fn processor_contract(&self) -> &str {
        &self.processor_contract
    }

    /// Returns whether the route is local or crosses remote egress.
    #[must_use]
    pub const fn data_boundary(&self) -> DataBoundary {
        self.data_boundary
    }

    /// Returns the hard frame-assembly and processor-work deadline.
    ///
    /// Mandatory terminal audit finalization has a separate bounded allowance
    /// of at most one second after this work deadline.
    #[must_use]
    pub const fn deadline_ms(&self) -> u64 {
        self.deadline_ms
    }

    /// Returns the hard route concurrency bound.
    #[must_use]
    pub const fn max_concurrency(&self) -> usize {
        self.max_concurrency
    }

    /// Returns the processor-advertised frame-cell bound.
    #[must_use]
    pub const fn max_frame_samples(&self) -> usize {
        self.max_frame_samples
    }

    /// Returns the processor-advertised encoded request bound.
    #[must_use]
    pub const fn max_request_bytes(&self) -> usize {
        self.max_request_bytes
    }
}

/// Machine-readable processor health entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessorHealthSummary {
    processor_id: String,
    health: ProcessorHealth,
}

impl ProcessorHealthSummary {
    /// Returns the processor identity.
    #[must_use]
    pub fn processor_id(&self) -> &str {
        &self.processor_id
    }

    /// Returns current readiness.
    #[must_use]
    pub const fn health(&self) -> ProcessorHealth {
        self.health
    }
}

/// Transport-neutral data-processing use cases shared by AI, CLI, and HTTP.
pub struct DataProcessingApplication {
    routes: Vec<DataProcessingRoute>,
    history: Arc<dyn HistoryQuery>,
    covariates: Option<Arc<dyn CovariateSource>>,
    live_state: Arc<dyn LiveState>,
    audit: Arc<dyn AuditSink>,
    clock: Arc<dyn Clock>,
    policy: SafetyPolicy,
}

impl DataProcessingApplication {
    /// Composes enabled routes with read-only source and audit capabilities.
    pub fn new(
        routes: Vec<DataProcessingRoute>,
        history: Arc<dyn HistoryQuery>,
        covariates: Option<Arc<dyn CovariateSource>>,
        live_state: Arc<dyn LiveState>,
        audit: Arc<dyn AuditSink>,
        clock: Arc<dyn Clock>,
        policy: SafetyPolicy,
    ) -> Result<Self, ApplicationError> {
        for (index, route) in routes.iter().enumerate() {
            if routes[..index].iter().any(|seen| {
                seen.task.identity() == route.task.identity()
                    && seen.binding.identity() == route.binding.identity()
            }) {
                return Err(ApplicationError::InvalidProcessingConfiguration(
                    "task and binding routes must be unique".to_string(),
                ));
            }
            validate_commissioned_binding(&route.task, &route.binding)?;
            let needs_covariates = route
                .task
                .features()
                .iter()
                .any(|feature| feature.role() == FeatureRole::FutureCovariate);
            if needs_covariates && covariates.is_none() {
                return Err(ApplicationError::InvalidProcessingConfiguration(
                    "task requires a CovariateSource".to_string(),
                ));
            }
        }
        Ok(Self {
            routes,
            history,
            covariates,
            live_state,
            audit,
            clock,
            policy,
        })
    }

    /// Lists enabled, commissioned task routes after authorization.
    pub async fn list_tasks(
        &self,
        context: &RequestContext,
    ) -> Result<Vec<DataProcessingTaskSummary>, ApplicationError> {
        self.policy.authorize(TASKS_LIST_CAPABILITY, context)?;
        Ok(self
            .routes
            .iter()
            .map(|route| {
                let descriptor = route.processor.descriptor();
                DataProcessingTaskSummary {
                    task: route.task.identity().clone(),
                    definition: route.task.clone(),
                    binding: route.binding.identity().clone(),
                    artifact: route.binding.artifact().cloned(),
                    processor_id: descriptor.id().to_string(),
                    processor_version: descriptor.version().to_string(),
                    processor_contract: route.task.processor_contract().to_string(),
                    data_boundary: descriptor.data_boundary(),
                    deadline_ms: route.deadline_ms,
                    max_concurrency: route.max_concurrency,
                    max_frame_samples: descriptor.max_frame_samples(),
                    max_request_bytes: descriptor.max_request_bytes(),
                }
            })
            .collect())
    }

    /// Reports processor readiness without sending task data.
    pub async fn processor_health(
        &self,
        context: &RequestContext,
    ) -> Result<Vec<ProcessorHealthSummary>, ApplicationError> {
        self.policy
            .authorize(PROCESSOR_HEALTH_CAPABILITY, context)?;
        let mut health = Vec::with_capacity(self.routes.len());
        for route in &self.routes {
            health.push(ProcessorHealthSummary {
                processor_id: route.processor.descriptor().id().to_string(),
                health: route
                    .processor
                    .health()
                    .await
                    .unwrap_or(ProcessorHealth::Unavailable),
            });
        }
        Ok(health)
    }

    /// Authorizes, assembles, audits, processes, validates, and stamps one task result.
    pub async fn process(
        &self,
        context: &RequestContext,
        request: ProcessTaskRequest,
    ) -> Result<DerivedData, ApplicationError> {
        if let Err(error) = self.policy.authorize(PROCESS_DATA_CAPABILITY, context) {
            self.record_audit(context, AuditOutcome::Rejected, Some(error.to_string()))
                .await?;
            return Err(error);
        }
        if request.as_of() > context.timestamp() {
            let error = ApplicationError::InvalidProcessingRequest(
                aether_domain::DomainError::InvalidProcessingWindow,
            );
            self.record_audit(context, AuditOutcome::Rejected, Some(error.to_string()))
                .await?;
            return Err(error);
        }
        let Some(route) = self.routes.iter().find(|route| {
            route.task.identity() == request.task() && route.binding.identity() == request.binding()
        }) else {
            let error = ApplicationError::InvalidProcessingConfiguration(
                "requested task or binding is not enabled".to_string(),
            );
            self.record_audit(context, AuditOutcome::Rejected, Some(error.to_string()))
                .await?;
            return Err(error);
        };
        let spec = route.task.forecast_spec().ok_or_else(|| {
            ApplicationError::InvalidProcessingConfiguration(
                "task has no typed execution specification".to_string(),
            )
        })?;
        if !request.as_of().get().is_multiple_of(spec.cadence_ms()) {
            let error = ApplicationError::InvalidProcessingRequest(
                aether_domain::DomainError::InvalidProcessingWindow,
            );
            self.record_audit(context, AuditOutcome::Rejected, Some(error.to_string()))
                .await?;
            return Err(error);
        }
        if route.processor.descriptor().data_boundary() == DataBoundary::Remote
            && !route.remote_egress_preapproved
            && !context.confirmed()
        {
            let error = ApplicationError::ConfirmationRequired {
                capability: PROCESS_DATA_CAPABILITY.name(),
            };
            self.record_audit(context, AuditOutcome::Rejected, Some(error.to_string()))
                .await?;
            return Err(error);
        }

        let _route_permit = match route.concurrency.clone().try_acquire_owned() {
            Ok(permit) => permit,
            Err(_) => {
                let error = ApplicationError::ProcessingUnavailable {
                    reason: "ROUTE_CONCURRENCY_LIMIT".to_string(),
                    retryable: true,
                    retry_after_ms: Some(250),
                };
                self.record_audit(context, AuditOutcome::Rejected, Some(error.to_string()))
                    .await?;
                return Err(error);
            },
        };
        let route_budget = std::time::Duration::from_millis(route.deadline_ms);
        let route_started = tokio::time::Instant::now();
        let work_deadline = route_started + route_budget;
        let audit_deadline = work_deadline + MAX_AUDIT_DURATION;

        let frame = match tokio::time::timeout(
            work_deadline.saturating_duration_since(tokio::time::Instant::now()),
            self.assemble_frame(route, &request),
        )
        .await
        {
            Ok(Ok(frame)) => frame,
            Err(_) => {
                let error = aether_ports::PortError::new(
                    aether_ports::PortErrorKind::Timeout,
                    "data-processing frame assembly exceeded the route deadline",
                );
                self.record_audit_within(
                    context,
                    AuditOutcome::Failed,
                    Some(error.to_string()),
                    audit_deadline,
                )
                .await?;
                return Err(ApplicationError::Port(error));
            },
            Ok(Err(error)) => {
                self.record_audit_within(
                    context,
                    AuditOutcome::Failed,
                    Some(error.to_string()),
                    audit_deadline,
                )
                .await?;
                return Err(error);
            },
        };
        let processor_request = (|| {
            if frame_cell_count(&frame)? > route.processor.descriptor().max_frame_samples() {
                return Err(ApplicationError::InvalidProcessingRequest(
                    aether_domain::DomainError::InvalidProcessingWindow,
                ));
            }
            let input_digest = compute_input_digest(
                route.task.identity(),
                route.binding.identity(),
                route.task.processor_contract(),
                route.binding.artifact(),
                &frame,
                request.options(),
            )
            .map_err(ApplicationError::ProcessingCodec)?;
            let processor_request_id =
                stable_processing_id("processor-request", context.request_id(), &input_digest);
            let deadline = TimestampMs::new(
                context
                    .timestamp()
                    .get()
                    .checked_add(route.deadline_ms)
                    .ok_or(aether_domain::DomainError::InvalidProcessingWindow)
                    .map_err(ApplicationError::InvalidProcessingRequest)?,
            );
            let processor_request = DataProcessingRequest::new(
                processor_request_id,
                route.task.identity().clone(),
                route.binding.identity().clone(),
                frame,
                context.timestamp(),
                deadline,
                route.task.processor_contract(),
                route.binding.artifact().cloned(),
                input_digest,
                request.options().clone(),
            )
            .map_err(ApplicationError::InvalidProcessingRequest)?;
            let encoded_bytes = encode_request(&processor_request)
                .map_err(ApplicationError::ProcessingCodec)?
                .len();
            let max_bytes = route.processor.descriptor().max_request_bytes();
            if encoded_bytes > max_bytes {
                return Err(ApplicationError::ProcessingRequestTooLarge {
                    encoded_bytes,
                    max_bytes,
                });
            }
            Ok(processor_request)
        })();
        let processor_request = match processor_request {
            Ok(request) => request,
            Err(error) => {
                self.record_audit_within(
                    context,
                    AuditOutcome::Rejected,
                    Some(error.to_string()),
                    audit_deadline,
                )
                .await?;
                return Err(error);
            },
        };
        let audit_detail = Some(format!(
            "task={} binding={} digest={} processor={} processor_request={}",
            route.task.identity().id(),
            route.binding.identity().id(),
            processor_request.input_digest(),
            route.processor.descriptor().id(),
            processor_request.request_id(),
        ));
        self.record_audit_within(
            context,
            AuditOutcome::Attempted,
            audit_detail,
            work_deadline,
        )
        .await?;

        let remaining_budget = work_deadline.saturating_duration_since(tokio::time::Instant::now());
        let result = match tokio::time::timeout(
            remaining_budget,
            route.processor.process(processor_request.clone()),
        )
        .await
        {
            Err(_) => {
                let error = aether_ports::PortError::new(
                    aether_ports::PortErrorKind::Timeout,
                    "data processor exceeded the route deadline",
                );
                self.record_audit_within(
                    context,
                    AuditOutcome::Failed,
                    Some(error.to_string()),
                    audit_deadline,
                )
                .await?;
                return Err(ApplicationError::ProcessorFailed(error));
            },
            Ok(result) => result,
        };
        let result = match result {
            Ok(result) => result,
            Err(error) => {
                self.record_audit_within(
                    context,
                    AuditOutcome::Failed,
                    Some(error.to_string()),
                    audit_deadline,
                )
                .await?;
                return Err(ApplicationError::ProcessorFailed(error));
            },
        };
        if let Err(error) = validate_processor_result(route, &processor_request, &result) {
            self.record_audit_within(
                context,
                AuditOutcome::Rejected,
                Some(error.to_string()),
                audit_deadline,
            )
            .await?;
            return Err(error);
        }
        if result.status() == ProcessingStatus::Unavailable {
            let unavailable = result.unavailable().ok_or_else(|| {
                ApplicationError::InvalidProcessorResult(
                    "unavailable result has no reason metadata".to_string(),
                )
            })?;
            let error = ApplicationError::ProcessingUnavailable {
                reason: unavailable.reason().to_string(),
                retryable: unavailable.retryable(),
                retry_after_ms: unavailable.retry_after_ms(),
            };
            self.record_audit_within(
                context,
                AuditOutcome::Failed,
                Some(error.to_string()),
                audit_deadline,
            )
            .await?;
            return Err(error);
        }

        let accepted_at = match self.clock.now() {
            Ok(accepted_at) => accepted_at,
            Err(error) => {
                let error = ApplicationError::Port(error);
                self.record_audit_within(
                    context,
                    AuditOutcome::Failed,
                    Some(error.to_string()),
                    audit_deadline,
                )
                .await?;
                return Err(error);
            },
        };
        if accepted_at < result.produced_at() || accepted_at > processor_request.deadline() {
            let error = ApplicationError::InvalidProcessorResult(
                "processor completion is outside the trusted acceptance window".to_string(),
            );
            self.record_audit_within(
                context,
                AuditOutcome::Rejected,
                Some(error.to_string()),
                audit_deadline,
            )
            .await?;
            return Err(error);
        }
        let result_id =
            stable_processing_id("derived-data", result.request_id(), result.input_digest());
        let derived = DerivedData::accept(
            result_id,
            accepted_at,
            processor_request.frame().quality().clone(),
            result,
        )
        .map_err(|error| {
            ApplicationError::InvalidProcessorResult(format!(
                "accepted derived data is inconsistent: {error}"
            ))
        });
        let derived = match derived {
            Ok(derived) => derived,
            Err(error) => {
                self.record_audit_within(
                    context,
                    AuditOutcome::Rejected,
                    Some(error.to_string()),
                    audit_deadline,
                )
                .await?;
                return Err(error);
            },
        };
        self.record_audit_within(context, AuditOutcome::Succeeded, None, audit_deadline)
            .await?;
        Ok(derived)
    }

    async fn assemble_frame(
        &self,
        route: &DataProcessingRoute,
        request: &ProcessTaskRequest,
    ) -> Result<ProcessingFrame, ApplicationError> {
        let spec = route.task.forecast_spec().ok_or_else(|| {
            ApplicationError::InvalidProcessingConfiguration(
                "task has no forecast specification".to_string(),
            )
        })?;
        let options = forecast_options(request.options());
        if options.horizon_steps() > spec.max_horizon_steps()
            || options.quantiles().len() > spec.max_quantiles()
        {
            return Err(ApplicationError::InvalidProcessingRequest(
                aether_domain::DomainError::InvalidProcessingWindow,
            ));
        }
        let history_features: Vec<_> = route
            .task
            .features()
            .iter()
            .filter(|feature| feature.role() == FeatureRole::History)
            .cloned()
            .collect();
        let history_policies = history_features
            .iter()
            .map(|feature| {
                HistoryFeaturePolicy::new(
                    feature.name(),
                    spec.history_aggregation_for(feature.name()),
                    spec.history_duplicate_policy_for(feature.name()),
                )
                .map_err(ApplicationError::InvalidProcessingRequest)
            })
            .collect::<Result<Vec<_>, _>>()?;
        let history_span = spec
            .cadence_ms()
            .checked_mul(
                u64::try_from(spec.history_steps().saturating_sub(1)).map_err(|_| {
                    ApplicationError::InvalidProcessingRequest(
                        aether_domain::DomainError::InvalidProcessingWindow,
                    )
                })?,
            )
            .ok_or(aether_domain::DomainError::InvalidProcessingWindow)
            .map_err(ApplicationError::InvalidProcessingRequest)?;
        let history_start = request
            .as_of()
            .get()
            .checked_sub(history_span)
            .ok_or(aether_domain::DomainError::InvalidProcessingWindow)
            .map(TimestampMs::new)
            .map_err(ApplicationError::InvalidProcessingRequest)?;
        let history_end = request
            .as_of()
            .get()
            .checked_add(spec.cadence_ms())
            .map(TimestampMs::new)
            .ok_or(aether_domain::DomainError::InvalidProcessingWindow)
            .map_err(ApplicationError::InvalidProcessingRequest)?;
        let history = self
            .history
            .query(
                HistoryWindow::new(
                    route.task.identity().clone(),
                    route.binding.identity().clone(),
                    history_features.clone(),
                    history_start,
                    history_end,
                    spec.history_steps(),
                    spec.history_aggregation(),
                    spec.history_duplicate_policy(),
                )
                .and_then(|window| window.with_feature_policies(history_policies))
                .and_then(|window| window.with_cutoff(request.as_of()))
                .map_err(ApplicationError::Port)?,
            )
            .await
            .map_err(ApplicationError::HistoryQueryFailed)?;
        let expected_history_times =
            regular_timestamps(history_start, spec.cadence_ms(), spec.history_steps())?;
        validate_segment(
            history.segment(),
            &history_features,
            &expected_history_times,
        )?;
        validate_feature_constraints(history.segment())?;
        validate_segment_provenance(
            history.provenance(),
            &history_features,
            SegmentKind::History,
        )?;
        let (history, live_tail_included) = self
            .merge_live_tail(route, request.as_of(), history)
            .await?;

        let future_features: Vec<_> = route
            .task
            .features()
            .iter()
            .filter(|feature| feature.role() == FeatureRole::FutureCovariate)
            .cloned()
            .collect();
        let future = if future_features.is_empty() {
            None
        } else {
            let start = request
                .as_of()
                .get()
                .checked_add(spec.cadence_ms())
                .map(TimestampMs::new)
                .ok_or(aether_domain::DomainError::InvalidProcessingWindow)
                .map_err(ApplicationError::InvalidProcessingRequest)?;
            let span = spec
                .cadence_ms()
                .checked_mul(u64::try_from(options.horizon_steps()).map_err(|_| {
                    ApplicationError::InvalidProcessingRequest(
                        aether_domain::DomainError::InvalidProcessingWindow,
                    )
                })?)
                .ok_or(aether_domain::DomainError::InvalidProcessingWindow)
                .map_err(ApplicationError::InvalidProcessingRequest)?;
            let end = start
                .get()
                .checked_add(span)
                .map(TimestampMs::new)
                .ok_or(aether_domain::DomainError::InvalidProcessingWindow)
                .map_err(ApplicationError::InvalidProcessingRequest)?;
            let source = self.covariates.as_ref().ok_or_else(|| {
                ApplicationError::InvalidProcessingConfiguration(
                    "task requires a CovariateSource".to_string(),
                )
            })?;
            let sourced = source
                .resolve(
                    CovariateWindow::new(
                        route.binding.identity().clone(),
                        future_features.clone(),
                        request.as_of(),
                        start,
                        end,
                        options.horizon_steps(),
                    )
                    .map_err(ApplicationError::Port)?,
                )
                .await
                .map_err(ApplicationError::CovariateSourceFailed)?;
            let expected = regular_timestamps(start, spec.cadence_ms(), options.horizon_steps())?;
            validate_segment(sourced.segment(), &future_features, &expected)?;
            validate_feature_constraints(sourced.segment())?;
            validate_segment_provenance(
                sourced.provenance(),
                &future_features,
                SegmentKind::FutureCovariates,
            )?;
            Some(sourced)
        };

        let mut provenance = history.provenance().to_vec();
        if let Some(future) = &future {
            provenance.extend_from_slice(future.provenance());
        }
        provenance.extend(
            route
                .binding
                .static_features()
                .iter()
                .map(|feature| {
                    SourceProvenance::new(
                        SegmentKind::StaticFeatures,
                        feature.definition().name(),
                        SourceKind::Constant,
                        None,
                        request.as_of(),
                    )
                    .map_err(ApplicationError::InvalidProcessingRequest)
                })
                .collect::<Result<Vec<_>, _>>()?,
        );
        let static_definitions: Vec<_> = route
            .binding
            .static_features()
            .iter()
            .map(|feature| feature.definition().clone())
            .collect();
        validate_segment_provenance(&provenance, &history_features, SegmentKind::History)?;
        validate_segment_provenance(&provenance, &future_features, SegmentKind::FutureCovariates)?;
        validate_segment_provenance(
            &provenance,
            &static_definitions,
            SegmentKind::StaticFeatures,
        )?;
        let expected_provenance_count = history_features
            .len()
            .checked_add(future_features.len())
            .and_then(|count| count.checked_add(static_definitions.len()))
            .ok_or(ApplicationError::InvalidProcessingRequest(
                aether_domain::DomainError::InvalidProcessingWindow,
            ))?;
        if provenance.len() != expected_provenance_count {
            return Err(ApplicationError::InputQualityRejected(
                aether_domain::DomainError::InvalidFrameQuality,
            ));
        }
        let input_watermark = provenance
            .iter()
            .filter(|source| {
                !matches!(
                    source.source_kind(),
                    SourceKind::Calendar | SourceKind::Constant
                )
            })
            .map(SourceProvenance::watermark)
            .max()
            .ok_or({
                ApplicationError::InputQualityRejected(
                    aether_domain::DomainError::InvalidFrameQuality,
                )
            })?;
        if provenance.iter().any(|source| {
            source.watermark() > request.as_of()
                || source
                    .issued_at()
                    .is_some_and(|issued| issued > request.as_of())
        }) {
            return Err(ApplicationError::InputQualityRejected(
                aether_domain::DomainError::InvalidProcessingWindow,
            ));
        }
        if spec.requires_future_issue_time()
            && provenance.iter().any(|source| {
                source.segment() == SegmentKind::FutureCovariates
                    && !matches!(
                        source.source_kind(),
                        SourceKind::Calendar | SourceKind::Constant
                    )
                    && source.issued_at().is_none()
            })
        {
            return Err(ApplicationError::InputQualityRejected(
                aether_domain::DomainError::InvalidFrameQuality,
            ));
        }
        if spec.max_input_age_ms().is_some_and(|max_age_ms| {
            provenance
                .iter()
                .filter(|source| {
                    !matches!(
                        source.source_kind(),
                        SourceKind::Calendar | SourceKind::Constant
                    )
                })
                .any(|source| {
                    request
                        .as_of()
                        .get()
                        .saturating_sub(source.watermark().get())
                        > max_age_ms
                })
        }) {
            return Err(ApplicationError::InputQualityRejected(
                aether_domain::DomainError::InvalidFrameQuality,
            ));
        }
        let (missing, substituted, cells) = frame_cell_counts(
            history.segment(),
            future.as_ref().map(|value| value.segment()),
            route.binding.static_features(),
        );
        let missing_ratio = if cells == 0 {
            0.0
        } else {
            missing as f64 / cells as f64
        };
        if missing_ratio > spec.max_missing_ratio() {
            return Err(ApplicationError::InputQualityRejected(
                aether_domain::DomainError::InvalidFrameQuality,
            ));
        }
        let max_gap_ms = maximum_observation_gap(history.segment(), spec.cadence_ms());
        if spec
            .max_gap_ms()
            .is_some_and(|allowed_gap_ms| max_gap_ms > allowed_gap_ms)
        {
            return Err(ApplicationError::InputQualityRejected(
                aether_domain::DomainError::InvalidFrameQuality,
            ));
        }
        let quality = aether_domain::FrameQuality::new(
            input_watermark,
            missing_ratio,
            max_gap_ms,
            live_tail_included,
            substituted,
        )
        .map_err(ApplicationError::InputQualityRejected)?;
        ProcessingFrame::new(
            request.as_of(),
            spec.cadence_ms(),
            history.into_parts().0,
            future.map(|value| value.into_parts().0),
            route.binding.static_features().to_vec(),
            quality,
            provenance,
        )
        .map_err(ApplicationError::InputQualityRejected)
    }

    async fn merge_live_tail(
        &self,
        route: &DataProcessingRoute,
        as_of: TimestampMs,
        history: aether_ports::SourcedSegment,
    ) -> Result<(aether_ports::SourcedSegment, bool), ApplicationError> {
        let mappings: Vec<_> = route
            .binding
            .point_features()
            .iter()
            .filter(|binding| binding.live_tail())
            .collect();
        if mappings.is_empty() {
            return Ok((history, false));
        }
        if history.segment().timestamps().last().copied() != Some(as_of) {
            return Ok((history, false));
        }
        let addresses: Vec<_> = mappings.iter().map(|binding| binding.address()).collect();
        let Ok(samples) = self.live_state.read_many(&addresses).await else {
            // A live tail is an optimization over already-authoritative stored history.
            // Its absence must not make the deterministic history frame unavailable.
            return Ok((history, false));
        };
        if samples.len() != mappings.len() {
            return Ok((history, false));
        }
        let mut accepted = Vec::with_capacity(samples.len());
        for (mapping, sample) in mappings.into_iter().zip(samples) {
            let Some(sample) = sample else {
                return Ok((history, false));
            };
            if sample.address() != mapping.address()
                || !sample.value().is_finite()
                || sample.timestamp() != as_of
                || !matches!(
                    sample.quality(),
                    PointQuality::Good | PointQuality::Uncertain
                )
            {
                return Ok((history, false));
            }
            accepted.push((mapping, sample));
        }

        let timestamps = history.segment().timestamps().to_vec();
        let mut series = Vec::with_capacity(history.segment().series().len());
        let mut provenance = Vec::with_capacity(history.segment().series().len());
        for stored in history.segment().series() {
            let Some((mapping, sample)) = accepted
                .iter()
                .find(|(mapping, _)| mapping.feature() == stored.definition().name())
            else {
                series.push(stored.clone());
                let stored_provenance = history
                    .provenance()
                    .iter()
                    .find(|source| source.feature() == stored.definition().name())
                    .ok_or_else(|| {
                        ApplicationError::InvalidProcessingConfiguration(
                            "history provenance is incomplete".to_string(),
                        )
                    })?;
                provenance.push(stored_provenance.clone());
                continue;
            };
            let mut values = stored.values().to_vec();
            let latest = values.last_mut().ok_or_else(|| {
                ApplicationError::InvalidProcessingConfiguration(
                    "live-tail history series is empty".to_string(),
                )
            })?;
            *latest = FeatureValue::number(sample.value())
                .map_err(ApplicationError::InvalidProcessingRequest)?;
            let mut quality = stored.quality().to_vec();
            let latest_quality = quality.last_mut().ok_or_else(|| {
                ApplicationError::InvalidProcessingConfiguration(
                    "live-tail history quality is empty".to_string(),
                )
            })?;
            *latest_quality = match sample.quality() {
                PointQuality::Good => SampleQuality::Good,
                PointQuality::Uncertain => SampleQuality::Uncertain,
                PointQuality::Bad | PointQuality::Unavailable => {
                    return Ok((history, false));
                },
            };
            series.push(
                Series::new(stored.definition().clone(), values, quality)
                    .map_err(ApplicationError::InvalidProcessingRequest)?,
            );
            let source_ref = history
                .provenance()
                .iter()
                .find(|source| source.feature() == mapping.feature())
                .and_then(SourceProvenance::source_ref);
            provenance.push(
                SourceProvenance::new(
                    SegmentKind::History,
                    mapping.feature(),
                    SourceKind::HistoryAndLive,
                    source_ref,
                    sample.timestamp(),
                )
                .map_err(ApplicationError::InvalidProcessingRequest)?,
            );
        }
        let segment =
            Segment::new(timestamps, series).map_err(ApplicationError::InvalidProcessingRequest)?;
        let sourced = aether_ports::SourcedSegment::new(segment, provenance)
            .map_err(ApplicationError::Port)?;
        Ok((sourced, true))
    }

    async fn record_audit(
        &self,
        context: &RequestContext,
        outcome: AuditOutcome,
        detail: Option<String>,
    ) -> Result<(), ApplicationError> {
        self.record_audit_for(context, outcome, detail, MAX_AUDIT_DURATION)
            .await
    }

    async fn record_audit_within(
        &self,
        context: &RequestContext,
        outcome: AuditOutcome,
        detail: Option<String>,
        deadline: tokio::time::Instant,
    ) -> Result<(), ApplicationError> {
        let remaining = deadline
            .saturating_duration_since(tokio::time::Instant::now())
            .min(MAX_AUDIT_DURATION);
        self.record_audit_for(context, outcome, detail, remaining)
            .await
    }

    async fn record_audit_for(
        &self,
        context: &RequestContext,
        outcome: AuditOutcome,
        detail: Option<String>,
        timeout: std::time::Duration,
    ) -> Result<(), ApplicationError> {
        let record = AuditRecord::new(
            context.request_id(),
            context.actor().id(),
            PROCESS_DATA_CAPABILITY.name(),
            outcome,
            context.timestamp(),
            detail,
        );
        match tokio::time::timeout(timeout, self.audit.record(record)).await {
            Ok(result) => result.map_err(ApplicationError::AuditUnavailable),
            Err(_) => Err(ApplicationError::AuditUnavailable(PortError::new(
                PortErrorKind::Timeout,
                "mandatory data-processing audit timed out",
            ))),
        }
    }
}

fn stable_processing_id(scope: &str, outer_id: &str, input_digest: &str) -> String {
    let name = format!("aether.data-processing.v1\0{scope}\0{outer_id}\0{input_digest}");
    Uuid::new_v5(&Uuid::NAMESPACE_URL, name.as_bytes()).to_string()
}

fn maximum_task_cell_count(task: &DataProcessingTask) -> Result<usize, ApplicationError> {
    let spec = task.forecast_spec().ok_or_else(|| {
        ApplicationError::InvalidProcessingConfiguration(
            "task has no typed execution specification".to_string(),
        )
    })?;
    let history_features = task
        .features()
        .iter()
        .filter(|feature| feature.role() == FeatureRole::History)
        .count();
    let future_features = task
        .features()
        .iter()
        .filter(|feature| feature.role() == FeatureRole::FutureCovariate)
        .count();
    let static_features = task
        .features()
        .iter()
        .filter(|feature| feature.role() == FeatureRole::Static)
        .count();
    history_features
        .checked_mul(spec.history_steps())
        .and_then(|count| {
            future_features
                .checked_mul(spec.max_horizon_steps())
                .and_then(|future| count.checked_add(future))
        })
        .and_then(|count| count.checked_add(static_features))
        .ok_or_else(|| {
            ApplicationError::InvalidProcessingConfiguration(
                "task frame cell limit overflowed".to_string(),
            )
        })
}

fn frame_cell_count(frame: &ProcessingFrame) -> Result<usize, ApplicationError> {
    let history = frame
        .history()
        .series()
        .iter()
        .try_fold(0usize, |count, series| count.checked_add(series.len()));
    let future = if let Some(segment) = frame.future_covariates() {
        segment
            .series()
            .iter()
            .try_fold(0usize, |count, series| count.checked_add(series.len()))
    } else {
        Some(0)
    };
    history
        .and_then(|history| future.and_then(|future| future.checked_add(history)))
        .and_then(|count| count.checked_add(frame.static_features().len()))
        .ok_or(ApplicationError::InvalidProcessingRequest(
            aether_domain::DomainError::InvalidProcessingWindow,
        ))
}

fn validate_commissioned_binding(
    task: &DataProcessingTask,
    binding: &DataProcessingBinding,
) -> Result<(), ApplicationError> {
    let expected_static: Vec<_> = task
        .features()
        .iter()
        .filter(|feature| feature.role() == FeatureRole::Static)
        .collect();
    if expected_static.len() != binding.static_features().len()
        || expected_static.iter().any(|definition| {
            !binding
                .static_features()
                .iter()
                .any(|feature| feature.definition() == *definition)
        })
    {
        return Err(ApplicationError::InvalidProcessingConfiguration(
            "commissioned static features do not match the task".to_string(),
        ));
    }

    if binding.point_features().iter().any(|point| {
        point.address().kind().is_writable()
            || !task.features().iter().any(|feature| {
                feature.name() == point.feature()
                    && feature.role() == FeatureRole::History
                    && feature.value_type() == FeatureValueType::Number
            })
    }) {
        return Err(ApplicationError::InvalidProcessingConfiguration(
            "point mappings must resolve numeric history features to read-only addresses"
                .to_string(),
        ));
    }
    let specification = task.forecast_spec().ok_or_else(|| {
        ApplicationError::InvalidProcessingConfiguration(
            "task has no typed execution specification".to_string(),
        )
    })?;
    if binding.point_features().iter().any(|point| {
        point.live_tail()
            && specification.history_aggregation_for(point.feature()) != HistoryAggregation::Last
    }) {
        return Err(ApplicationError::InvalidProcessingConfiguration(
            "instantaneous live-tail replacement requires last-value history aggregation"
                .to_string(),
        ));
    }
    let target = specification.target().name();
    if !binding
        .point_features()
        .iter()
        .any(|point| point.feature() == target)
    {
        return Err(ApplicationError::InvalidProcessingConfiguration(
            "the forecast target must resolve to a commissioned read-only point".to_string(),
        ));
    }
    Ok(())
}

fn forecast_options(options: &ProcessingOptions) -> &ForecastOptions {
    match options {
        ProcessingOptions::Forecast(options) => options,
    }
}

fn regular_timestamps(
    start: TimestampMs,
    cadence_ms: u64,
    count: usize,
) -> Result<Vec<TimestampMs>, ApplicationError> {
    (0..count)
        .map(|index| {
            cadence_ms
                .checked_mul(u64::try_from(index).map_err(|_| {
                    ApplicationError::InvalidProcessingRequest(
                        aether_domain::DomainError::InvalidProcessingWindow,
                    )
                })?)
                .and_then(|offset| start.get().checked_add(offset))
                .map(TimestampMs::new)
                .ok_or({
                    ApplicationError::InvalidProcessingRequest(
                        aether_domain::DomainError::InvalidProcessingWindow,
                    )
                })
        })
        .collect()
}

fn validate_segment(
    segment: &Segment,
    expected_features: &[FeatureDefinition],
    expected_timestamps: &[TimestampMs],
) -> Result<(), ApplicationError> {
    if segment.timestamps() != expected_timestamps
        || segment.series().len() != expected_features.len()
        || expected_features.iter().any(|definition| {
            !segment
                .series()
                .iter()
                .any(|series| series.definition() == definition)
        })
    {
        return Err(ApplicationError::InputQualityRejected(
            aether_domain::DomainError::InvalidProcessingWindow,
        ));
    }
    Ok(())
}

fn validate_feature_constraints(segment: &Segment) -> Result<(), ApplicationError> {
    if segment.series().iter().any(|series| {
        series
            .definition()
            .numeric_constraints()
            .is_some_and(|constraints| {
                series.values().iter().any(|value| {
                    !value.is_missing()
                        && value
                            .as_number()
                            .is_none_or(|number| !constraints.accepts(number))
                })
            })
    }) {
        return Err(ApplicationError::InputQualityRejected(
            aether_domain::DomainError::InvalidFrameQuality,
        ));
    }
    Ok(())
}

fn validate_segment_provenance(
    provenance: &[SourceProvenance],
    expected_features: &[FeatureDefinition],
    segment: SegmentKind,
) -> Result<(), ApplicationError> {
    let segment_sources: Vec<_> = provenance
        .iter()
        .filter(|source| source.segment() == segment)
        .collect();
    if segment_sources.len() != expected_features.len()
        || expected_features.iter().any(|feature| {
            segment_sources
                .iter()
                .filter(|source| source.feature() == feature.name())
                .count()
                != 1
        })
    {
        return Err(ApplicationError::InputQualityRejected(
            aether_domain::DomainError::InvalidFrameQuality,
        ));
    }
    Ok(())
}

fn frame_cell_counts(
    history: &Segment,
    future: Option<&Segment>,
    static_features: &[StaticFeature],
) -> (usize, usize, usize) {
    let mut missing = 0usize;
    let mut substituted = 0usize;
    let mut cells = 0usize;
    for segment in [Some(history), future].into_iter().flatten() {
        for series in segment.series() {
            cells = cells.saturating_add(series.len());
            missing = missing.saturating_add(
                series
                    .quality()
                    .iter()
                    .filter(|quality| **quality == SampleQuality::Missing)
                    .count(),
            );
            substituted = substituted.saturating_add(
                series
                    .quality()
                    .iter()
                    .filter(|quality| **quality == SampleQuality::Substituted)
                    .count(),
            );
        }
    }
    cells = cells.saturating_add(static_features.len());
    missing = missing.saturating_add(
        static_features
            .iter()
            .filter(|feature| feature.quality() == SampleQuality::Missing)
            .count(),
    );
    substituted = substituted.saturating_add(
        static_features
            .iter()
            .filter(|feature| feature.quality() == SampleQuality::Substituted)
            .count(),
    );
    (missing, substituted, cells)
}

fn validate_processor_result(
    route: &DataProcessingRoute,
    request: &DataProcessingRequest,
    result: &ProcessingResult,
) -> Result<(), ApplicationError> {
    encode_result(result).map_err(|_| {
        ApplicationError::InvalidProcessorResult(
            "processor result violates the versioned wire contract".to_string(),
        )
    })?;
    let descriptor = route.processor.descriptor();
    if result.request_id() != request.request_id()
        || result.task() != request.task()
        || result.binding() != request.binding()
        || result.input_digest() != request.input_digest()
        || result.input_watermark() != request.frame().quality().input_watermark()
        || result.processor().id() != descriptor.id()
        || result.processor().version() != descriptor.version()
        || result.processor().contract() != request.processor_contract()
        || result.produced_at() < request.submitted_at()
        || result.produced_at() > request.deadline()
    {
        return Err(ApplicationError::InvalidProcessorResult(
            "processor correlation or provenance does not match the request".to_string(),
        ));
    }
    if let Some(selector) = request.artifact_selector()
        && let Some(artifact) = result.artifact()
        && (artifact.kind() != selector.kind()
            || artifact.family() != selector.family()
            || selector
                .version()
                .is_some_and(|version| version != artifact.version())
            || selector
                .digest()
                .is_some_and(|digest| digest != artifact.digest()))
    {
        return Err(ApplicationError::InvalidProcessorResult(
            "artifact provenance does not match the selected policy".to_string(),
        ));
    }
    if result.status() == ProcessingStatus::Produced
        && request.artifact_selector().is_some()
        && result.artifact().is_none()
    {
        return Err(ApplicationError::InvalidProcessorResult(
            "selected artifact provenance is missing".to_string(),
        ));
    }
    if let Some(expiry) = result.expires_at() {
        let max_age = route
            .task
            .forecast_spec()
            .map(|spec| spec.max_output_age_ms())
            .unwrap_or(0);
        if expiry.get().saturating_sub(result.produced_at().get()) > max_age {
            return Err(ApplicationError::InvalidProcessorResult(
                "processor expiry exceeds the task policy".to_string(),
            ));
        }
    }
    if result.status() == ProcessingStatus::Unavailable {
        return Ok(());
    }
    if result.status() == ProcessingStatus::Fallback {
        let spec = route.task.forecast_spec().ok_or_else(|| {
            ApplicationError::InvalidProcessorResult("task has no forecast policy".to_string())
        })?;
        let fallback = result.fallback().ok_or_else(|| {
            ApplicationError::InvalidProcessorResult("fallback metadata is missing".to_string())
        })?;
        let allowed = spec
            .allowed_fallbacks()
            .iter()
            .any(|item| item == fallback.strategy());
        if !allowed {
            return Err(ApplicationError::InvalidProcessorResult(
                "processor used an undeclared fallback".to_string(),
            ));
        }
        let history = request.frame().history();
        let history_cut = history.timestamps().last().copied().ok_or_else(|| {
            ApplicationError::InvalidProcessorResult(
                "fallback request history is empty".to_string(),
            )
        })?;
        if fallback.based_on_data_through() > history_cut {
            return Err(ApplicationError::InvalidProcessorResult(
                "fallback provenance advances beyond the historical data used".to_string(),
            ));
        }
        let source_series = history
            .series()
            .iter()
            .find(|series| series.definition().name() == fallback.source_feature())
            .ok_or_else(|| {
                ApplicationError::InvalidProcessorResult(
                    "fallback source is not present in the historical frame".to_string(),
                )
            })?;
        let source_index = source_series
            .values()
            .iter()
            .zip(source_series.quality())
            .rposition(|(value, quality)| *quality != SampleQuality::Missing && !value.is_missing())
            .ok_or_else(|| {
                ApplicationError::InvalidProcessorResult(
                    "fallback source has no usable historical observation".to_string(),
                )
            })?;
        if history.timestamps()[source_index] != fallback.based_on_data_through() {
            return Err(ApplicationError::InvalidProcessorResult(
                "fallback must use the latest usable source observation".to_string(),
            ));
        }
        let policy = spec.fallback_policy(fallback.strategy()).ok_or_else(|| {
            ApplicationError::InvalidProcessorResult(
                "fallback has no complete task-owned acceptance policy".to_string(),
            )
        })?;
        if fallback.strategy_version() != policy.version()
            || fallback.source_feature() != policy.source_feature()
        {
            return Err(ApplicationError::InvalidProcessorResult(
                "fallback provenance does not match the task policy".to_string(),
            ));
        }
        let expiry = result.expires_at().ok_or_else(|| {
            ApplicationError::InvalidProcessorResult("fallback expiry is missing".to_string())
        })?;
        if expiry.get().saturating_sub(result.produced_at().get()) > policy.max_output_age_ms() {
            return Err(ApplicationError::InvalidProcessorResult(
                "fallback expiry exceeds the fallback policy".to_string(),
            ));
        }
        if fallback.strategy() == "persistence" {
            let source_value = source_series.values()[source_index]
                .as_number()
                .ok_or_else(|| {
                    ApplicationError::InvalidProcessorResult(
                        "persistence fallback source is not numeric".to_string(),
                    )
                })?;
            let output = match result.output() {
                Some(ProcessingOutput::Forecast(output)) => output,
                None => {
                    return Err(ApplicationError::InvalidProcessorResult(
                        "persistence fallback has no forecast output".to_string(),
                    ));
                },
            };
            if output.points().iter().any(|point| {
                point.value().to_bits() != source_value.to_bits()
                    || point
                        .quantiles()
                        .iter()
                        .any(|quantile| quantile.value().to_bits() != source_value.to_bits())
            }) {
                return Err(ApplicationError::InvalidProcessorResult(
                    "persistence fallback output differs from its declared source".to_string(),
                ));
            }
        }
    }

    let output = match result.output() {
        Some(ProcessingOutput::Forecast(output)) => output,
        None => {
            return Err(ApplicationError::InvalidProcessorResult(
                "usable result has no typed output".to_string(),
            ));
        },
    };
    let spec = route.task.forecast_spec().ok_or_else(|| {
        ApplicationError::InvalidProcessorResult("task has no forecast policy".to_string())
    })?;
    let options = forecast_options(request.options());
    if output.target() != spec.target().name()
        || output.unit() != spec.target().unit()
        || output.sign_convention() != spec.target().sign_convention()
        || output.cadence_ms() != spec.cadence_ms()
        || output.points().len() != options.horizon_steps()
    {
        return Err(ApplicationError::InvalidProcessorResult(
            "forecast shape or semantics do not match the task".to_string(),
        ));
    }
    let generated_timestamps;
    let expected_timestamps = if let Some(future) = request.frame().future_covariates() {
        future.timestamps()
    } else {
        let start = request
            .frame()
            .as_of()
            .get()
            .checked_add(spec.cadence_ms())
            .map(TimestampMs::new)
            .ok_or_else(|| {
                ApplicationError::InvalidProcessorResult(
                    "forecast time axis overflowed".to_string(),
                )
            })?;
        generated_timestamps = regular_timestamps(
            start,
            spec.cadence_ms(),
            options.horizon_steps(),
        )
        .map_err(|_| {
            ApplicationError::InvalidProcessorResult("forecast time axis is invalid".to_string())
        })?;
        &generated_timestamps
    };
    if output.points().len() != expected_timestamps.len()
        || output
            .points()
            .iter()
            .zip(expected_timestamps)
            .any(|(point, expected)| point.timestamp() != *expected)
        || output.points().iter().any(|point| {
            point.quantiles().len() != options.quantiles().len()
                || point
                    .quantiles()
                    .iter()
                    .zip(options.quantiles())
                    .any(|(actual, expected)| actual.probability() != *expected)
        })
    {
        return Err(ApplicationError::InvalidProcessorResult(
            "forecast time axis or quantiles do not match the request".to_string(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use aether_domain::{
        FeatureDefinition, FeatureRole, FeatureValue, NumericFeatureConstraints, SampleQuality,
        Segment, Series, TimestampMs, maximum_observation_gap,
    };

    #[test]
    fn observation_gap_uses_usable_cells_instead_of_the_regular_label_grid() {
        let definition = FeatureDefinition::numeric("load", FeatureRole::History, "kW")
            .expect("feature is valid");
        let series = Series::new(
            definition,
            vec![
                FeatureValue::number(1.0).expect("value is valid"),
                FeatureValue::missing(),
                FeatureValue::number(3.0).expect("value is valid"),
            ],
            vec![
                SampleQuality::Good,
                SampleQuality::Missing,
                SampleQuality::Good,
            ],
        )
        .expect("series is valid");
        let segment = Segment::new(
            vec![
                TimestampMs::new(1_000),
                TimestampMs::new(2_000),
                TimestampMs::new(3_000),
            ],
            vec![series],
        )
        .expect("segment is valid");

        assert_eq!(maximum_observation_gap(&segment, 1_000), 2_000);
    }

    #[test]
    fn observation_gap_includes_leading_and_trailing_window_boundaries() {
        let definition = FeatureDefinition::numeric("load", FeatureRole::History, "kW")
            .expect("feature is valid");
        let segment = Segment::new(
            vec![
                TimestampMs::new(1_000),
                TimestampMs::new(2_000),
                TimestampMs::new(3_000),
                TimestampMs::new(4_000),
            ],
            vec![
                Series::new(
                    definition,
                    vec![
                        FeatureValue::missing(),
                        FeatureValue::number(2.0).expect("value is valid"),
                        FeatureValue::missing(),
                        FeatureValue::missing(),
                    ],
                    vec![
                        SampleQuality::Missing,
                        SampleQuality::Good,
                        SampleQuality::Missing,
                        SampleQuality::Missing,
                    ],
                )
                .expect("series is valid"),
                Series::new(
                    FeatureDefinition::numeric("ambient", FeatureRole::History, "Cel")
                        .expect("feature is valid"),
                    vec![FeatureValue::missing(); 4],
                    vec![SampleQuality::Missing; 4],
                )
                .expect("all-missing series is valid"),
            ],
        )
        .expect("segment is valid");

        assert_eq!(maximum_observation_gap(&segment, 1_000), 4_000);
    }

    #[test]
    fn task_owned_numeric_constraints_reject_out_of_range_source_values() {
        let definition = FeatureDefinition::numeric("humidity", FeatureRole::History, "%")
            .expect("feature is valid")
            .with_numeric_constraints(
                NumericFeatureConstraints::new(Some(0.0), Some(100.0), false)
                    .expect("limits are valid"),
            )
            .expect("constrained feature is valid");
        let series = Series::new(
            definition,
            vec![FeatureValue::number(101.0).expect("value is finite")],
            vec![SampleQuality::Good],
        );

        assert!(series.is_err());
    }
}
