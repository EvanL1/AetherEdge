//! Fail-safe AetherEMS composition layered over the industry-neutral kernel.

use aether_example_minimal_gateway::MinimalGateway;
use serde::Deserialize;
use thiserror::Error;

const ENERGY_PACK_MANIFEST: &str = include_str!("../../../packs/energy/pack.yaml");
const ENERGY_IO_EXAMPLES: &str = include_str!("../../../packs/energy/examples/config/io/io.yaml");
const ENERGY_AUTOMATION_EXAMPLE: &str =
    include_str!("../../../packs/energy/examples/config/automation/automation.yaml");
const ENERGY_RULE_EXAMPLE: &str = include_str!(
    "../../../packs/energy/examples/config/automation/rules/battery_soc_management.json"
);

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
    pub auto_load_instances: bool,
}

/// Errors raised while composing the bundled energy distribution.
#[derive(Debug, Error)]
pub enum EnergyGatewayError {
    #[error("cannot compose the Aether core: {0}")]
    Core(#[from] aether_sdk::BuildError),
    #[error("cannot parse bundled asset {asset}: {message}")]
    InvalidAsset {
        asset: &'static str,
        message: String,
    },
    #[error("unsafe bundled energy pack: {0}")]
    UnsafePack(String),
}

#[derive(Deserialize)]
struct PackManifest {
    schema_version: u32,
    id: String,
    name: String,
    status: String,
    compatibility: PackCompatibility,
    capabilities: PackCapabilities,
}

#[derive(Deserialize)]
struct PackCompatibility {
    aether: String,
}

#[derive(Deserialize)]
struct PackCapabilities {
    models: Vec<String>,
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
}

/// Runnable proof that the energy distribution is an opt-in layer over Aether.
pub struct EnergyGateway {
    core: MinimalGateway,
    summary: EnergyPackSummary,
}

impl EnergyGateway {
    /// Compose the industry-neutral gateway and inspect the bundled energy pack.
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
        let manifest: PackManifest = parse_yaml("packs/energy/pack.yaml", manifest_contents)?;
        let io: IoExamples = parse_yaml("packs/energy/examples/config/io/io.yaml", io_contents)?;
        let automation: AutomationExample = parse_yaml(
            "packs/energy/examples/config/automation/automation.yaml",
            automation_contents,
        )?;
        let rule: RuleExample = serde_json::from_str(rule_contents).map_err(|error| {
            EnergyGatewayError::InvalidAsset {
                asset: "packs/energy/examples/config/automation/rules/battery_soc_management.json",
                message: error.to_string(),
            }
        })?;

        if manifest.schema_version != 1 {
            return Err(EnergyGatewayError::UnsafePack(format!(
                "unsupported schema version {}",
                manifest.schema_version
            )));
        }
        if !supports_aether_release(&manifest.compatibility.aether, env!("CARGO_PKG_VERSION")) {
            return Err(EnergyGatewayError::UnsafePack(format!(
                "Aether {} does not satisfy {}",
                env!("CARGO_PKG_VERSION"),
                manifest.compatibility.aether
            )));
        }

        let enabled_channel_count = io.channels.iter().filter(|channel| channel.enabled).count();
        let enabled_rule_count = usize::from(rule.enabled);
        if enabled_channel_count > 0 || enabled_rule_count > 0 || automation.auto_load_instances {
            return Err(EnergyGatewayError::UnsafePack(
                "bundled examples must require explicit commissioning".to_string(),
            ));
        }

        let summary = EnergyPackSummary {
            id: manifest.id,
            name: manifest.name,
            status: manifest.status,
            aether_compatibility: manifest.compatibility.aether,
            capabilities: manifest.capabilities.models,
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
}

fn supports_aether_release(requirement: &str, current: &str) -> bool {
    let mut clauses = requirement.split(',');
    let Some(minimum) = clauses.next().and_then(|clause| clause.strip_prefix(">=")) else {
        return false;
    };
    let Some(maximum) = clauses.next().and_then(|clause| clause.strip_prefix('<')) else {
        return false;
    };
    if clauses.next().is_some() {
        return false;
    }

    match (
        parse_release_version(minimum),
        parse_release_version(maximum),
        parse_release_version(current),
    ) {
        (Some(minimum), Some(maximum), Some(current)) => current >= minimum && current < maximum,
        _ => false,
    }
}

fn parse_release_version(version: &str) -> Option<(u64, u64, u64)> {
    let core = version.split_once('-').map_or(version, |(core, _)| core);
    let mut parts = core.split('.');
    let parsed = (
        parts.next()?.parse().ok()?,
        parts.next()?.parse().ok()?,
        parts.next()?.parse().ok()?,
    );
    parts.next().is_none().then_some(parsed)
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
        );

        assert!(matches!(result, Err(EnergyGatewayError::UnsafePack(_))));
    }

    #[test]
    fn unexpectedly_enabled_device_channel_fails_closed() {
        let io = ENERGY_IO_EXAMPLES.replacen("enabled: false", "enabled: true", 1);

        let result = EnergyGateway::from_assets(
            ENERGY_PACK_MANIFEST,
            &io,
            ENERGY_AUTOMATION_EXAMPLE,
            ENERGY_RULE_EXAMPLE,
        );

        assert!(matches!(result, Err(EnergyGatewayError::UnsafePack(_))));
    }

    #[test]
    fn incompatible_aether_release_fails_closed() {
        let manifest = ENERGY_PACK_MANIFEST.replacen(">=0.5.0,<0.6.0", ">=0.6.0,<0.7.0", 1);

        let result = EnergyGateway::from_assets(
            &manifest,
            ENERGY_IO_EXAMPLES,
            ENERGY_AUTOMATION_EXAMPLE,
            ENERGY_RULE_EXAMPLE,
        );

        assert!(matches!(result, Err(EnergyGatewayError::UnsafePack(_))));
    }
}
