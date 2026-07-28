//! Channel creation and factory methods
//!
//! Contains all protocol-specific channel creation logic and
//! SHM store initialization for the ChannelManager.

use std::sync::Arc;

use common::io_config::MAX_CHANNEL_TIMING_MS;
use common::{ValidationLevel, ValidationResult};
#[cfg(any(feature = "aether_485", all(feature = "can", target_os = "linux")))]
use tracing::warn;
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

#[cfg(feature = "modbus")]
fn required_string_parameter<'a>(
    parameters: &'a std::collections::HashMap<String, serde_json::Value>,
    parameter: &str,
) -> Result<&'a str> {
    parameters
        .get(parameter)
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| IoError::config(format!("'{parameter}' must be a non-empty string")))
}

#[cfg(feature = "modbus")]
fn required_u16_parameter(
    parameters: &std::collections::HashMap<String, serde_json::Value>,
    parameter: &str,
) -> Result<u16> {
    let value = parameters
        .get(parameter)
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| IoError::config(format!("'{parameter}' must be a positive integer")))?;
    let value = u16::try_from(value)
        .map_err(|_| IoError::config(format!("'{parameter}' exceeds the u16 range")))?;
    if value == 0 {
        return Err(IoError::config(format!(
            "'{parameter}' must be greater than zero"
        )));
    }
    Ok(value)
}

#[cfg(feature = "modbus")]
fn required_u32_parameter(
    parameters: &std::collections::HashMap<String, serde_json::Value>,
    parameter: &str,
) -> Result<u32> {
    let value = parameters
        .get(parameter)
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| IoError::config(format!("'{parameter}' must be a positive integer")))?;
    let value = u32::try_from(value)
        .map_err(|_| IoError::config(format!("'{parameter}' exceeds the u32 range")))?;
    if value == 0 {
        return Err(IoError::config(format!(
            "'{parameter}' must be greater than zero"
        )));
    }
    Ok(value)
}

fn timing_parameter(
    parameters: &std::collections::HashMap<String, serde_json::Value>,
    parameter: &str,
) -> Result<Option<u64>> {
    let Some(value) = parameters.get(parameter) else {
        return Ok(None);
    };
    let value = value.as_u64().ok_or_else(|| {
        IoError::config(format!(
            "'{parameter}' must be a positive integer number of milliseconds"
        ))
    })?;
    if !(1..=MAX_CHANNEL_TIMING_MS).contains(&value) {
        return Err(IoError::config(format!(
            "'{parameter}' must be between 1 and {MAX_CHANNEL_TIMING_MS} milliseconds"
        )));
    }
    Ok(Some(value))
}

fn poll_interval_ms(
    parameters: &std::collections::HashMap<String, serde_json::Value>,
    default: u64,
) -> Result<u64> {
    Ok(timing_parameter(parameters, "poll_interval_ms")?.unwrap_or(default))
}

fn command_guard(config: &RuntimeChannelConfig) -> Result<CommandGuard> {
    CommandGuard::from_runtime(config).map_err(|error| IoError::config(error.to_string()))
}

#[cfg(feature = "modbus")]
use crate::core::channels::converters::convert_to_modbus_point_configs;
#[cfg(feature = "modbus")]
use crate::core::channels::factory::{create_modbus_channel, create_modbus_rtu_channel};

#[cfg(all(target_os = "linux", feature = "gpio"))]
use crate::core::channels::factory::create_gpio_channel;

#[cfg(all(feature = "can", target_os = "linux"))]
use crate::core::channels::converters::convert_to_can_point_configs;
#[cfg(all(feature = "can", target_os = "linux"))]
use crate::core::channels::factory::create_can_channel;

#[cfg(feature = "aether_485")]
use crate::core::channels::factory::create_aether_485_channel;

/// Get the base directory for channel log files.
/// Uses AETHER_LOG_DIR environment variable if set, otherwise falls back to "/app/logs".
fn get_channel_log_base_dir() -> String {
    let base = std::env::var("AETHER_LOG_DIR").unwrap_or_else(|_| "/app/logs".to_string());
    format!("{}/io/channels", base)
}

