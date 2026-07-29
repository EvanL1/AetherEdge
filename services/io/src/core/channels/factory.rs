//! Protocol client factory
//!
//! Create ChannelRuntime implementations from configuration.
//!
//! This module provides factory functions that create physical protocol client
//! instances from IO configuration.

#[cfg(any(
    feature = "mqtt",
    feature = "http",
    feature = "aether_485",
    all(target_os = "linux", feature = "gpio")
))]
use std::collections::HashMap;
use std::sync::Arc;

#[cfg(any(
    feature = "mqtt",
    feature = "http",
    feature = "aether_485",
    all(target_os = "linux", feature = "gpio")
))]
use serde::de::DeserializeOwned;

#[cfg(any(
    feature = "modbus",
    feature = "mqtt",
    feature = "http",
    feature = "aether_485",
    feature = "iec61850",
    all(target_os = "linux", feature = "can"),
    all(target_os = "linux", feature = "gpio")
))]
use common::io_config::MAX_CHANNEL_TIMING_MS;

#[cfg(any(
    feature = "modbus",
    feature = "mqtt",
    feature = "http",
    feature = "aether_485",
    feature = "iec61850",
    all(target_os = "linux", feature = "can"),
    all(target_os = "linux", feature = "gpio")
))]
use crate::core::channels::protocol_registry::BuiltProtocolRuntime;
use crate::core::channels::protocol_registry::{ProtocolAdapterFactory, ProtocolRegistry};
#[cfg(any(
    feature = "modbus",
    feature = "mqtt",
    feature = "http",
    feature = "aether_485",
    feature = "iec61850",
    all(target_os = "linux", feature = "can"),
    all(target_os = "linux", feature = "gpio")
))]
use crate::core::config::{ChannelConfig, RuntimeChannelConfig};
#[cfg(any(
    feature = "modbus",
    feature = "mqtt",
    feature = "http",
    feature = "aether_485",
    feature = "iec61850",
    all(target_os = "linux", feature = "can"),
    all(target_os = "linux", feature = "gpio")
))]
use crate::error::IoError;
use crate::error::Result;
#[cfg(any(
    feature = "mqtt",
    feature = "http",
    feature = "aether_485",
    all(target_os = "linux", feature = "gpio")
))]
use crate::protocols::core::error::GatewayError;
#[cfg(any(
    feature = "modbus",
    feature = "mqtt",
    feature = "http",
    feature = "aether_485",
    feature = "iec61850",
    all(target_os = "linux", feature = "can"),
    all(target_os = "linux", feature = "gpio")
))]
use crate::protocols::gateway::ChannelRuntime;

#[cfg(feature = "modbus")]
use crate::protocols::adapters::modbus::ModbusChannel;
#[cfg(feature = "modbus")]
use crate::protocols::adapters::modbus_config::ModbusChannelConfig;
#[cfg(feature = "modbus")]
use crate::protocols::core::point::PointConfig;

#[cfg(all(target_os = "linux", feature = "gpio"))]
use crate::protocols::adapters::gpio::{GpioChannel, GpioChannelParamsConfig, GpioPinConfig};

#[cfg(all(feature = "can", target_os = "linux"))]
use crate::protocols::adapters::can::{CanClient, CanConfig, CanPoint};

#[cfg(feature = "aether_485")]
use crate::protocols::adapters::aether_485::{
    Aether485Channel, Aether485ParamsConfig, Aether485PointMapping, PollTarget,
};

#[cfg(feature = "http")]
use crate::protocols::adapters::http::{HttpChannel, HttpParamsConfig};
#[cfg(feature = "mqtt")]
use crate::protocols::adapters::mqtt::{MqttChannel, MqttParamsConfig};
#[cfg(any(feature = "mqtt", feature = "http"))]
use crate::protocols::core::json_mapper::{JsonMapper, JsonMappingConfig};

