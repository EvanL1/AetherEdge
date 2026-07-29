//! Static protocol-adapter composition for the IO runtime.
//!
//! A protocol becomes available only when its Rust factory is registered by a
//! composition root and the binary is rebuilt. The registry performs no
//! dynamic loading and protocol runtimes never receive SHM or SQLite handles.

use std::collections::BTreeMap;
use std::sync::Arc;

use crate::core::config::{ChannelConfig, RuntimeChannelConfig};
use crate::error::{IoError, Result};
use crate::protocols::gateway::ChannelRuntime;

/// A protocol runtime plus the manager-owned scheduling policy selected by its
/// statically linked factory.
pub struct BuiltProtocolRuntime {
    pub(crate) runtime: Box<dyn ChannelRuntime>,
    pub(crate) poll_interval_ms: u64,
}

impl BuiltProtocolRuntime {
    pub fn new(runtime: Box<dyn ChannelRuntime>, poll_interval_ms: u64) -> Self {
        Self {
            runtime,
            poll_interval_ms,
        }
    }
}

/// Factory boundary implemented by every statically linked physical protocol.
///
/// Implementations own protocol parameter and point-mapping interpretation.
/// The channel manager owns lifecycle, command governance, diagnostics wiring,
/// and writes to the authoritative SHM data plane.
pub trait ProtocolAdapterFactory: Send + Sync + 'static {
    /// Stable routing key stored in channel desired state.
    fn protocol_id(&self) -> &'static str;

    /// Validate protocol-owned channel parameters before desired state commits.
    fn validate(&self, config: &ChannelConfig) -> Result<()>;

    /// Build one protocol runtime from a read-only configuration generation.
    fn build(&self, config: &RuntimeChannelConfig) -> Result<BuiltProtocolRuntime>;
}

/// Immutable registry of factories selected at compile time by a composition root.
pub struct ProtocolRegistry {
    factories: BTreeMap<&'static str, Arc<dyn ProtocolAdapterFactory>>,
}

impl ProtocolRegistry {
    /// Build a registry and reject duplicate adapter ownership.
    pub fn try_new(
        factories: impl IntoIterator<Item = Arc<dyn ProtocolAdapterFactory>>,
    ) -> Result<Self> {
        let mut registered = BTreeMap::new();
        for factory in factories {
            let protocol_id = factory.protocol_id();
            if protocol_id.trim().is_empty()
                || crate::utils::normalize_protocol_name(protocol_id).as_ref() != protocol_id
            {
                return Err(IoError::config(
                    "protocol adapter identifiers must be non-empty and canonical",
                ));
            }
            if registered.insert(protocol_id, factory).is_some() {
                return Err(IoError::config(format!(
                    "duplicate protocol adapter registration: {protocol_id}"
                )));
            }
        }
        Ok(Self {
            factories: registered,
        })
    }

    /// Registered protocol identifiers in deterministic order.
    pub fn protocol_ids(&self) -> Vec<&'static str> {
        self.factories.keys().copied().collect()
    }

    pub(crate) fn validate(&self, config: &ChannelConfig) -> Result<()> {
        self.factory(config.protocol())?.validate(config)
    }

    pub(crate) fn build(&self, config: &RuntimeChannelConfig) -> Result<BuiltProtocolRuntime> {
        self.factory(config.protocol())?.build(config)
    }

    fn factory(&self, requested: &str) -> Result<&Arc<dyn ProtocolAdapterFactory>> {
        let protocol_id = crate::utils::normalize_protocol_name(requested);
        self.factories.get(protocol_id.as_ref()).ok_or_else(|| {
            IoError::config(format!(
                "protocol '{protocol_id}' is not statically linked into this IO binary"
            ))
        })
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::core::config::{ChannelCore, ChannelLoggingConfig};

    struct StubFactory(&'static str);

    impl ProtocolAdapterFactory for StubFactory {
        fn protocol_id(&self) -> &'static str {
            self.0
        }

        fn validate(&self, _config: &ChannelConfig) -> Result<()> {
            Ok(())
        }

        fn build(&self, _config: &RuntimeChannelConfig) -> Result<BuiltProtocolRuntime> {
            Err(IoError::config("stub runtime is not buildable"))
        }
    }

    fn config(protocol: &str) -> ChannelConfig {
        ChannelConfig {
            core: ChannelCore {
                id: 1,
                name: "registry-test".to_owned(),
                description: None,
                protocol: protocol.to_owned(),
                enabled: true,
            },
            parameters: HashMap::new(),
            logging: ChannelLoggingConfig::default(),
        }
    }

    #[test]
    fn registration_is_deterministic_and_rejects_duplicate_ownership() {
        let registry = ProtocolRegistry::try_new([
            Arc::new(StubFactory("zeta")) as Arc<dyn ProtocolAdapterFactory>,
            Arc::new(StubFactory("alpha")) as Arc<dyn ProtocolAdapterFactory>,
        ])
        .expect("unique static protocol factories");
        assert_eq!(registry.protocol_ids(), vec!["alpha", "zeta"]);
        assert!(registry.validate(&config("alpha")).is_ok());
        assert!(registry.validate(&config("missing")).is_err());

        assert!(
            ProtocolRegistry::try_new([
                Arc::new(StubFactory("duplicate")) as Arc<dyn ProtocolAdapterFactory>,
                Arc::new(StubFactory("duplicate")) as Arc<dyn ProtocolAdapterFactory>,
            ])
            .is_err()
        );
        assert!(
            ProtocolRegistry::try_new([
                Arc::new(StubFactory("Not Canonical")) as Arc<dyn ProtocolAdapterFactory>
            ])
            .is_err()
        );
    }
}