impl ChannelManager {
    /// Configure logging for a channel based on ChannelLoggingConfig.
    ///
    /// Sets up both tracing and file logging handlers when enabled.
    /// Returns the composite log handler for hot-reload support.
    fn configure_channel_logging(
        protocol: &mut Box<dyn ChannelRuntime>,
        channel_id: u32,
        channel_name: &str,
        logging_config: &crate::core::config::ChannelLoggingConfig,
    ) -> Arc<dyn ChannelLogHandler> {
        // Create composite handler with tracing
        let mut composite = CompositeLogHandler::new().with_handler(Arc::new(TracingLogHandler));

        // Add file logging if enabled
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

        // Create Arc and clone for return value (for hot-reload support)
        let handler: Arc<dyn ChannelLogHandler> = Arc::new(composite);
        protocol.set_log_handler(handler.clone());

        // Configure log config based on logging level
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
        handler
    }

    /// Create channel
    ///
    /// Returns an Arc to the created ChannelEntry for convenience.
    pub async fn create_channel(
        &self,
        channel_config: Arc<ChannelConfig>,
    ) -> Result<Arc<ChannelEntry>> {
        let channel_id = channel_config.id();

        // Bounds check for pre-allocated Vec
        let slot = self
            .channels
            .get(channel_id as usize)
            .ok_or_else(|| IoError::invalid_channel_id(channel_id))?;

        // Validate channel doesn't exist (O(1) atomic load)
        if slot.load().is_some() {
            return Err(IoError::channel_exists(channel_id));
        }

        // Validate before loading points or constructing a protocol client so
        // direct startup/reload paths cannot bypass the governed mutator.
        validate_channel_config_for_runtime(&channel_config)?;

        // Convert to RuntimeChannelConfig and load configuration from SQLite
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

        // Get protocol using normalized name
        let protocol_name = crate::utils::normalize_protocol_name(runtime_config.protocol());
        let base_config = Arc::clone(&runtime_config.base);

        // Branch based on protocol type - create ChannelEntry directly
        let entry = self
            .create_channel_by_protocol(&protocol_name, channel_id, &runtime_config, base_config)
            .await?;

        let entry = Arc::new(entry);

        // Register channel with all subsystems
        self.register_channel_subsystems(channel_id, slot, &entry, &runtime_config);

        info!("Ch{} created ({})", channel_id, protocol_name);
        Ok(entry)
    }

    /// Create a ChannelEntry for the given protocol type.
    async fn create_channel_by_protocol(
        &self,
        protocol_name: &str,
        channel_id: u32,
        runtime_config: &Arc<RuntimeChannelConfig>,
        base_config: Arc<ChannelConfig>,
    ) -> Result<ChannelEntry> {
        match protocol_name {
            #[cfg(feature = "modbus")]
            "modbus_tcp" => {
                self.create_modbus_channel_impl(channel_id, runtime_config, base_config)
                    .await
            },
            #[cfg(feature = "modbus")]
            "modbus_rtu" => {
                self.create_modbus_rtu_channel_impl(channel_id, runtime_config, base_config)
                    .await
            },
            #[cfg(all(target_os = "linux", feature = "gpio"))]
            "gpio" | "di_do" | "dido" => {
                self.create_gpio_channel_impl(channel_id, runtime_config, base_config)
                    .await
            },
            #[cfg(all(feature = "can", target_os = "linux"))]
            "can" => {
                self.create_can_channel_impl(channel_id, runtime_config, base_config)
                    .await
            },
            #[cfg(feature = "aether_485")]
            "aether_485" => {
                self.create_aether_485_channel_impl(channel_id, runtime_config, base_config)
                    .await
            },
            #[cfg(feature = "iec61850")]
            "iec61850" => {
                self.create_iec61850_channel_impl(channel_id, runtime_config, base_config)
                    .await
            },
            _ => {
                #[allow(unused_mut)]
                let mut supported = Vec::new();
                #[cfg(feature = "modbus")]
                supported.extend(["modbus_tcp", "modbus_rtu"]);
                #[cfg(all(target_os = "linux", feature = "gpio"))]
                supported.push("gpio/di_do");
                #[cfg(all(feature = "can", target_os = "linux"))]
                supported.push("can");
                #[cfg(feature = "aether_485")]
                supported.push("aether_485");
                #[cfg(feature = "iec61850")]
                supported.push("iec61850");

                Err(anyhow::anyhow!(
                    "Unsupported protocol '{}' for channel {}. Supported: {}",
                    protocol_name,
                    channel_id,
                    supported.join(", ")
                )
                .into())
            },
        }
    }