#[cfg(any(
    feature = "mqtt",
    feature = "http",
    feature = "aether_485",
    all(target_os = "linux", feature = "gpio")
))]
fn parse_parameters<T: DeserializeOwned>(
    parameters: &HashMap<String, serde_json::Value>,
) -> crate::protocols::core::error::Result<T> {
    serde_json::from_value(serde_json::to_value(parameters).map_err(|error| {
        GatewayError::Config(format!("Failed to encode channel parameters: {error}"))
    })?)
    .map_err(|error| GatewayError::Config(format!("Invalid channel parameters: {error}")))
}

#[cfg(feature = "mqtt")]
fn mqtt_parameters(
    parameters: &HashMap<String, serde_json::Value>,
) -> crate::protocols::core::error::Result<MqttParamsConfig> {
    for retired in [
        "connect_timeout_ms",
        "max_reconnect_attempts",
        "reconnect_delay_ms",
    ] {
        if parameters.contains_key(retired) {
            return Err(GatewayError::Config(format!(
                "MQTT parameter '{retired}' was never enforced and has been retired"
            )));
        }
    }
    let config = parse_parameters::<MqttParamsConfig>(parameters)?;
    config.validate()?;
    Ok(config)
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

#[cfg(any(
    feature = "modbus",
    feature = "mqtt",
    feature = "aether_485",
    feature = "iec61850",
    all(target_os = "linux", feature = "can"),
    all(target_os = "linux", feature = "gpio")
))]
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

#[cfg(any(
    feature = "modbus",
    feature = "mqtt",
    feature = "aether_485",
    feature = "iec61850",
    all(target_os = "linux", feature = "can"),
    all(target_os = "linux", feature = "gpio")
))]
fn poll_interval_ms(
    parameters: &std::collections::HashMap<String, serde_json::Value>,
    default: u64,
) -> Result<u64> {
    Ok(timing_parameter(parameters, "poll_interval_ms")?.unwrap_or(default))
}

#[cfg(any(feature = "mqtt", feature = "http"))]
fn compile_json_mapper(
    runtime_config: &RuntimeChannelConfig,
    mapping_config: &JsonMappingConfig,
) -> crate::protocols::core::error::Result<Arc<JsonMapper>> {
    use common::PointType;

    let rows = runtime_config
        .telemetry_points
        .iter()
        .map(|point| {
            (
                point.base.point_id,
                PointType::Telemetry,
                point.base.protocol_mappings.as_deref(),
            )
        })
        .chain(runtime_config.signal_points.iter().map(|point| {
            (
                point.base.point_id,
                PointType::Signal,
                point.base.protocol_mappings.as_deref(),
            )
        }))
        .chain(runtime_config.control_points.iter().map(|point| {
            (
                point.base.point_id,
                PointType::Control,
                point.base.protocol_mappings.as_deref(),
            )
        }))
        .chain(runtime_config.adjustment_points.iter().map(|point| {
            (
                point.base.point_id,
                PointType::Adjustment,
                point.base.protocol_mappings.as_deref(),
            )
        }));
    Ok(Arc::new(
        JsonMapper::from_inline_mappings(runtime_config.id(), rows)?.with_config(mapping_config)?,
    ))
}

// ============================================================================
// Modbus Channel Factory
// ============================================================================

/// Create a ModbusChannel for TCP mode wrapped as ChannelRuntime.
///
/// Note: The channel no longer holds a store reference. Storage is handled
/// by the service layer (ChannelManager) after polling.
///
/// # Arguments
///
/// * `channel_id` - Unique channel identifier (used for logging)
/// * `host` - Modbus TCP server host address
/// * `port` - Modbus TCP server port
/// * `point_configs` - Point configurations with Modbus addresses
/// * `io_timeout_ms` - Optional I/O timeout in milliseconds (default: 3000ms)
#[cfg(feature = "modbus")]
fn create_modbus_channel(
    channel_id: u32,
    host: &str,
    port: u16,
    point_configs: Vec<PointConfig>,
    io_timeout_ms: Option<u64>,
) -> Box<dyn ChannelRuntime> {
    use std::time::Duration;

    let address = format!("{}:{}", host, port);

    let mut config = ModbusChannelConfig::tcp(&address).with_points(point_configs);

    // Apply custom I/O timeout if provided
    if let Some(timeout_ms) = io_timeout_ms {
        config = config.with_io_timeout(Duration::from_millis(timeout_ms));
    }

    let channel_name = format!("modbus_tcp_{}", channel_id);
    let channel = ModbusChannel::new(config, channel_id, channel_name);

    // ModbusChannel directly implements ChannelRuntime - no wrapper needed
    // Logging is configured by ChannelManager.configure_channel_logging()
    Box::new(channel)
}

