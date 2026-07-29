//! Generic channel creation over the statically composed protocol registry.

use std::sync::Arc;

use common::{ValidationLevel, ValidationResult};
use tracing::{debug, info};

use crate::core::channels::channel_entry::ChannelEntry;
use crate::core::channels::channel_manager::ChannelManager;
use crate::core::channels::command_guard::CommandGuard;
use crate::core::config::{ChannelConfig, RuntimeChannelConfig};
use crate::error::{IoError, Result};
use crate::protocols::core::file_logging::{ChannelFileLogHandler, FileLogLevel};
use crate::protocols::core::log_handlers::{CompositeLogHandler, TracingLogHandler};
use crate::protocols::core::logging::{ChannelLogConfig, ChannelLogHandler, LogEventType};
use crate::protocols::gateway::ChannelRuntime;
use crate::store::ShmDataStore;

fn validate_channel_config_for_runtime(config: &ChannelConfig) -> Result<()> {
    let mut validation = ValidationResult::new(ValidationLevel::Schema);
    config.validate(&mut validation, 0);
    if validation.is_valid {
        Ok(())
    } else {
        Err(IoError::config(
            "channel parameters do not satisfy the runtime protocol schema",
        ))
    }
}

fn command_guard(config: &RuntimeChannelConfig) -> Result<CommandGuard> {
    CommandGuard::from_runtime(config).map_err(|error| IoError::config(error.to_string()))
}

/// Get the base directory for channel log files.
/// Uses AETHER_LOG_DIR environment variable if set, otherwise falls back to "/app/logs".
fn get_channel_log_base_dir() -> String {
    let base = std::env::var("AETHER_LOG_DIR").unwrap_or_else(|_| "/app/logs".to_string());
    format!("{}/io/channels", base)
}

impl ChannelManager {
    /// Configure manager-owned diagnostics for a protocol runtime.
    fn configure_channel_logging(
        protocol: &mut Box<dyn ChannelRuntime>,
        channel_id: u32,
        channel_name: &str,
        logging_config: &crate::core::config::ChannelLoggingConfig,
    ) {
        let mut composite = CompositeLogHandler::new().with_handler(Arc::new(TracingLogHandler));

        if logging_config.enabled {
            let level = FileLogLevel::parse(logging_config.level.as_deref());
            let log_dir = get_channel_log_base_dir();
            let file_handler = ChannelFileLogHandler::new(&log_dir)
                .with_level(level)
                .with_channel(channel_id, channel_name);
            composite.add_handler(Arc::new(file_handler));
            info!(
                "Ch{} file logging enabled (level={:?}, dir={})",
                channel_id, level, log_dir
            );
        }

        let handler: Arc<dyn ChannelLogHandler> = Arc::new(composite);
        protocol.set_log_handler(handler);

        let log_config = if logging_config.enabled {
            let level = logging_config.level.as_deref().unwrap_or("info");
            if level.eq_ignore_ascii_case("debug") {
                ChannelLogConfig::all()
            } else {
                ChannelLogConfig::new()
                    .with_raw_packets(true)
                    .enable_event(LogEventType::RawPacket)
            }
        } else {
            ChannelLogConfig::default()
        };
        protocol.set_log_config(log_config);
    }

    /// Create a runtime through the factory registered for the configured protocol ID.
    pub async fn create_channel(
        &self,
        channel_config: Arc<ChannelConfig>,
    ) -> Result<Arc<ChannelEntry>> {
        let channel_id = channel_config.id();
        let slot = self
            .channels
            .get(channel_id as usize)
            .ok_or_else(|| IoError::invalid_channel_id(channel_id))?;
        if slot.load().is_some() {
            return Err(IoError::channel_exists(channel_id));
        }

        validate_channel_config_for_runtime(&channel_config)?;
        self.protocol_registry.validate(&channel_config)?;

        let mut runtime_config = RuntimeChannelConfig::from_base_arc(Arc::clone(&channel_config));
        self.load_channel_configuration(&mut runtime_config).await?;
        let runtime_config = Arc::new(runtime_config);

        info!(
            "Ch{}: T={} S={} C={} A={} pts",
            channel_id,
            runtime_config.telemetry_points.len(),
            runtime_config.signal_points.len(),
            runtime_config.control_points.len(),
            runtime_config.adjustment_points.len()
        );

        let protocol_id = crate::utils::normalize_protocol_name(runtime_config.protocol());
        let base_config = Arc::clone(&runtime_config.base);
        let mut built = self.protocol_registry.build(&runtime_config)?;
        Self::configure_channel_logging(
            &mut built.runtime,
            channel_id,
            runtime_config.name(),
            &base_config.logging,
        );

        let entry = Arc::new(ChannelEntry::new(
            built.runtime,
            self.create_data_store(),
            base_config,
            protocol_id.to_string(),
            built.poll_interval_ms,
            command_guard(&runtime_config)?,
        )?);
        self.register_channel_subsystems(channel_id, slot, &entry);

        info!("Ch{} created ({})", channel_id, protocol_id);
        Ok(entry)
    }