    /// Register a newly created channel with all subsystems.
    fn register_channel_subsystems(
        &self,
        channel_id: u32,
        slot: &arc_swap::ArcSwapOption<ChannelEntry>,
        entry: &Arc<ChannelEntry>,
        _runtime_config: &Arc<RuntimeChannelConfig>,
    ) {
        // 1. Atomic store (publish channel to be visible)
        slot.store(Some(Arc::clone(entry)));

        // 2. Register command_tx with cache
        if let (Some(cache), Some(tx)) = (&self.command_tx_cache, &entry.command_tx) {
            cache.register(channel_id, tx.clone());
        }

        // 3. Register with SHM listener for event-driven M2C dispatch
        if let (Some(listener), Some(tx)) = (&self.shm_listener, &entry.command_tx) {
            listener.register_channel(channel_id, tx.clone());
            debug!(
                "Ch{} registered with ShmListener for event-driven dispatch",
                channel_id
            );
        }

        // 4. Register in active channel index for O(1) iteration
        self.active_channel_ids.insert(channel_id);
    }

    /// Create Modbus TCP channel entry.
    #[cfg(feature = "modbus")]
    async fn create_modbus_channel_impl(
        &self,
        channel_id: u32,
        runtime_config: &Arc<RuntimeChannelConfig>,
        base_config: Arc<ChannelConfig>,
    ) -> Result<ChannelEntry> {
        debug!("Ch{} creating Modbus TCP channel", channel_id);

        let store = self.create_data_store();
        let point_configs = convert_to_modbus_point_configs(runtime_config);

        let params = &runtime_config.base.parameters;
        let host = required_string_parameter(params, "host")?;
        let port = required_u16_parameter(params, "port")?;

        let io_timeout_ms = timing_parameter(params, "read_timeout_ms")?;
        if let Some(timeout) = io_timeout_ms {
            debug!("Ch{} using read_timeout_ms: {}ms", channel_id, timeout);
        }

        let mut protocol =
            create_modbus_channel(channel_id, host, port, point_configs, io_timeout_ms);

        let log_handler = Self::configure_channel_logging(
            &mut protocol,
            channel_id,
            runtime_config.name(),
            &base_config.logging,
        );

        let poll_interval_ms = poll_interval_ms(params, 1000)?;

        let protocol_label = base_config.protocol().to_string();

        ChannelEntry::new(
            protocol,
            store,
            base_config,
            protocol_label,
            poll_interval_ms,
            log_handler,
            command_guard(runtime_config)?,
        )
    }

    /// Create Modbus RTU (serial) channel entry.
    #[cfg(feature = "modbus")]
    async fn create_modbus_rtu_channel_impl(
        &self,
        channel_id: u32,
        runtime_config: &Arc<RuntimeChannelConfig>,
        base_config: Arc<ChannelConfig>,
    ) -> Result<ChannelEntry> {
        debug!("Ch{} creating Modbus RTU channel", channel_id);

        let store = self.create_data_store();
        let point_configs = convert_to_modbus_point_configs(runtime_config);

        let params = &runtime_config.base.parameters;
        let device = required_string_parameter(params, "device")?;
        let baud_rate = required_u32_parameter(params, "baud_rate")?;

        let io_timeout_ms = timing_parameter(params, "read_timeout_ms")?;
        if let Some(timeout) = io_timeout_ms {
            debug!("Ch{} using read_timeout_ms: {}ms", channel_id, timeout);
        }

        let mut protocol =
            create_modbus_rtu_channel(channel_id, device, baud_rate, point_configs, io_timeout_ms);

        let log_handler = Self::configure_channel_logging(
            &mut protocol,
            channel_id,
            runtime_config.name(),
            &base_config.logging,
        );

        let poll_interval_ms = poll_interval_ms(params, 1000)?;

        let protocol_label = base_config.protocol().to_string();

        ChannelEntry::new(
            protocol,
            store,
            base_config,
            protocol_label,
            poll_interval_ms,
            log_handler,
            command_guard(runtime_config)?,
        )
    }

