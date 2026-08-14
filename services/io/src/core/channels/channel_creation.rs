//! Common channel lifecycle assembly.
//!
//! Protocol-specific discovery, validation, mapping, and construction belong
//! to the compile-time registry in `protocols::factory`.

use std::sync::Arc;

use tracing::{debug, info};

use crate::core::channels::RuntimeChannelConfig;
use crate::core::channels::channel_entry::ChannelEntry;
use crate::core::channels::channel_manager::ChannelManager;
use crate::core::channels::channel_task::CommandDispatchContext;
use crate::core::channels::command_guard::CommandGuard;
use crate::core::channels::runtime_policy::ChannelRuntimePolicy;
use crate::core::config::ChannelConfig;
use crate::error::{IoError, Result};
use crate::protocols::core::file_logging::{ChannelFileLogHandler, FileLogLevel};
use crate::protocols::core::log_handlers::{CompositeLogHandler, TracingLogHandler};
use crate::protocols::core::logging::{ChannelLogConfig, ChannelLogHandler, LogEventType};
use crate::protocols::runtime::ChannelRuntime;

pub(crate) fn validate_channel_config_for_runtime(config: &ChannelConfig) -> Result<()> {
    FileLogLevel::parse(config.logging.level.as_deref()).ok_or_else(|| {
        IoError::config("channel log level must be exactly debug, info, or error")
    })?;
    crate::protocols::get_protocol_registry()
        .factory(config.protocol())
        .ok_or_else(|| IoError::config("channel protocol is unavailable in this IO runtime build"))?
        .validate_channel(config)
}

fn command_guard(config: &RuntimeChannelConfig) -> Result<CommandGuard> {
    CommandGuard::from_runtime(config).map_err(|error| IoError::config(error.to_string()))
}

fn channel_log_base_dir() -> String {
    let base = std::env::var("AETHER_LOG_DIR").unwrap_or_else(|_| "/app/logs".to_string());
    format!("{base}/io/channels")
}

impl ChannelManager {
    fn configure_channel_logging(
        &self,
        protocol: &mut Box<dyn ChannelRuntime>,
        channel_id: u32,
        channel_name: &str,
        logging_config: &crate::core::config::ChannelLoggingConfig,
    ) -> Result<Arc<dyn ChannelLogHandler>> {
        let mut composite = CompositeLogHandler::new().with_handler(Arc::new(TracingLogHandler));
        let configured_level = logging_config.level.as_deref().unwrap_or("info");
        let level = FileLogLevel::parse(Some(configured_level)).ok_or_else(|| {
            IoError::config("channel log level must be exactly debug, info, or error")
        })?;
        if logging_config.enabled {
            let log_dir = channel_log_base_dir();
            composite.add_handler(Arc::new(
                ChannelFileLogHandler::new(&log_dir, self.file_log_worker()?)
                    .with_level(level)
                    .with_channel(channel_id, channel_name),
            ));
            info!(
                "Ch{} file logging enabled (level={:?}, dir={})",
                channel_id, level, log_dir
            );
        }

        let handler: Arc<dyn ChannelLogHandler> = Arc::new(composite);
        protocol.set_log_handler(Arc::clone(&handler));
        let log_config = if !logging_config.enabled {
            ChannelLogConfig::default()
        } else {
            match configured_level {
                "debug" => ChannelLogConfig::all(),
                "info" => ChannelLogConfig::new()
                    .with_raw_packets(true)
                    .enable_event(LogEventType::RawPacket),
                "error" => ChannelLogConfig::errors_only(),
                _ => return Err(IoError::config("invalid canonical channel log level")),
            }
        };
        protocol.set_log_config(log_config);
        Ok(handler)
    }

