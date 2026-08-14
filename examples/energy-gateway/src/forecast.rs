//! Deterministic, model-free forecast processor used by the local EMS proof.

use aether_sdk::domain::{
    DataProcessingRequest, FallbackInfo, FeatureValue, ForecastOutput, ForecastPoint,
    ProcessingOptions, ProcessingOutput, ProcessingResult, ProcessingStatus, ProcessorProvenance,
    TimestampMs,
};
use aether_sdk::ports::{
    DataBoundary, DataProcessor, DataProcessorDescriptor, PortError, PortErrorKind, PortResult,
    ProcessorHealth,
};
use async_trait::async_trait;

use crate::LoadForecastContract;

const FORECAST_CONTRACT: &str = "aether.data-processing.forecast";
const FALLBACK_TTL_MS: u64 = 1_800_000;

/// Persistence fallback that repeats the last actual target value.
///
/// This processor is intentionally simple: it proves the complete Aether data
/// path with no Python runtime, model file, external database, or network. Its
/// output is always labeled `Fallback`; it never pretends to be model output.
#[derive(Debug)]
pub struct PersistenceForecastProcessor {
    descriptor: DataProcessorDescriptor,
    fallback_ttl_ms: u64,
}

impl PersistenceForecastProcessor {
    /// Creates a bounded local processor descriptor.
    pub fn new(max_frame_samples: usize, max_request_bytes: usize) -> PortResult<Self> {
        Self::with_limits(max_frame_samples, max_request_bytes, FALLBACK_TTL_MS)
    }

    /// Creates a processor whose frame, payload, and fallback lifetime bounds
    /// come from the validated bundled load task.
    pub fn for_load_contract(contract: &LoadForecastContract) -> PortResult<Self> {
        Self::with_limits(
            contract.maximum_frame_samples(),
            contract.max_request_bytes(),
            contract.fallback_max_expires_after_ms(),
        )
    }

    fn with_limits(
        max_frame_samples: usize,
        max_request_bytes: usize,
        fallback_ttl_ms: u64,
    ) -> PortResult<Self> {
        if fallback_ttl_ms == 0 {
            return Err(rejected("fallback lifetime must be positive"));
        }
        Ok(Self {
            descriptor: DataProcessorDescriptor::new(
                "energy.persistence-forecast",
                env!("CARGO_PKG_VERSION"),
                vec![aether_sdk::domain::TaskKind::Forecast],
                vec![FORECAST_CONTRACT.to_string()],
                DataBoundary::Local,
                max_frame_samples,
                max_request_bytes,
            )?,
            fallback_ttl_ms,
        })
    }
}

#[async_trait]
impl DataProcessor for PersistenceForecastProcessor {
    fn descriptor(&self) -> &DataProcessorDescriptor {
        &self.descriptor
    }

    async fn health(&self) -> PortResult<ProcessorHealth> {
        Ok(ProcessorHealth::Healthy)
    }

    async fn process(&self, request: DataProcessingRequest) -> PortResult<ProcessingResult> {
        let ProcessingOptions::Forecast(options) = request.options();
        if !options.quantiles().is_empty() {
            return Err(rejected(
                "persistence fallback does not synthesize forecast quantiles",
            ));
        }
        let (target, unit, sign_convention) = match request.task().id() {
            "energy.site-load-forecast" => ("load", "kW", "positive_consumption"),
            "energy.site-pv-forecast" => ("pv", "kW", "positive_generation"),
            _ => return Err(rejected("persistence processor does not support this task")),
        };
        let target_series = request
            .frame()
            .history()
            .series()
            .iter()
            .find(|series| series.definition().name() == target)
            .ok_or_else(|| rejected("forecast target is absent from the history frame"))?;
        let last_value = target_series
            .values()
            .last()
            .and_then(FeatureValue::as_number)
            .ok_or_else(|| rejected("forecast target has no finite latest value"))?;
        let timestamps = if let Some(future) = request.frame().future_covariates() {
            if future.timestamps().len() != options.horizon_steps() {
                return Err(rejected(
                    "future time axis does not match the requested horizon",
                ));
            }
            future.timestamps().to_vec()
        } else {
            (1..=options.horizon_steps())
                .map(|step| {
                    let offset =
                        request
                            .frame()
                            .cadence_ms()
                            .checked_mul(u64::try_from(step).map_err(|_| {
                                rejected("forecast horizon exceeds timestamp capacity")
                            })?)
                            .ok_or_else(|| rejected("forecast horizon overflows"))?;
                    request
                        .frame()
                        .as_of()
                        .get()
                        .checked_add(offset)
                        .map(TimestampMs::new)
                        .ok_or_else(|| rejected("forecast timestamp overflows"))
                })
                .collect::<PortResult<Vec<_>>>()?
        };
        let points = timestamps
            .into_iter()
            .map(|timestamp| {
                ForecastPoint::new(timestamp, last_value, vec![])
                    .map_err(|error| invalid(error.to_string()))
            })
            .collect::<PortResult<Vec<_>>>()?;
        let output = ForecastOutput::new(
            target,
            unit,
            sign_convention,
            request.frame().cadence_ms(),
            points,
        )
        .map_err(|error| invalid(error.to_string()))?;
        let produced_at = request
            .submitted_at()
            .get()
            .checked_add(1)
            .map(TimestampMs::new)
            .ok_or_else(|| invalid("processor timestamp overflows".to_string()))?;
        if produced_at > request.deadline() {
            return Err(PortError::new(
                PortErrorKind::Timeout,
                "processing deadline elapsed",
            ));
        }
        let expires_at = produced_at
            .get()
            .checked_add(self.fallback_ttl_ms)
            .map(TimestampMs::new)
            .ok_or_else(|| invalid("forecast expiry overflows".to_string()))?;
        let based_on_data_through = request
            .frame()
            .history()
            .timestamps()
            .last()
            .copied()
            .ok_or_else(|| invalid("forecast history is empty".to_string()))?;
        ProcessingResult::new(
            request.request_id(),
            request.task().clone(),
            request.binding().clone(),
            request.input_digest(),
            ProcessingStatus::Fallback,
            ProcessorProvenance::new(
                self.descriptor.id(),
                self.descriptor.version(),
                FORECAST_CONTRACT,
            )
            .map_err(|error| invalid(error.to_string()))?,
            None,
            request.frame().quality().input_watermark(),
            produced_at,
            Some(expires_at),
            Some(ProcessingOutput::Forecast(output)),
            Some(
                FallbackInfo::new(
                    "persistence",
                    "1",
                    "MODEL_NOT_REQUIRED",
                    target,
                    based_on_data_through,
                )
                .map_err(|error| invalid(error.to_string()))?,
            ),
            None,
        )
        .and_then(|result| result.with_warnings(vec!["PERSISTENCE_FALLBACK_USED".to_string()]))
        .map_err(|error| invalid(error.to_string()))
    }
}

fn rejected(message: &'static str) -> PortError {
    PortError::new(PortErrorKind::Rejected, message)
}

fn invalid(message: String) -> PortError {
    PortError::new(PortErrorKind::InvalidData, message)
}