    fn register_channel_subsystems(
        &self,
        channel_id: u32,
        slot: &arc_swap::ArcSwapOption<ChannelEntry>,
        entry: &Arc<ChannelEntry>,
    ) {
        slot.store(Some(Arc::clone(entry)));
        if let (Some(listener), Some(tx)) = (&self.shm_listener, &entry.command_tx) {
            listener.register_channel(channel_id, tx.clone());
            debug!(
                "Ch{} registered with ShmListener for event-driven dispatch",
                channel_id
            );
        }
        self.active_channel_ids.insert(channel_id);
    }

    fn create_data_store(&self) -> Arc<ShmDataStore> {
        Arc::clone(&self.store)
    }

    async fn load_channel_configuration(
        &self,
        runtime_config: &mut RuntimeChannelConfig,
    ) -> Result<()> {
        use crate::core::config::sqlite_loader::IoSqliteLoader;

        if let Some(ref pool) = self.sqlite_pool {
            let loader = IoSqliteLoader::with_pool(pool.clone());
            loader.load_runtime_channel_points(runtime_config).await?;
        } else {
            let db_path =
                std::env::var("AETHER_DB_PATH").unwrap_or_else(|_| "data/aether.db".to_string());
            let loader = IoSqliteLoader::new(&db_path).await?;
            loader.load_runtime_channel_points(runtime_config).await?;
        }
        Ok(())
    }
}

#[cfg(test)]
#[allow(clippy::disallowed_methods)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Arc;

    #[cfg(feature = "modbus")]
    use super::validate_channel_config_for_runtime;
    use crate::core::config::{ChannelConfig, ChannelCore, ChannelLoggingConfig};

    fn config(
        id: u32,
        protocol: &str,
        parameters: HashMap<String, serde_json::Value>,
    ) -> ChannelConfig {
        ChannelConfig {
            core: ChannelCore {
                id,
                name: "runtime-validation".to_owned(),
                description: None,
                protocol: protocol.to_owned(),
                enabled: true,
            },
            parameters,
            logging: ChannelLoggingConfig::default(),
        }
    }

    #[cfg(feature = "modbus")]
    #[test]
    fn direct_runtime_validation_accepts_historical_zero_id_but_rejects_zero_poll() {
        let valid = HashMap::from([
            ("host".to_owned(), serde_json::json!("127.0.0.1")),
            ("port".to_owned(), serde_json::json!(502)),
        ]);
        assert!(
            validate_channel_config_for_runtime(&config(0, "modbus_tcp", valid.clone())).is_ok()
        );
        let mut invalid = valid;
        invalid.insert("poll_interval_ms".to_owned(), serde_json::json!(0));
        assert!(validate_channel_config_for_runtime(&config(0, "modbus_tcp", invalid)).is_err());
    }

    #[tokio::test]
    async fn channel_manager_rejects_invalid_or_unlinked_config_before_loading_or_spawning() {
        let manager = super::ChannelManager::new_for_test(
            crate::test_utils::create_test_shm_handle(),
            crate::test_utils::create_test_routing_cache(),
        )
        .expect("test channel manager");

        for config in [
            config(
                1,
                "modbus_tcp",
                HashMap::from([
                    ("host".to_owned(), serde_json::json!("127.0.0.1")),
                    ("port".to_owned(), serde_json::json!(502)),
                    ("poll_interval_ms".to_owned(), serde_json::json!(0)),
                ]),
            ),
            config(
                2,
                "modbus_tcp",
                HashMap::from([
                    ("host".to_owned(), serde_json::json!(123)),
                    ("port".to_owned(), serde_json::json!(502)),
                ]),
            ),
            config(3, "not_compiled", HashMap::new()),
        ] {
            let channel_id = config.id();
            let error = manager
                .create_channel(Arc::new(config))
                .await
                .expect_err("invalid direct config must be rejected");
            assert!(matches!(error, super::IoError::ConfigError(_)));
            assert!(manager.get_channel(channel_id).is_none());
        }
    }
}
