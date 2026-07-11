//! Protocol client factory
//!
//! Create ChannelRuntime implementations from configuration.
//!
//! This module provides factory functions that create protocol client instances
//! (VirtualChannel, ModbusChannel, GpioChannel, CanClient) from io configuration.

use crate::protocols::adapters::virtual_channel::{VirtualChannel, VirtualChannelConfig};
use crate::protocols::core::point::PointConfig;
use crate::protocols::gateway::ChannelRuntime;

#[cfg(feature = "modbus")]
use crate::protocols::adapters::modbus::{ModbusChannel, ModbusChannelConfig, ReconnectConfig};

#[cfg(all(target_os = "linux", feature = "gpio"))]
use crate::protocols::adapters::gpio::{GpioChannel, GpioChannelConfig, GpioPinConfig};

#[cfg(all(feature = "can", target_os = "linux"))]
use crate::protocols::adapters::can::{CanClient, CanConfig, CanPoint};

#[cfg(all(target_os = "linux", feature = "gpio"))]
use crate::core::config::RuntimeChannelConfig;

#[cfg(feature = "aether_485")]
use crate::protocols::adapters::aether_485::{
    Aether485Channel, Aether485ChannelConfig, Aether485PointMapping, PollTarget,
};

// ============================================================================
// Virtual Channel Factory
// ============================================================================

/// Create a VirtualChannel that directly implements ChannelRuntime.
///
/// Note: The channel no longer holds a store reference. Storage is handled
/// by the service layer (ChannelManager) after polling.
pub fn create_virtual_channel(
    channel_id: u32,
    channel_name: &str,
    point_configs: Vec<PointConfig>,
) -> Box<dyn ChannelRuntime> {
    let config = VirtualChannelConfig::new(channel_name).with_points(point_configs);
    let channel = VirtualChannel::new(config, channel_id);

    // VirtualChannel now directly implements ChannelRuntime - no wrapper needed
    Box::new(channel)
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

    let mut config = ModbusChannelConfig::tcp(&address)
        .with_points(point_configs)
        .with_reconnect(ReconnectConfig::default());

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

    let mut config = ModbusChannelConfig::rtu(device, baud_rate)
        .with_points(point_configs)
        .with_reconnect(ReconnectConfig::default());

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
) -> Box<dyn ChannelRuntime> {
    use std::time::Duration;

    // Use sysfs driver - simpler and works directly with global GPIO numbers
    let mut gpio_config = GpioChannelConfig::new_sysfs("/sys/class/gpio");

    // Get poll interval from parameters
    if let Some(interval_ms) = runtime_config
        .base
        .parameters
        .get("poll_interval_ms")
        .and_then(|v| v.as_u64())
    {
        gpio_config = gpio_config.with_poll_interval(Duration::from_millis(interval_ms));
    }

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
    Box::new(channel)
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
    runtime_config: &crate::core::config::RuntimeChannelConfig,
) -> Box<dyn ChannelRuntime> {
    use aether_model::PointType;
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
