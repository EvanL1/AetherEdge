//! Fail-safe AetherEMS composition layered over the industry-neutral kernel.

mod forecast;
mod load_forecast;

pub use forecast::PersistenceForecastProcessor;
pub use load_forecast::LoadForecastContract;

use aether_example_minimal_gateway::MinimalGateway;
use aether_sdk::pack::{PackRuntime, parse_pack_manifest};
use serde::Deserialize;
use thiserror::Error;

const ENERGY_PACK_MANIFEST: &str = include_str!("../../../packs/energy/pack.yaml");
const ENERGY_IO_EXAMPLES: &str = include_str!("../../../packs/energy/examples/config/io/io.yaml");
const ENERGY_AUTOMATION_EXAMPLE: &str =
    include_str!("../../../packs/energy/examples/config/automation/automation.yaml");
const ENERGY_RULE_EXAMPLE: &str =
    include_str!("../../../packs/energy/rules/battery_soc_management.json");
const LOAD_FORECAST_TASK: &str =
    include_str!("../../../packs/energy/data-processing/tasks/site-load-forecast.yaml");
const PV_FORECAST_TASK: &str =
    include_str!("../../../packs/energy/data-processing/tasks/site-pv-forecast.yaml");
const EXAMPLE_PROCESSING_BINDING: &str =
    include_str!("../../../packs/energy/data-processing/bindings/example-site.yaml");
const ENERGY_PACK_ROOT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../packs/energy");

/// Pack metadata exposed by the safe AetherEMS composition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnergyPackSummary {
    pub id: String,
    pub name: String,
    pub status: String,
    pub aether_compatibility: String,
    pub capabilities: Vec<String>,
    pub example_channel_count: usize,
    pub enabled_channel_count: usize,
    pub enabled_rule_count: usize,
    pub data_processing_task_count: usize,
    pub enabled_data_processing_task_count: usize,
    pub enabled_data_processing_binding_count: usize,
    pub example_data_processing_binding_commissioned: bool,
    pub auto_load_instances: bool,
}

