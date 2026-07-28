//! Fail-safe AetherEMS compatibility composition over the industry-neutral kernel.

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
const ENERGY_PACK_ROOT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../packs/energy");

/// Pack metadata exposed by the safe compatibility composition.
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
    pub auto_load_instances: bool,
}

/// Errors raised while composing the bundled energy compatibility proof.
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

/// Runnable proof that the energy distribution is an opt-in layer over Aether.
pub struct EnergyGateway {
    core: MinimalGateway,
    summary: EnergyPackSummary,
}

impl EnergyGateway {
    /// Composes the neutral gateway and validates the disabled energy examples.
    pub fn bundled() -> Result<Self, EnergyGatewayError> {
        Self::from_assets(
            ENERGY_PACK_MANIFEST,
            ENERGY_IO_EXAMPLES,
            ENERGY_AUTOMATION_EXAMPLE,
            ENERGY_RULE_EXAMPLE,
        )
    }

    fn from_assets(
        manifest_contents: &str,
        io_contents: &str,
        automation_contents: &str,
        rule_contents: &str,
    ) -> Result<Self, EnergyGatewayError> {
        let manifest =
            parse_pack_manifest(manifest_contents, ENERGY_PACK_ROOT, &energy_pack_runtime()?)?;
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

        let capabilities = manifest
            .capability_ids("models")
            .ok_or_else(|| {
                EnergyGatewayError::UnsafePack(
                    "pack manifest has no model capabilities".to_string(),
                )
            })?
            .to_vec();
        let summary = EnergyPackSummary {
            id: manifest.id().to_string(),
            name: manifest.name().to_string(),
            status: manifest.status().to_string(),
            aether_compatibility: manifest.aether_requirement().to_string(),
            capabilities,
            example_channel_count: io.channels.len(),
            enabled_channel_count,
            enabled_rule_count,
            auto_load_instances: automation.auto_load_instances,
        };

        Ok(Self {
            core: MinimalGateway::new()?,
            summary,
        })
    }

    /// Returns the shared command/query API used by human and AI interfaces.
    #[must_use]
    pub const fn application(&self) -> &aether_sdk::application::EdgeApplication {
        self.core.application()
    }

    /// Returns validated energy-pack metadata without commissioning devices.
    #[must_use]
    pub const fn pack_summary(&self) -> &EnergyPackSummary {
        &self.summary
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
        assert!(matches!(
            EnergyGateway::from_assets(
                &manifest,
                ENERGY_IO_EXAMPLES,
                ENERGY_AUTOMATION_EXAMPLE,
                ENERGY_RULE_EXAMPLE,
            ),
            Err(EnergyGatewayError::Pack(_))
        ));
    }

    #[test]
    fn unexpectedly_enabled_device_channel_fails_closed() {
        let io = ENERGY_IO_EXAMPLES.replacen("enabled: false", "enabled: true", 1);
        assert!(matches!(
            EnergyGateway::from_assets(
                ENERGY_PACK_MANIFEST,
                &io,
                ENERGY_AUTOMATION_EXAMPLE,
                ENERGY_RULE_EXAMPLE,
            ),
            Err(EnergyGatewayError::UnsafePack(_))
        ));
    }

    #[test]
    fn rule_template_must_remain_disabled_and_uncommissioned() {
        for rule in [
            ENERGY_RULE_EXAMPLE.replacen("\"enabled\": false", "\"enabled\": true", 1),
            ENERGY_RULE_EXAMPLE.replacen("\"commissioned\": false", "\"commissioned\": true", 1),
        ] {
            assert!(matches!(
                EnergyGateway::from_assets(
                    ENERGY_PACK_MANIFEST,
                    ENERGY_IO_EXAMPLES,
                    ENERGY_AUTOMATION_EXAMPLE,
                    &rule,
                ),
                Err(EnergyGatewayError::UnsafePack(_))
            ));
        }
    }

    #[test]
    fn incompatible_aether_release_fails_closed() {
        let manifest = ENERGY_PACK_MANIFEST.replacen(">=0.0.1,<0.1.0", ">=0.1.0,<0.2.0", 1);
        assert!(matches!(
            EnergyGateway::from_assets(
                &manifest,
                ENERGY_IO_EXAMPLES,
                ENERGY_AUTOMATION_EXAMPLE,
                ENERGY_RULE_EXAMPLE,
            ),
            Err(EnergyGatewayError::Pack(_))
        ));
    }
}