    /// Compile and publish one immutable runtime snapshot.
    pub fn create_channel(
        &self,
        runtime_config: RuntimeChannelConfig,
    ) -> Result<Arc<ChannelEntry>> {
        // Hold this lease through subsystem registration. Shutdown takes the
        // write side and therefore cannot snapshot active IDs until every
        // already-started creation is either published or rejected.
        let _creation_lease = self.acquire_creation_lease()?;
        let channel_id = runtime_config.id();
        let _reservation = self.reserve_channel_lifecycle(channel_id)?;
        let slot = self
            .channels
            .get(channel_id as usize)
            .ok_or_else(|| IoError::invalid_channel_id(channel_id))?;
        if slot.load().is_some() {
            return Err(IoError::channel_exists(channel_id));
        }

        let factory = crate::protocols::get_protocol_registry()
            .factory(runtime_config.protocol())
            .ok_or_else(|| {
                IoError::config(format!(
                    "channel {channel_id} protocol is unavailable in this IO runtime build"
                ))
            })?;

        info!(
            "Ch{}: T={} S={} C={} A={} pts",
            channel_id,
            runtime_config.telemetry_points.len(),
            runtime_config.signal_points.len(),
            runtime_config.control_points.len(),
            runtime_config.adjustment_points.len()
        );

        let runtime_policy = ChannelRuntimePolicy::compile(
            &runtime_config.channel_config().parameters,
            factory.default_poll_interval_ms(),
        )?;
        let mut protocol = factory.build(&runtime_config)?;
        let log_handler = self.configure_channel_logging(
            &mut protocol,
            channel_id,
            runtime_config.name(),
            &runtime_config.channel_config().logging,
        )?;
        let command_guard = command_guard(&runtime_config)?;
        let base_config = runtime_config.base;
        let (entry, command_tx) = ChannelEntry::new(
            protocol,
            Arc::clone(&self.store),
            channel_id,
            base_config.core.name,
            factory.metadata().protocol_type,
            runtime_policy,
            log_handler,
            CommandDispatchContext {
                guard: command_guard,
                outcomes: Arc::clone(&self.command_outcomes),
                ledger: self.command_ledger.clone(),
            },
        )?;
        let entry = Arc::new(entry);

        let previous = slot.compare_and_swap(&None::<Arc<ChannelEntry>>, Some(Arc::clone(&entry)));
        if previous.is_some() {
            if let Some(handle) = entry.take_task_handle() {
                handle.abort();
            }
            return Err(IoError::channel_exists(channel_id));
        }

        self.register_channel_subsystems(channel_id, command_tx);
        info!(
            "Ch{} created ({})",
            channel_id,
            factory.metadata().protocol_type
        );
        Ok(entry)
    }

    fn register_channel_subsystems(
        &self,
        channel_id: u32,
        command_tx: tokio::sync::mpsc::Sender<super::traits::ChannelCommand>,
    ) {
        if let Some(listener) = &self.shm_listener {
            listener.register_channel(channel_id, command_tx);
            debug!(
                "Ch{} registered with ShmListener for event-driven dispatch",
                channel_id
            );
        }
        self.active_channel_ids.insert(channel_id);
    }
}