/// Errors raised while composing the bundled energy distribution.
#[derive(Debug, Error)]
pub enum EnergyGatewayError {
    #[error("cannot compose the Aether core: {0}")]
    Core(#[from] aether_sdk::BuildError),
    #[error("cannot load the bundled energy pack: {0}")]
    Pack(#[from] aether_sdk::pack::PackError),
    #[error("cannot construct the explicit example runtime manifest: {0}")]
    RuntimeManifest(#[from] aether_runtime_catalog::RuntimeManifestError),
    #[error("cannot parse bundled asset {asset}: {message}")]
    InvalidAsset {
        asset: &'static str,
        message: String,
    },
    #[error("unsafe bundled energy pack: {0}")]
    UnsafePack(String),
}

#[derive(Deserialize)]
struct IoExamples {
    channels: Vec<ChannelExample>,
}

#[derive(Deserialize)]
struct ChannelExample {
    enabled: bool,
}

#[derive(Deserialize)]
struct AutomationExample {
    auto_load_instances: bool,
}

#[derive(Deserialize)]
struct RuleExample {
    enabled: bool,
    commissioned: bool,
}

#[derive(Deserialize)]
struct DataProcessingTaskAsset {
    schema: String,
    id: String,
    revision: u32,
    enabled: bool,
    kind: String,
    processor_contract: String,
    inputs: DataProcessingInputs,
}

#[derive(Deserialize)]
struct DataProcessingInputs {
    history: Vec<DataProcessingFeature>,
    future_covariates: Vec<DataProcessingFeature>,
}

#[derive(Deserialize)]
struct DataProcessingFeature {
    name: String,
}

#[derive(Deserialize)]
struct DataProcessingBindingAsset {
    schema: String,
    revision: u32,
    enabled: bool,
    commissioned: bool,
    task_bindings: Vec<DataProcessingTaskBinding>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DataProcessingTaskBinding {
    task_id: String,
    revision: u32,
    enabled: bool,
}

/// Runnable proof that the energy distribution is an opt-in layer over Aether.
pub struct EnergyGateway {
    core: MinimalGateway,
    summary: EnergyPackSummary,
    load_forecast_contract: LoadForecastContract,
}

/// Loads and validates the bundled site-load task without commissioning it.
pub fn bundled_load_forecast_contract() -> Result<LoadForecastContract, EnergyGatewayError> {
    LoadForecastContract::from_yaml(LOAD_FORECAST_TASK)
}

impl EnergyGateway {
    /// Compose the industry-neutral gateway and inspect the bundled energy pack.
    pub fn bundled() -> Result<Self, EnergyGatewayError> {
        Self::from_assets(
            ENERGY_PACK_MANIFEST,
            ENERGY_IO_EXAMPLES,
            ENERGY_AUTOMATION_EXAMPLE,
            ENERGY_RULE_EXAMPLE,
            LOAD_FORECAST_TASK,
            PV_FORECAST_TASK,
            EXAMPLE_PROCESSING_BINDING,
        )
    }

    fn from_assets(
        manifest_contents: &str,
        io_contents: &str,
        automation_contents: &str,
        rule_contents: &str,
        load_task_contents: &str,
        pv_task_contents: &str,
        processing_binding_contents: &str,
    ) -> Result<Self, EnergyGatewayError> {
        let pack_runtime = energy_pack_runtime()?;
        let manifest = parse_pack_manifest(manifest_contents, ENERGY_PACK_ROOT, &pack_runtime)?;
        let io: IoExamples = parse_yaml("packs/energy/examples/config/io/io.yaml", io_contents)?;
        let automation: AutomationExample = parse_yaml(
            "packs/energy/examples/config/automation/automation.yaml",
            automation_contents,
        )?;
        let rule: RuleExample = serde_json::from_str(rule_contents).map_err(|error| {
            EnergyGatewayError::InvalidAsset {
                asset: "packs/energy/rules/battery_soc_management.json",
                message: error.to_string(),
            }
        })?;
        let load_task: DataProcessingTaskAsset = parse_yaml(
            "packs/energy/data-processing/tasks/site-load-forecast.yaml",
            load_task_contents,
        )?;
        let pv_task: DataProcessingTaskAsset = parse_yaml(
            "packs/energy/data-processing/tasks/site-pv-forecast.yaml",
            pv_task_contents,
        )?;
        let load_forecast_contract = LoadForecastContract::from_yaml(load_task_contents)?;
        reject_forbidden_binding_fields(processing_binding_contents)?;
        let processing_binding: DataProcessingBindingAsset = parse_yaml(
            "packs/energy/data-processing/bindings/example-site.yaml",
            processing_binding_contents,
        )?;

        let enabled_channel_count = io.channels.iter().filter(|channel| channel.enabled).count();
        let enabled_rule_count = usize::from(rule.enabled);
        if enabled_channel_count > 0
            || enabled_rule_count > 0
            || rule.commissioned
            || automation.auto_load_instances
        {
            return Err(EnergyGatewayError::UnsafePack(
                "bundled examples must require explicit commissioning".to_string(),
            ));
        }

        validate_data_processing_assets(
            manifest
                .capability_ids("data_processing_tasks")
                .ok_or_else(|| {
                    EnergyGatewayError::UnsafePack(
                        "pack manifest has no data-processing task capabilities".to_string(),
                    )
                })?,
            [&load_task, &pv_task],
            &processing_binding,
        )?;

        let enabled_data_processing_task_count = [&load_task, &pv_task]
            .iter()
            .filter(|task| task.enabled)
            .count();
        let enabled_data_processing_binding_count = processing_binding
            .task_bindings
            .iter()
            .filter(|binding| binding.enabled)
            .count();

        let summary = EnergyPackSummary {
            id: manifest.id().to_string(),
            name: manifest.name().to_string(),
            status: manifest.status().to_string(),
            aether_compatibility: manifest.aether_requirement().to_string(),
            capabilities: manifest
                .capability_ids("models")
                .ok_or_else(|| {
                    EnergyGatewayError::UnsafePack(
                        "pack manifest has no model capabilities".to_string(),
                    )
                })?
                .to_vec(),
            example_channel_count: io.channels.len(),
            enabled_channel_count,
            enabled_rule_count,
            data_processing_task_count: 2,
            enabled_data_processing_task_count,
            enabled_data_processing_binding_count,
            example_data_processing_binding_commissioned: processing_binding.commissioned,
            auto_load_instances: automation.auto_load_instances,
        };

        Ok(Self {
            core: MinimalGateway::new()?,
            summary,
            load_forecast_contract,
        })
    }

    /// Return the shared command/query API used by human and AI interfaces.
    #[must_use]
    pub const fn application(&self) -> &aether_sdk::application::EdgeApplication {
        self.core.application()
    }

    /// Return validated energy-pack metadata without commissioning devices.
    #[must_use]
    pub const fn pack_summary(&self) -> &EnergyPackSummary {
        &self.summary
    }

    /// Returns the validated, disabled-by-default bundled load-forecast contract.
    #[must_use]
    pub const fn load_forecast_contract(&self) -> &LoadForecastContract {
        &self.load_forecast_contract
    }
}

fn energy_pack_runtime() -> Result<PackRuntime, aether_runtime_catalog::RuntimeManifestError> {
    aether_runtime_catalog::KernelRuntimeManifest::from_io_features(
        env!("CARGO_PKG_VERSION"),
        "aarch64-unknown-linux-musl",
        ["can", "gpio", "http", "modbus", "mqtt"],
    )?
    .pack_runtime()
}

fn reject_forbidden_binding_fields(contents: &str) -> Result<(), EnergyGatewayError> {
    let value: serde_yml::Value = parse_yaml(
        "packs/energy/data-processing/bindings/example-site.yaml",
        contents,
    )?;
    scan_binding_value(&value)
}

fn scan_binding_value(value: &serde_yml::Value) -> Result<(), EnergyGatewayError> {
    match value {
        serde_yml::Value::Mapping(mapping) => {
            for (key, nested) in mapping {
                let serde_yml::Value::String(key) = key else {
                    return Err(EnergyGatewayError::UnsafePack(
                        "data-processing binding keys must be strings".to_string(),
                    ));
                };
                let normalized = key.to_ascii_lowercase().replace('-', "_");
                if ["endpoint", "processor", "artifact", "credential"]
                    .iter()
                    .any(|forbidden| normalized.contains(forbidden))
                {
                    return Err(EnergyGatewayError::UnsafePack(
                        "bundled bindings cannot select processor routes, artifacts, endpoints, or credentials"
                            .to_string(),
                    ));
                }
                scan_binding_value(nested)?;
            }
            Ok(())
        },
        serde_yml::Value::Sequence(sequence) => {
            for nested in sequence {
                scan_binding_value(nested)?;
            }
            Ok(())
        },
        serde_yml::Value::Tagged(_) => Err(EnergyGatewayError::UnsafePack(
            "tagged YAML values are forbidden in bundled bindings".to_string(),
        )),
        serde_yml::Value::Null
        | serde_yml::Value::Bool(_)
        | serde_yml::Value::Number(_)
        | serde_yml::Value::String(_) => Ok(()),
    }
}

fn validate_data_processing_assets(
    declared_task_ids: &[String],
    tasks: [&DataProcessingTaskAsset; 2],
    binding: &DataProcessingBindingAsset,
) -> Result<(), EnergyGatewayError> {
    if binding.schema != "aether.data-processing-binding.v1" || binding.revision == 0 {
        return Err(EnergyGatewayError::UnsafePack(
            "data-processing binding schema or revision is invalid".to_string(),
        ));
    }
    if binding.enabled
        || binding.commissioned
        || binding.task_bindings.iter().any(|item| item.enabled)
    {
        return Err(EnergyGatewayError::UnsafePack(
            "bundled data-processing assets must require explicit commissioning".to_string(),
        ));
    }

    for task in tasks {
        if task.schema != "aether.data-processing-task.v1"
            || task.revision == 0
            || task.kind != "forecast"
            || task.processor_contract != "aether.data-processing.forecast.v1"
            || task.enabled
        {
            return Err(EnergyGatewayError::UnsafePack(format!(
                "data-processing task {} is invalid or unexpectedly enabled",
                task.id
            )));
        }
        if !declared_task_ids
            .iter()
            .any(|declared| declared == &task.id)
        {
            return Err(EnergyGatewayError::UnsafePack(format!(
                "data-processing task {} is absent from the pack manifest",
                task.id
            )));
        }
        let Some(bound) = binding
            .task_bindings
            .iter()
            .find(|candidate| candidate.task_id == task.id)
        else {
            return Err(EnergyGatewayError::UnsafePack(format!(
                "data-processing task {} has no example binding",
                task.id
            )));
        };
        if bound.revision != task.revision {
            return Err(EnergyGatewayError::UnsafePack(format!(
                "data-processing task {} and binding revisions differ",
                task.id
            )));
        }
    }

    let load = tasks[0];
    let pv = tasks[1];
    if load.inputs.history.len() != 5
        || load.inputs.future_covariates.len() != 4
        || pv.inputs.history.len() != 20
        || pv.inputs.future_covariates.len() != 19
    {
        return Err(EnergyGatewayError::UnsafePack(
            "forecast task feature counts do not match the processor contracts".to_string(),
        ));
    }
    let pv_history_weather: Vec<&str> = pv
        .inputs
        .history
        .iter()
        .filter(|feature| feature.name != "pv")
        .map(|feature| feature.name.as_str())
        .collect();
    let pv_future: Vec<&str> = pv
        .inputs
        .future_covariates
        .iter()
        .map(|feature| feature.name.as_str())
        .collect();
    if pv_history_weather != pv_future {
        return Err(EnergyGatewayError::UnsafePack(
            "PV historical and future weather feature order differs".to_string(),
        ));
    }

    Ok(())
}

fn parse_yaml<T>(asset: &'static str, contents: &str) -> Result<T, EnergyGatewayError>
where
    T: for<'de> Deserialize<'de>,
{
    serde_yml::from_str(contents).map_err(|error| EnergyGatewayError::InvalidAsset {
        asset,
        message: error.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unsupported_pack_schema_fails_closed() {
        let manifest = ENERGY_PACK_MANIFEST.replacen("schema_version: 1", "schema_version: 2", 1);

        let result = EnergyGateway::from_assets(
            &manifest,
            ENERGY_IO_EXAMPLES,
            ENERGY_AUTOMATION_EXAMPLE,
            ENERGY_RULE_EXAMPLE,
            LOAD_FORECAST_TASK,
            PV_FORECAST_TASK,
            EXAMPLE_PROCESSING_BINDING,
        );

        assert!(matches!(result, Err(EnergyGatewayError::Pack(_))));
    }

    #[test]
    fn unexpectedly_enabled_device_channel_fails_closed() {
        let io = ENERGY_IO_EXAMPLES.replacen("enabled: false", "enabled: true", 1);

        let result = EnergyGateway::from_assets(
            ENERGY_PACK_MANIFEST,
            &io,
            ENERGY_AUTOMATION_EXAMPLE,
            ENERGY_RULE_EXAMPLE,
            LOAD_FORECAST_TASK,
            PV_FORECAST_TASK,
            EXAMPLE_PROCESSING_BINDING,
        );

        assert!(matches!(result, Err(EnergyGatewayError::UnsafePack(_))));
    }

    #[test]
    fn rule_template_must_remain_disabled_and_uncommissioned() {
        for rule in [
            ENERGY_RULE_EXAMPLE.replacen("\"enabled\": false", "\"enabled\": true", 1),
            ENERGY_RULE_EXAMPLE.replacen("\"commissioned\": false", "\"commissioned\": true", 1),
        ] {
            let result = EnergyGateway::from_assets(
                ENERGY_PACK_MANIFEST,
                ENERGY_IO_EXAMPLES,
                ENERGY_AUTOMATION_EXAMPLE,
                &rule,
                LOAD_FORECAST_TASK,
                PV_FORECAST_TASK,
                EXAMPLE_PROCESSING_BINDING,
            );

            assert!(matches!(result, Err(EnergyGatewayError::UnsafePack(_))));
        }
    }

    #[test]
    fn incompatible_aether_release_fails_closed() {
        let manifest = ENERGY_PACK_MANIFEST.replacen(">=0.5.0,<0.6.0", ">=0.6.0,<0.7.0", 1);

        let result = EnergyGateway::from_assets(
            &manifest,
            ENERGY_IO_EXAMPLES,
            ENERGY_AUTOMATION_EXAMPLE,
            ENERGY_RULE_EXAMPLE,
            LOAD_FORECAST_TASK,
            PV_FORECAST_TASK,
            EXAMPLE_PROCESSING_BINDING,
        );

        assert!(matches!(result, Err(EnergyGatewayError::Pack(_))));
    }

    #[test]
    fn unexpectedly_enabled_data_processing_task_fails_closed() {
        let load_task = LOAD_FORECAST_TASK.replacen("enabled: false", "enabled: true", 1);

        let result = EnergyGateway::from_assets(
            ENERGY_PACK_MANIFEST,
            ENERGY_IO_EXAMPLES,
            ENERGY_AUTOMATION_EXAMPLE,
            ENERGY_RULE_EXAMPLE,
            &load_task,
            PV_FORECAST_TASK,
            EXAMPLE_PROCESSING_BINDING,
        );

        assert!(matches!(result, Err(EnergyGatewayError::UnsafePack(_))));
    }

    #[test]
    fn processor_route_in_pack_binding_is_rejected() {
        let binding = EXAMPLE_PROCESSING_BINDING.replacen(
            "enabled: false\n  - task_id: energy.site-pv-forecast",
            "enabled: false\n    processor_ref: forbidden\n  - task_id: energy.site-pv-forecast",
            1,
        );

        let result = EnergyGateway::from_assets(
            ENERGY_PACK_MANIFEST,
            ENERGY_IO_EXAMPLES,
            ENERGY_AUTOMATION_EXAMPLE,
            ENERGY_RULE_EXAMPLE,
            LOAD_FORECAST_TASK,
            PV_FORECAST_TASK,
            &binding,
        );

        assert!(matches!(result, Err(EnergyGatewayError::UnsafePack(_))));
    }

    #[test]
    fn load_contract_policy_changes_fail_closed() {
        for (original, unsafe_value) in [
            ("cadence_seconds: 900", "cadence_seconds: 300"),
            ("history_steps: 672", "history_steps: 96"),
            ("horizon_steps: 288", "horizon_steps: 289"),
            ("max_missing_ratio: 0.0", "max_missing_ratio: 0.02"),
            ("missing_policy: reject", "missing_policy: fill"),
            (
                "require_covariate_issue_time_not_after_as_of: true",
                "require_covariate_issue_time_not_after_as_of: false",
            ),
            ("live_tail: forbidden", "live_tail: allowed"),
            (
                "correlation_key: input_digest",
                "correlation_key: request_id",
            ),
            ("expires_after_seconds: 3600", "expires_after_seconds: 7200"),
            ("name: persistence", "name: synthetic-zero"),
            (
                "synthetic_zero_baseline: forbidden",
                "synthetic_zero_baseline: allowed",
            ),
            ("remote_egress: denied", "remote_egress: allowed"),
        ] {
            let load_task = LOAD_FORECAST_TASK.replacen(original, unsafe_value, 1);

            let result = EnergyGateway::from_assets(
                ENERGY_PACK_MANIFEST,
                ENERGY_IO_EXAMPLES,
                ENERGY_AUTOMATION_EXAMPLE,
                ENERGY_RULE_EXAMPLE,
                &load_task,
                PV_FORECAST_TASK,
                EXAMPLE_PROCESSING_BINDING,
            );

            assert!(
                matches!(result, Err(EnergyGatewayError::UnsafePack(_))),
                "mutation {original:?} -> {unsafe_value:?} must fail closed"
            );
        }
    }

    #[test]
    fn forbidden_binding_route_and_secret_fields_fail_closed_at_any_depth() {
        for forbidden in [
            "endpoint: https://processor.invalid",
            "processor_ref: forbidden",
            "artifact_ref: forbidden",
            "credential_ref: forbidden",
        ] {
            let binding = EXAMPLE_PROCESSING_BINDING.replacen(
                "description: Synthetic, non-routable binding for data-processing conformance examples.",
                &format!(
                    "description: Synthetic, non-routable binding for data-processing conformance examples.\n{forbidden}"
                ),
                1,
            );

            let result = EnergyGateway::from_assets(
                ENERGY_PACK_MANIFEST,
                ENERGY_IO_EXAMPLES,
                ENERGY_AUTOMATION_EXAMPLE,
                ENERGY_RULE_EXAMPLE,
                LOAD_FORECAST_TASK,
                PV_FORECAST_TASK,
                &binding,
            );

            assert!(
                matches!(result, Err(EnergyGatewayError::UnsafePack(_))),
                "field {forbidden:?} must fail closed"
            );
        }

        let nested_credential = EXAMPLE_PROCESSING_BINDING.replacen(
            "source_ref: example_forecast_weather",
            "source_ref: example_forecast_weather\n    credentials:\n      token: forbidden",
            1,
        );
        let result = EnergyGateway::from_assets(
            ENERGY_PACK_MANIFEST,
            ENERGY_IO_EXAMPLES,
            ENERGY_AUTOMATION_EXAMPLE,
            ENERGY_RULE_EXAMPLE,
            LOAD_FORECAST_TASK,
            PV_FORECAST_TASK,
            &nested_credential,
        );
        assert!(matches!(result, Err(EnergyGatewayError::UnsafePack(_))));
    }
}