/// Create a ModbusChannel for RTU (serial) mode wrapped as ChannelRuntime.
///
/// Note: The channel no longer holds a store reference. Storage is handled
/// by the service layer (ChannelManager) after polling.
///
/// # Arguments
///
/// * `channel_id` - Unique channel identifier (used for logging)
/// * `device` - Serial device path (e.g., "/dev/ttyUSB0" on Linux)
/// * `baud_rate` - Serial baud rate (e.g., 9600, 19200, 115200)
/// * `point_configs` - Point configurations with Modbus addresses
/// * `io_timeout_ms` - Optional I/O timeout in milliseconds (default: 3000ms)
#[cfg(feature = "modbus")]
fn create_modbus_rtu_channel(
    channel_id: u32,
    device: &str,
    baud_rate: u32,
    point_configs: Vec<PointConfig>,
    io_timeout_ms: Option<u64>,
) -> Box<dyn ChannelRuntime> {
    use std::time::Duration;

    let mut config = ModbusChannelConfig::rtu(device, baud_rate).with_points(point_configs);

    // Apply custom I/O timeout if provided
    if let Some(timeout_ms) = io_timeout_ms {
        config = config.with_io_timeout(Duration::from_millis(timeout_ms));
    }

    let channel_name = format!("modbus_rtu_{}", channel_id);
    let channel = ModbusChannel::new(config, channel_id, channel_name);

    // ModbusChannel directly implements ChannelRuntime - no wrapper needed
    // Logging is configured by ChannelManager.configure_channel_logging()
    Box::new(channel)
}

// ============================================================================
// GPIO Channel Factory
// ============================================================================

/// Create a GpioChannel for digital I/O wrapped as ChannelRuntime.
///
/// Note: Only available on Linux with `gpio` feature enabled.
/// Storage is handled by the service layer (ChannelManager) after polling.
///
/// GPIO pins use explicit `point_type` in `GpioPinConfig`:
/// - Digital inputs (DI) → `PointType::Signal`
/// - Digital outputs (DO) → `PointType::Control`
///
/// # Arguments
///
/// * `channel_id` - Unique channel identifier
/// * `runtime_config` - Channel configuration containing GPIO pin mappings
#[cfg(all(target_os = "linux", feature = "gpio"))]
fn create_gpio_channel(
    channel_id: u32,
    runtime_config: &RuntimeChannelConfig,
) -> Result<Box<dyn ChannelRuntime>> {
    let params = parse_parameters::<GpioChannelParamsConfig>(&runtime_config.base.parameters)?;
    let mut gpio_config = params.to_config()?;

    // Helper to parse gpio_number from protocol_mappings JSON
    // Expected format: {"gpio_number": 496, ...}
    let parse_gpio_number = |protocol_mappings: &Option<String>| -> Option<u32> {
        let json_str = protocol_mappings.as_ref()?;
        let json: serde_json::Value = serde_json::from_str(json_str).ok()?;
        json.get("gpio_number")?.as_u64().map(|n| n as u32)
    };

    // Configure DI pins from signal points (using sysfs with global GPIO numbers)
    // GpioPinConfig::digital_input_sysfs automatically sets point_type = Signal
    for pt in &runtime_config.signal_points {
        if let Some(gpio_num) = parse_gpio_number(&pt.base.protocol_mappings) {
            let pin_config = GpioPinConfig::digital_input_sysfs(gpio_num, pt.base.point_id)
                .with_active_low(pt.reverse);

            gpio_config = gpio_config.add_pin(pin_config);
        }
    }

    // Configure DO pins from control points (using sysfs with global GPIO numbers)
    // GpioPinConfig::digital_output_sysfs automatically sets point_type = Control
    for pt in &runtime_config.control_points {
        if let Some(gpio_num) = parse_gpio_number(&pt.base.protocol_mappings) {
            let pin_config = GpioPinConfig::digital_output_sysfs(gpio_num, pt.base.point_id)
                .with_active_low(pt.reverse);

            gpio_config = gpio_config.add_pin(pin_config);
        }
    }

    let channel_name = format!("gpio_{}", channel_id);
    // GpioChannel directly implements ChannelRuntime - no wrapper needed
    let channel = GpioChannel::new(gpio_config, channel_id, channel_name);
    Ok(Box::new(channel))
}