    /// Create GPIO channel entry for DI/DO.
    #[cfg(all(target_os = "linux", feature = "gpio"))]
    async fn create_gpio_channel_impl(
        &self,
        channel_id: u32,
        runtime_config: &Arc<RuntimeChannelConfig>,
        base_config: Arc<ChannelConfig>,
    ) -> Result<ChannelEntry> {
        debug!("Ch{} creating GPIO channel", channel_id);

        let store = self.create_data_store();

        let mut protocol = create_gpio_channel(channel_id, runtime_config);

        let log_handler = Self::configure_channel_logging(
            &mut protocol,
            channel_id,
            runtime_config.name(),
            &base_config.logging,
        );

        // GPIO needs faster polling (default 200ms for responsive DI detection)
        let poll_interval_ms = poll_interval_ms(&runtime_config.base.parameters, 200)?;

        ChannelEntry::new(
            protocol,
            store,
            base_config,
            "gpio".to_string(),
            poll_interval_ms,
            log_handler,
            command_guard(runtime_config)?,
        )
    }

    /// Create CAN channel entry.
    #[cfg(all(feature = "can", target_os = "linux"))]
    async fn create_can_channel_impl(
        &self,
        channel_id: u32,
        runtime_config: &Arc<RuntimeChannelConfig>,
        base_config: Arc<ChannelConfig>,
    ) -> Result<ChannelEntry> {
        debug!("Ch{} creating CAN channel", channel_id);

        let store = self.create_data_store();
        let can_point_configs = convert_to_can_point_configs(runtime_config);

        if can_point_configs.is_empty() {
            warn!("Ch{} has no CAN point mappings configured", channel_id);
        }

        let params = &runtime_config.base.parameters;
        let can_interface = params
            .get("device")
            .and_then(|v| v.as_str())
            .unwrap_or_else(|| {
                info!(
                    "Ch{} CAN device not configured, using default: can0",
                    channel_id
                );
                "can0"
            });

        let mut protocol = create_can_channel(channel_id, can_interface, can_point_configs)?;

        let log_handler = Self::configure_channel_logging(
            &mut protocol,
            channel_id,
            runtime_config.name(),
            &base_config.logging,
        );

        // CAN is event-driven, needs faster polling (default 200ms)
        let poll_interval_ms = poll_interval_ms(params, 200)?;

        ChannelEntry::new(
            protocol,
            store,
            base_config,
            "can".to_string(),
            poll_interval_ms,
            log_handler,
            command_guard(runtime_config)?,
        )
    }

    /// Create Aether-485 channel entry.
    #[cfg(feature = "aether_485")]
    async fn create_aether_485_channel_impl(
        &self,
        channel_id: u32,
        runtime_config: &Arc<RuntimeChannelConfig>,
        base_config: Arc<ChannelConfig>,
    ) -> Result<ChannelEntry> {
        debug!("Ch{} creating Aether-485 channel", channel_id);

        let store = self.create_data_store();
        let params = &runtime_config.base.parameters;

        let mut protocol =
            create_aether_485_channel(channel_id, runtime_config.name(), params, runtime_config);

        let log_handler = Self::configure_channel_logging(
            &mut protocol,
            channel_id,
            runtime_config.name(),
            &base_config.logging,
        );

        let poll_interval_ms = poll_interval_ms(params, 1000)?;

        ChannelEntry::new(
            protocol,
            store,
            base_config,
            "aether_485".to_string(),
            poll_interval_ms,
            log_handler,
            command_guard(runtime_config)?,
        )
    }