#[cfg(test)]
#[allow(clippy::disallowed_methods)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Arc;

    use super::validate_channel_config_for_runtime;
    use crate::core::channels::RuntimeChannelConfig;
    use crate::core::config::{ChannelConfig, ChannelCore, ChannelLoggingConfig};

    fn config(
        id: u32,
        protocol: &str,
        parameters: HashMap<String, serde_json::Value>,
    ) -> ChannelConfig {
        ChannelConfig {
            core: ChannelCore {
                id,
                name: format!("{protocol}-{id}"),
                description: None,
                protocol: protocol.to_string(),
                enabled: true,
            },
            parameters,
            logging: ChannelLoggingConfig::default(),
        }
    }

    #[cfg(feature = "modbus")]
    #[test]
    fn runtime_validation_uses_the_registered_modbus_factory() {
        let valid = HashMap::from([
            ("host".to_owned(), serde_json::json!("127.0.0.1")),
            ("port".to_owned(), serde_json::json!(502)),
        ]);
        assert!(
            validate_channel_config_for_runtime(&config(1, "modbus_tcp", valid.clone())).is_ok()
        );

        let mut unknown = valid;
        unknown.insert("silent_fallback".to_owned(), serde_json::json!(true));
        assert!(validate_channel_config_for_runtime(&config(1, "modbus_tcp", unknown)).is_err());
    }

    #[cfg(feature = "modbus")]
    #[test]
    fn runtime_validation_rejects_noncanonical_channel_log_levels() {
        let parameters = HashMap::from([
            ("host".to_owned(), serde_json::json!("127.0.0.1")),
            ("port".to_owned(), serde_json::json!(502)),
        ]);
        let mut invalid = config(1, "modbus_tcp", parameters);
        invalid.logging.level = Some("DEBUG".to_owned());
        assert!(validate_channel_config_for_runtime(&invalid).is_err());
    }

    #[cfg(feature = "modbus")]
    #[test]
    fn channel_manager_rejects_invalid_config_before_publishing_runtime() {
        let manager = super::ChannelManager::new(
            crate::test_utils::create_test_shm_handle(),
            crate::test_utils::create_test_routing_cache(),
        )
        .expect("test channel manager");
        let invalid = config(
            7,
            "modbus_tcp",
            HashMap::from([
                ("host".to_owned(), serde_json::json!(123)),
                ("port".to_owned(), serde_json::json!(502)),
            ]),
        );
        assert!(
            manager
                .create_channel(RuntimeChannelConfig::from_base(invalid))
                .is_err()
        );
        assert!(manager.get_channel(7).is_none());
    }

    #[cfg(feature = "modbus")]
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_channel_creation_publishes_exactly_one_runtime() {
        let manager = Arc::new(
            super::ChannelManager::new(
                crate::test_utils::create_test_shm_handle(),
                crate::test_utils::create_test_routing_cache(),
            )
            .expect("test channel manager"),
        );
        let contenders = 16;
        let barrier = Arc::new(tokio::sync::Barrier::new(contenders));
        let mut tasks = tokio::task::JoinSet::new();
        for _ in 0..contenders {
            let manager = Arc::clone(&manager);
            let barrier = Arc::clone(&barrier);
            tasks.spawn(async move {
                barrier.wait().await;
                let channel = config(
                    8,
                    "modbus_tcp",
                    HashMap::from([
                        ("host".to_owned(), serde_json::json!("127.0.0.1")),
                        ("port".to_owned(), serde_json::json!(9)),
                    ]),
                );
                manager
                    .create_channel(RuntimeChannelConfig::from_base(channel))
                    .is_ok()
            });
        }

        let mut successes = 0;
        while let Some(result) = tasks.join_next().await {
            successes += usize::from(result.expect("creation task"));
        }
        assert_eq!(successes, 1);
        assert_eq!(manager.channel_count(), 1);
        manager.remove_channel(8).await.expect("remove winner");
    }

    #[cfg(feature = "modbus")]
    #[test]
    fn shutdown_fence_deterministically_rejects_late_channel_creation() {
        let manager = super::ChannelManager::new(
            crate::test_utils::create_test_shm_handle(),
            crate::test_utils::create_test_routing_cache(),
        )
        .expect("test channel manager");
        manager.begin_shutdown().expect("establish shutdown fence");

        let channel = config(
            9,
            "modbus_tcp",
            HashMap::from([
                ("host".to_owned(), serde_json::json!("127.0.0.1")),
                ("port".to_owned(), serde_json::json!(502)),
            ]),
        );
        let error = manager
            .create_channel(RuntimeChannelConfig::from_base(channel))
            .expect_err("shutdown must reject a late runtime");

        assert!(manager.is_shutting_down());
        assert!(error.to_string().contains("shutting down"));
        assert_eq!(manager.channel_count(), 0);
        assert!(manager.get_channel(9).is_none());
    }

    #[tokio::test]
    async fn every_discoverable_driver_example_uses_its_registered_factory() {
        let manager = super::ChannelManager::new(
            crate::test_utils::create_test_shm_handle(),
            crate::test_utils::create_test_routing_cache(),
        )
        .expect("test channel manager");

        let mut channel_id = 1;
        for protocol in crate::protocols::get_protocol_registry().protocols() {
            assert!(!protocol.drivers.is_empty());
            for driver in &protocol.drivers {
                let parameters = driver
                    .example_config
                    .as_object()
                    .unwrap_or_else(|| panic!("{} example must be an object", driver.name))
                    .clone()
                    .into_iter()
                    .collect();
                let channel = config(channel_id, protocol.protocol_type, parameters);
                manager
                    .create_channel(RuntimeChannelConfig::from_base(channel))
                    .unwrap_or_else(|error| {
                        panic!(
                            "{} ({}) is discoverable but not constructible: {error}",
                            protocol.protocol_type, driver.name
                        )
                    });
                channel_id += 1;
            }
        }
    }
}
