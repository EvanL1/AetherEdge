//! Protocol client factory
//!
//! Create ChannelRuntime implementations from configuration.
//!
//! This module provides factory functions that create physical protocol client
//! instances from IO configuration.

#[cfg(any(
    feature = "mqtt",
    feature = "http",
    all(target_os = "linux", feature = "gpio")
))]
use std::collections::HashMap;
#[cfg(any(feature = "mqtt", feature = "http"))]
use std::sync::Arc;

#[cfg(any(
    feature = "mqtt",
    feature = "http",
    all(target_os = "linux", feature = "gpio")
))]
use serde::de::DeserializeOwned;

use crate::core::config::ChannelConfig;
#[cfg(any(
    feature = "mqtt",
    feature = "http",
    all(target_os = "linux", feature = "gpio")
))]
use crate::protocols::core::error::GatewayError;
use crate::protocols::core::error::Result;
#[cfg(any(
    feature = "modbus",
    feature = "mqtt",
    feature = "http",
    feature = "aether_485",
    all(target_os = "linux", feature = "can"),
    all(target_os = "linux", feature = "gpio")
))]
use crate::protocols::gateway::ChannelRuntime;

#[cfg(feature = "modbus")]
use crate::protocols::adapters::modbus::{ModbusChannel, ModbusChannelConfig};
#[cfg(feature = "modbus")]
use crate::protocols::core::point::PointConfig;

#[cfg(all(target_os = "linux", feature = "gpio"))]
use crate::protocols::adapters::gpio::{GpioChannel, GpioChannelParamsConfig, GpioPinConfig};

#[cfg(all(feature = "can", target_os = "linux"))]
use crate::protocols::adapters::can::{CanClient, CanConfig, CanPoint};

#[cfg(any(
    feature = "aether_485",
    feature = "mqtt",
    feature = "http",
    all(target_os = "linux", feature = "gpio")
))]
use crate::core::config::RuntimeChannelConfig;

#[cfg(feature = "aether_485")]
use crate::protocols::adapters::aether_485::{
    Aether485Channel, Aether485ChannelConfig, Aether485PointMapping, PollTarget,
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
    all(target_os = "linux", feature = "gpio")
))]
fn parse_parameters<T: DeserializeOwned>(
    parameters: &HashMap<String, serde_json::Value>,
) -> Result<T> {
    serde_json::from_value(serde_json::to_value(parameters).map_err(|error| {
        GatewayError::Config(format!("Failed to encode channel parameters: {error}"))
    })?)
    .map_err(|error| GatewayError::Config(format!("Invalid channel parameters: {error}")))
}

#[cfg(feature = "mqtt")]
fn mqtt_parameters(parameters: &HashMap<String, serde_json::Value>) -> Result<MqttParamsConfig> {
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

/// Validate feature-selected protocol parameters before desired state commits.
pub(crate) fn validate_protocol_parameters(config: &ChannelConfig) -> Result<()> {
    match crate::utils::normalize_protocol_name(config.protocol()).as_ref() {
        #[cfg(feature = "mqtt")]
        "mqtt" => mqtt_parameters(&config.parameters).map(|_| ()),
        #[cfg(feature = "http")]
        "http" => parse_parameters::<HttpParamsConfig>(&config.parameters)?.validate(),
        #[cfg(all(target_os = "linux", feature = "gpio"))]
        "gpio" | "di_do" | "dido" => {
            parse_parameters::<GpioChannelParamsConfig>(&config.parameters)?
                .to_config()
                .map(|_| ())
        },
        _ => Ok(()),
    }
}

#[cfg(any(feature = "mqtt", feature = "http"))]
fn compile_json_mapper(
    runtime_config: &RuntimeChannelConfig,
    mapping_config: &JsonMappingConfig,
) -> Result<Arc<JsonMapper>> {
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
pub fn create_modbus_channel(
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
pub fn create_modbus_rtu_channel(
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
pub fn create_gpio_channel(
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
pub fn create_can_channel(
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
pub fn create_mqtt_channel(
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
pub fn create_http_channel(
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
pub fn create_aether_485_channel(
    channel_id: u32,
    channel_name: &str,
    params: &std::collections::HashMap<String, serde_json::Value>,
    runtime_config: &RuntimeChannelConfig,
) -> Box<dyn ChannelRuntime> {
    use common::PointType;
    use std::time::Duration;

    let device = params
        .get("device")
        .and_then(|v| v.as_str())
        .unwrap_or("/dev/ttyAP0");
    let baud_rate = params
        .get("baud_rate")
        .and_then(|v| v.as_u64())
        .map(|n| n as u32)
        .unwrap_or(115_200);
    let timeout_ms = params
        .get("timeout_ms")
        .and_then(|v| v.as_u64())
        .unwrap_or(1000);
    let retry_count = params
        .get("retry_count")
        .and_then(|v| v.as_u64())
        .map(|n| n as u32)
        .unwrap_or(2);
    let frame_delay_ms = params
        .get("frame_delay_ms")
        .and_then(|v| v.as_u64())
        .unwrap_or(50);

    let config = Aether485ChannelConfig {
        device: device.to_string(),
        baud_rate,
        io_timeout: Duration::from_millis(timeout_ms),
        retry_count,
        frame_delay: Duration::from_millis(frame_delay_ms),
    };

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

    Box::new(Aether485Channel::new(config, channel_id, name, targets))
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
        let mqtt = create_mqtt_channel(7, "mqtt-channel", &mqtt).expect("MQTT runtime");
        assert_eq!(mqtt.protocol(), "mqtt");
        assert!(mqtt.is_event_driven());

        let http = runtime(
            "http",
            HashMap::from([
                ("url".into(), serde_json::json!("http://192.168.1.21/data")),
                ("poll_interval_ms".into(), serde_json::json!(2500)),
            ]),
        );
        let (http, interval_ms) =
            create_http_channel(7, "http-channel", &http).expect("HTTP runtime");
        assert_eq!(http.protocol(), "http");
        assert!(!http.is_event_driven());
        assert_eq!(interval_ms, 2500);
    }
}