    /// Create IEC 61850 MMS channel entry.
    #[cfg(feature = "iec61850")]
    async fn create_iec61850_channel_impl(
        &self,
        channel_id: u32,
        runtime_config: &Arc<RuntimeChannelConfig>,
        base_config: Arc<ChannelConfig>,
    ) -> Result<ChannelEntry> {
        use crate::protocols::adapters::iec61850::{Iec61850Channel, Iec61850ParamsConfig};
        use crate::protocols::core::point::{
            Iec61850Address, PointConfig, ProtocolAddress, TransformConfig,
        };

        debug!("Ch{} creating IEC 61850 MMS channel", channel_id);

        let store = self.create_data_store();
        let params = &runtime_config.base.parameters;

        // Build Iec61850ParamsConfig from the channel's `parameters` block.
        let address = params
            .get("address")
            .and_then(|v| v.as_str())
            .unwrap_or("127.0.0.1:102")
            .to_string();
        let connect_timeout_ms = params
            .get("connect_timeout_ms")
            .and_then(|v| v.as_u64())
            .unwrap_or(10_000);
        let request_timeout_ms = params
            .get("request_timeout_ms")
            .and_then(|v| v.as_u64())
            .unwrap_or(5_000);
        let poll_interval_ms = poll_interval_ms(params, 1_000)?;

        let reports: Vec<crate::protocols::adapters::iec61850::ReportConfig> = params
            .get("reports")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or_default();

        let iec61850_params = Iec61850ParamsConfig {
            address,
            connect_timeout_ms,
            request_timeout_ms,
            reports,
        };

        // Convert points: parse protocol_mappings JSON → Iec61850Address.
        // Expected protocol_mappings format: {"address": "domain/item$..."}
        let mut point_configs: Vec<PointConfig> = Vec::new();

        let parse_iec61850_point = |protocol_mappings: &Option<String>| -> Option<Iec61850Address> {
            let json_str = protocol_mappings.as_deref()?;
            let obj: serde_json::Value = serde_json::from_str(json_str).ok()?;
            let addr_str = obj.get("address")?.as_str()?;
            let ctrl_model = obj.get("ctrl_model").and_then(|v| v.as_u64()).unwrap_or(1) as u8;
            let mut addr = Iec61850Address::parse(addr_str).ok()?;
            addr.ctrl_model = ctrl_model;
            Some(addr)
        };

        for tp in &runtime_config.telemetry_points {
            if let Some(addr) = parse_iec61850_point(&tp.base.protocol_mappings) {
                point_configs.push(PointConfig {
                    id: tp.base.point_id,
                    point_type: common::PointType::Telemetry,
                    name: Some(tp.base.signal_name.clone()),
                    address: ProtocolAddress::Iec61850(addr),
                    transform: TransformConfig {
                        scale: tp.scale,
                        offset: tp.offset,
                        reverse: tp.reverse,
                        ..Default::default()
                    },
                    poll_group: None,
                    enabled: true,
                });
            } else {
                warn!(
                    "Ch{} telemetry point {} has no valid IEC 61850 address in protocol_mappings",
                    channel_id, tp.base.point_id
                );
            }
        }

        for sp in &runtime_config.signal_points {
            if let Some(addr) = parse_iec61850_point(&sp.base.protocol_mappings) {
                point_configs.push(PointConfig {
                    id: sp.base.point_id,
                    point_type: common::PointType::Signal,
                    name: Some(sp.base.signal_name.clone()),
                    address: ProtocolAddress::Iec61850(addr),
                    transform: TransformConfig {
                        reverse: sp.reverse,
                        ..Default::default()
                    },
                    poll_group: None,
                    enabled: true,
                });
            }
        }

        for cp in &runtime_config.control_points {
            if let Some(addr) = parse_iec61850_point(&cp.base.protocol_mappings) {
                point_configs.push(PointConfig {
                    id: cp.base.point_id,
                    point_type: common::PointType::Control,
                    name: Some(cp.base.signal_name.clone()),
                    address: ProtocolAddress::Iec61850(addr),
                    transform: TransformConfig {
                        reverse: cp.reverse,
                        ..Default::default()
                    },
                    poll_group: None,
                    enabled: true,
                });
            } else {
                warn!(
                    "Ch{} control point {} has no valid IEC 61850 address in protocol_mappings",
                    channel_id, cp.base.point_id
                );
            }
        }

        for ap in &runtime_config.adjustment_points {
            if let Some(addr) = parse_iec61850_point(&ap.base.protocol_mappings) {
                point_configs.push(PointConfig {
                    id: ap.base.point_id,
                    point_type: common::PointType::Adjustment,
                    name: Some(ap.base.signal_name.clone()),
                    address: ProtocolAddress::Iec61850(addr),
                    transform: TransformConfig {
                        scale: ap.scale,
                        offset: ap.offset,
                        ..Default::default()
                    },
                    poll_group: None,
                    enabled: true,
                });
            } else {
                warn!(
                    "Ch{} adjustment point {} has no valid IEC 61850 address in protocol_mappings",
                    channel_id, ap.base.point_id
                );
            }
        }

        let mut protocol: Box<dyn crate::protocols::gateway::ChannelRuntime> =
            Box::new(Iec61850Channel::new(
                channel_id,
                runtime_config.name(),
                &iec61850_params,
                point_configs,
            ));

        let log_handler = Self::configure_channel_logging(
            &mut protocol,
            channel_id,
            runtime_config.name(),
            &base_config.logging,
        );

        ChannelEntry::new(
            protocol,
            store,
            base_config,
            "iec61850".to_string(),
            poll_interval_ms,
            log_handler,
            command_guard(runtime_config)?,
        )
    }