// ============================================================================
// CAN Channel Factory
// ============================================================================

/// Create a CAN channel with the given configuration wrapped as ChannelRuntime.
///
/// This function creates a CanClient with the specified
/// CAN interface and point configurations.
#[cfg(all(feature = "can", target_os = "linux"))]
fn create_can_channel(
    channel_id: u32,
    can_interface: &str,
    points: Vec<CanPoint>,
) -> crate::protocols::core::error::Result<Box<dyn ChannelRuntime>> {
    let config = CanConfig {
        can_interface: can_interface.to_string(),
        bitrate: 250000,
        connect_timeout_ms: 3000,
        read_timeout_ms: 3000,
        retry_interval_ms: 2000,
        rx_poll_interval_ms: 50,
        data_read_interval_ms: 1000,
    };

    let channel_name = format!("can_{}", channel_id);
    // CanClient directly implements ChannelRuntime - no wrapper needed
    let mut client = CanClient::new(config, channel_id, channel_name);
    client.add_points(points)?;

    Ok(Box::new(client))
}

// ============================================================================
// JSON payload channel factories
// ============================================================================

/// Create an event-driven MQTT acquisition channel from reviewed parameters
/// and the same physical topology generation used by the channel runtime.
#[cfg(feature = "mqtt")]
fn create_mqtt_channel(
    channel_id: u32,
    channel_name: &str,
    runtime_config: &RuntimeChannelConfig,
) -> Result<Box<dyn ChannelRuntime>> {
    let params = mqtt_parameters(&runtime_config.base.parameters)?;
    let mapper = compile_json_mapper(runtime_config, &params.json_mapping)?;
    Ok(Box::new(MqttChannel::new(
        params.to_config(channel_id),
        channel_id,
        channel_name.to_owned(),
        mapper,
    )))
}

/// Create an outbound HTTP polling channel and return its polling interval.
#[cfg(feature = "http")]
fn create_http_channel(
    channel_id: u32,
    channel_name: &str,
    runtime_config: &RuntimeChannelConfig,
) -> Result<(Box<dyn ChannelRuntime>, u64)> {
    let params = parse_parameters::<HttpParamsConfig>(&runtime_config.base.parameters)?;
    params.validate()?;
    let mapper = compile_json_mapper(runtime_config, &params.json_mapping)?;
    let interval_ms = params.poll_interval_ms;
    Ok((
        Box::new(HttpChannel::new(
            params.to_config(),
            channel_id,
            channel_name.to_owned(),
            mapper,
        )),
        interval_ms,
    ))
}

// ============================================================================
// Aether-485 Channel Factory
// ============================================================================

/// Create a Aether-485 channel from runtime configuration.
///
/// Parses per-point `protocol_mappings` JSON (`{"device_id": N}`) to build
/// the list of poll targets, then assembles the serial channel.
#[cfg(feature = "aether_485")]
fn create_aether_485_channel(
    channel_id: u32,
    channel_name: &str,
    runtime_config: &RuntimeChannelConfig,
) -> Result<Box<dyn ChannelRuntime>> {
    use common::PointType;

    let params = parse_parameters::<Aether485ParamsConfig>(&runtime_config.base.parameters)?;
    let config = params.to_channel_config();

    let mut targets = Vec::new();

    for pt in &runtime_config.telemetry_points {
        if let Some(json_str) = pt.base.protocol_mappings.as_deref() {
            match serde_json::from_str::<Aether485PointMapping>(json_str) {
                Ok(mapping) => targets.push(PollTarget {
                    point_id: pt.base.point_id,
                    point_type: PointType::Telemetry,
                    device_id: mapping.device_id,
                    cmd: mapping.cmd,
                }),
                Err(e) => tracing::warn!(
                    "Ch{} point {} invalid aether_485 mapping: {}",
                    channel_id,
                    pt.base.point_id,
                    e
                ),
            }
        }
    }

    for pt in &runtime_config.signal_points {
        if let Some(json_str) = pt.base.protocol_mappings.as_deref() {
            match serde_json::from_str::<Aether485PointMapping>(json_str) {
                Ok(mapping) => targets.push(PollTarget {
                    point_id: pt.base.point_id,
                    point_type: PointType::Signal,
                    device_id: mapping.device_id,
                    cmd: mapping.cmd,
                }),
                Err(e) => tracing::warn!(
                    "Ch{} point {} invalid aether_485 mapping: {}",
                    channel_id,
                    pt.base.point_id,
                    e
                ),
            }
        }
    }

    let name = if channel_name.is_empty() {
        format!("v485_{}", channel_id)
    } else {
        channel_name.to_string()
    };

    Ok(Box::new(Aether485Channel::new(
        config, channel_id, name, targets,
    )))
}

// ============================================================================
// Statically linked protocol factory composition
// ============================================================================

#[cfg(feature = "modbus")]
struct ModbusTcpFactory;

#[cfg(feature = "modbus")]
impl ProtocolAdapterFactory for ModbusTcpFactory {
    fn protocol_id(&self) -> &'static str {
        "modbus_tcp"
    }

    fn validate(&self, config: &ChannelConfig) -> Result<()> {
        required_string_parameter(&config.parameters, "host")?;
        required_u16_parameter(&config.parameters, "port")?;
        timing_parameter(&config.parameters, "read_timeout_ms")?;
        poll_interval_ms(&config.parameters, 1_000)?;
        Ok(())
    }

    fn build(&self, config: &RuntimeChannelConfig) -> Result<BuiltProtocolRuntime> {
        let points = crate::core::channels::converters::convert_to_modbus_point_configs(config);
        let host = required_string_parameter(&config.base.parameters, "host")?;
        let port = required_u16_parameter(&config.base.parameters, "port")?;
        let timeout = timing_parameter(&config.base.parameters, "read_timeout_ms")?;
        Ok(BuiltProtocolRuntime::new(
            create_modbus_channel(config.id(), host, port, points, timeout),
            poll_interval_ms(&config.base.parameters, 1_000)?,
        ))
    }
}

#[cfg(feature = "modbus")]
struct ModbusRtuFactory;

#[cfg(feature = "modbus")]
impl ProtocolAdapterFactory for ModbusRtuFactory {
    fn protocol_id(&self) -> &'static str {
        "modbus_rtu"
    }

    fn validate(&self, config: &ChannelConfig) -> Result<()> {
        required_string_parameter(&config.parameters, "device")?;
        required_u32_parameter(&config.parameters, "baud_rate")?;
        timing_parameter(&config.parameters, "read_timeout_ms")?;
        poll_interval_ms(&config.parameters, 1_000)?;
        Ok(())
    }

    fn build(&self, config: &RuntimeChannelConfig) -> Result<BuiltProtocolRuntime> {
        let points = crate::core::channels::converters::convert_to_modbus_point_configs(config);
        let device = required_string_parameter(&config.base.parameters, "device")?;
        let baud_rate = required_u32_parameter(&config.base.parameters, "baud_rate")?;
        let timeout = timing_parameter(&config.base.parameters, "read_timeout_ms")?;
        Ok(BuiltProtocolRuntime::new(
            create_modbus_rtu_channel(config.id(), device, baud_rate, points, timeout),
            poll_interval_ms(&config.base.parameters, 1_000)?,
        ))
    }
}