    /// Returns the process-wide authoritative SHM store.
    fn create_data_store(&self) -> Arc<ShmDataStore> {
        Arc::clone(&self.store)
    }

    /// Load channel configuration from SQLite
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

    use super::{poll_interval_ms, timing_parameter, validate_channel_config_for_runtime};
    #[cfg(feature = "modbus")]
    use super::{required_string_parameter, required_u16_parameter, required_u32_parameter};
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
    async fn channel_manager_rejects_invalid_config_before_loading_or_spawning_runtime() {
        let manager = super::ChannelManager::new(
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

    #[test]
    fn runtime_timing_parser_never_falls_back_for_present_invalid_values() {
        for value in [
            serde_json::json!(0),
            serde_json::json!("1000"),
            serde_json::json!(86_400_001),
        ] {
            let parameters = HashMap::from([("poll_interval_ms".to_owned(), value)]);
            assert!(poll_interval_ms(&parameters, 1_000).is_err());
        }
        assert_eq!(poll_interval_ms(&HashMap::new(), 1_000).unwrap(), 1_000);
        assert!(
            timing_parameter(
                &HashMap::from([("read_timeout_ms".to_owned(), serde_json::json!(0))]),
                "read_timeout_ms",
            )
            .is_err()
        );
    }

    #[cfg(feature = "modbus")]
    #[test]
    fn direct_modbus_runtime_parser_rejects_fallback_and_truncation() {
        assert!(required_string_parameter(&HashMap::new(), "host").is_err());
        assert!(
            required_string_parameter(
                &HashMap::from([("host".to_owned(), serde_json::json!(123))]),
                "host",
            )
            .is_err()
        );
        assert!(
            required_u16_parameter(
                &HashMap::from([("port".to_owned(), serde_json::json!(65_536))]),
                "port",
            )
            .is_err()
        );
        assert!(
            required_u32_parameter(
                &HashMap::from([("baud_rate".to_owned(), serde_json::json!(4_294_967_296_u64))]),
                "baud_rate",
            )
            .is_err()
        );
        assert_eq!(
            required_u16_parameter(
                &HashMap::from([("port".to_owned(), serde_json::json!(65_535))]),
                "port",
            )
            .unwrap(),
            u16::MAX
        );
        assert_eq!(
            required_u32_parameter(
                &HashMap::from([("baud_rate".to_owned(), serde_json::json!(4_294_967_295_u64))]),
                "baud_rate",
            )
            .unwrap(),
            u32::MAX
        );
    }
}