#[cfg(all(target_os = "linux", feature = "gpio"))]
struct GpioFactory;

#[cfg(all(target_os = "linux", feature = "gpio"))]
impl ProtocolAdapterFactory for GpioFactory {
    fn protocol_id(&self) -> &'static str {
        "gpio"
    }

    fn validate(&self, config: &ChannelConfig) -> Result<()> {
        parse_parameters::<GpioChannelParamsConfig>(&config.parameters)?
            .to_config()
            .map(|_| ())?;
        poll_interval_ms(&config.parameters, 200)?;
        Ok(())
    }

    fn build(&self, config: &RuntimeChannelConfig) -> Result<BuiltProtocolRuntime> {
        Ok(BuiltProtocolRuntime::new(
            create_gpio_channel(config.id(), config)?,
            poll_interval_ms(&config.base.parameters, 200)?,
        ))
    }
}

#[cfg(all(feature = "can", target_os = "linux"))]
struct CanFactory;

#[cfg(all(feature = "can", target_os = "linux"))]
impl ProtocolAdapterFactory for CanFactory {
    fn protocol_id(&self) -> &'static str {
        "can"
    }

    fn validate(&self, config: &ChannelConfig) -> Result<()> {
        poll_interval_ms(&config.parameters, 200).map(|_| ())
    }

    fn build(&self, config: &RuntimeChannelConfig) -> Result<BuiltProtocolRuntime> {
        let points = crate::core::channels::converters::convert_to_can_point_configs(config);
        let interface = config
            .base
            .parameters
            .get("device")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("can0");
        Ok(BuiltProtocolRuntime::new(
            create_can_channel(config.id(), interface, points)?,
            poll_interval_ms(&config.base.parameters, 200)?,
        ))
    }
}

#[cfg(feature = "aether_485")]
struct Aether485Factory;

#[cfg(feature = "aether_485")]
impl ProtocolAdapterFactory for Aether485Factory {
    fn protocol_id(&self) -> &'static str {
        "aether_485"
    }

    fn validate(&self, config: &ChannelConfig) -> Result<()> {
        parse_parameters::<Aether485ParamsConfig>(&config.parameters)?;
        poll_interval_ms(&config.parameters, 1_000).map(|_| ())
    }

    fn build(&self, config: &RuntimeChannelConfig) -> Result<BuiltProtocolRuntime> {
        Ok(BuiltProtocolRuntime::new(
            create_aether_485_channel(config.id(), config.name(), config)?,
            poll_interval_ms(&config.base.parameters, 1_000)?,
        ))
    }
}

#[cfg(feature = "mqtt")]
struct MqttFactory;

#[cfg(feature = "mqtt")]
impl ProtocolAdapterFactory for MqttFactory {
    fn protocol_id(&self) -> &'static str {
        "mqtt"
    }

    fn validate(&self, config: &ChannelConfig) -> Result<()> {
        mqtt_parameters(&config.parameters)?;
        poll_interval_ms(&config.parameters, 1_000).map(|_| ())
    }

    fn build(&self, config: &RuntimeChannelConfig) -> Result<BuiltProtocolRuntime> {
        Ok(BuiltProtocolRuntime::new(
            create_mqtt_channel(config.id(), config.name(), config)?,
            poll_interval_ms(&config.base.parameters, 1_000)?,
        ))
    }
}

#[cfg(feature = "http")]
struct HttpFactory;

#[cfg(feature = "http")]
impl ProtocolAdapterFactory for HttpFactory {
    fn protocol_id(&self) -> &'static str {
        "http"
    }

    fn validate(&self, config: &ChannelConfig) -> Result<()> {
        parse_parameters::<HttpParamsConfig>(&config.parameters)?.validate()?;
        Ok(())
    }

    fn build(&self, config: &RuntimeChannelConfig) -> Result<BuiltProtocolRuntime> {
        let (runtime, interval) = create_http_channel(config.id(), config.name(), config)?;
        Ok(BuiltProtocolRuntime::new(runtime, interval))
    }
}

#[cfg(feature = "iec61850")]
struct Iec61850Factory;

#[cfg(feature = "iec61850")]
impl ProtocolAdapterFactory for Iec61850Factory {
    fn protocol_id(&self) -> &'static str {
        "iec61850"
    }

    fn validate(&self, config: &ChannelConfig) -> Result<()> {
        timing_parameter(&config.parameters, "connect_timeout_ms")?;
        timing_parameter(&config.parameters, "request_timeout_ms")?;
        poll_interval_ms(&config.parameters, 1_000).map(|_| ())
    }

    fn build(&self, config: &RuntimeChannelConfig) -> Result<BuiltProtocolRuntime> {
        use crate::protocols::adapters::iec61850::{Iec61850Channel, Iec61850ParamsConfig};
        use crate::protocols::core::point::{
            Iec61850Address, PointConfig, ProtocolAddress, TransformConfig,
        };

        let params = &config.base.parameters;
        let iec61850_params = Iec61850ParamsConfig {
            address: params
                .get("address")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("127.0.0.1:102")
                .to_owned(),
            connect_timeout_ms: timing_parameter(params, "connect_timeout_ms")?.unwrap_or(10_000),
            request_timeout_ms: timing_parameter(params, "request_timeout_ms")?.unwrap_or(5_000),
            reports: params
                .get("reports")
                .map(|value| serde_json::from_value(value.clone()))
                .transpose()
                .map_err(|error| IoError::config(format!("invalid IEC 61850 reports: {error}")))?
                .unwrap_or_default(),
        };

        let parse_address = |mapping: &Option<String>| -> Option<Iec61850Address> {
            let object: serde_json::Value = serde_json::from_str(mapping.as_deref()?).ok()?;
            let mut address = Iec61850Address::parse(object.get("address")?.as_str()?).ok()?;
            address.ctrl_model = object
                .get("ctrl_model")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(1) as u8;
            Some(address)
        };
        let mut points = Vec::<PointConfig>::new();

        for point in &config.telemetry_points {
            if let Some(address) = parse_address(&point.base.protocol_mappings) {
                points.push(PointConfig {
                    id: point.base.point_id,
                    point_type: common::PointType::Telemetry,
                    name: Some(point.base.signal_name.clone()),
                    address: ProtocolAddress::Iec61850(address),
                    transform: TransformConfig {
                        scale: point.scale,
                        offset: point.offset,
                        reverse: point.reverse,
                        ..Default::default()
                    },
                    poll_group: None,
                    enabled: true,
                });
            } else {
                tracing::warn!(
                    "Ch{} telemetry point {} has no valid IEC 61850 address in protocol_mappings",
                    config.id(),
                    point.base.point_id
                );
            }
        }
        for point in &config.signal_points {
            if let Some(address) = parse_address(&point.base.protocol_mappings) {
                points.push(PointConfig {
                    id: point.base.point_id,
                    point_type: common::PointType::Signal,
                    name: Some(point.base.signal_name.clone()),
                    address: ProtocolAddress::Iec61850(address),
                    transform: TransformConfig {
                        reverse: point.reverse,
                        ..Default::default()
                    },
                    poll_group: None,
                    enabled: true,
                });
            }
        }
        for point in &config.control_points {
            if let Some(address) = parse_address(&point.base.protocol_mappings) {
                points.push(PointConfig {
                    id: point.base.point_id,
                    point_type: common::PointType::Control,
                    name: Some(point.base.signal_name.clone()),
                    address: ProtocolAddress::Iec61850(address),
                    transform: TransformConfig {
                        reverse: point.reverse,
                        ..Default::default()
                    },
                    poll_group: None,
                    enabled: true,
                });
            } else {
                tracing::warn!(
                    "Ch{} control point {} has no valid IEC 61850 address in protocol_mappings",
                    config.id(),
                    point.base.point_id
                );
            }
        }
        for point in &config.adjustment_points {
            if let Some(address) = parse_address(&point.base.protocol_mappings) {
                points.push(PointConfig {
                    id: point.base.point_id,
                    point_type: common::PointType::Adjustment,
                    name: Some(point.base.signal_name.clone()),
                    address: ProtocolAddress::Iec61850(address),
                    transform: TransformConfig {
                        scale: point.scale,
                        offset: point.offset,
                        ..Default::default()
                    },
                    poll_group: None,
                    enabled: true,
                });
            } else {
                tracing::warn!(
                    "Ch{} adjustment point {} has no valid IEC 61850 address in protocol_mappings",
                    config.id(),
                    point.base.point_id
                );
            }
        }

        Ok(BuiltProtocolRuntime::new(
            Box::new(Iec61850Channel::new(
                config.id(),
                config.name(),
                &iec61850_params,
                points,
            )),
            poll_interval_ms(params, 1_000)?,
        ))
    }
}

/// Compose the exact protocol set linked into this binary.
///
/// Adding a protocol requires adding its Rust factory here and rebuilding the
/// IO binary. No runtime plugin discovery or dynamic loading is performed.
pub fn compiled_protocol_registry() -> Result<Arc<ProtocolRegistry>> {
    let factories: Vec<Arc<dyn ProtocolAdapterFactory>> = vec![
        #[cfg(feature = "modbus")]
        Arc::new(ModbusTcpFactory),
        #[cfg(feature = "modbus")]
        Arc::new(ModbusRtuFactory),
        #[cfg(all(target_os = "linux", feature = "gpio"))]
        Arc::new(GpioFactory),
        #[cfg(all(feature = "can", target_os = "linux"))]
        Arc::new(CanFactory),
        #[cfg(feature = "aether_485")]
        Arc::new(Aether485Factory),
        #[cfg(feature = "mqtt")]
        Arc::new(MqttFactory),
        #[cfg(feature = "http")]
        Arc::new(HttpFactory),
        #[cfg(feature = "iec61850")]
        Arc::new(Iec61850Factory),
    ];
    ProtocolRegistry::try_new(factories).map(Arc::new)
}

#[cfg(all(test, feature = "mqtt", feature = "http"))]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::core::config::{
        ChannelConfig, ChannelCore, ChannelLoggingConfig, RuntimeChannelConfig,
    };

    fn runtime(
        protocol: &str,
        parameters: HashMap<String, serde_json::Value>,
    ) -> RuntimeChannelConfig {
        RuntimeChannelConfig::from_base(ChannelConfig {
            core: ChannelCore {
                id: 7,
                name: format!("{protocol}-channel"),
                description: None,
                protocol: protocol.to_owned(),
                enabled: true,
            },
            parameters,
            logging: ChannelLoggingConfig::default(),
        })
    }

    #[test]
    fn distribution_json_protocols_have_concrete_channel_runtimes() {
        let registry = compiled_protocol_registry().expect("compiled protocol registry");
        let mqtt = runtime(
            "mqtt",
            HashMap::from([
                (
                    "broker".into(),
                    serde_json::json!("tcp://192.168.1.20:1883"),
                ),
                (
                    "subscriptions".into(),
                    serde_json::json!([{"topic": "device/telemetry", "qos": 1}]),
                ),
            ]),
        );
        let mqtt = registry.build(&mqtt).expect("MQTT runtime");
        assert_eq!(mqtt.runtime.protocol(), "mqtt");
        assert!(mqtt.runtime.is_event_driven());

        let http = runtime(
            "http",
            HashMap::from([
                ("url".into(), serde_json::json!("http://192.168.1.21/data")),
                ("poll_interval_ms".into(), serde_json::json!(2500)),
            ]),
        );
        let http = registry.build(&http).expect("HTTP runtime");
        assert_eq!(http.runtime.protocol(), "http");
        assert!(!http.runtime.is_event_driven());
        assert_eq!(http.poll_interval_ms, 2500);
    }
}
