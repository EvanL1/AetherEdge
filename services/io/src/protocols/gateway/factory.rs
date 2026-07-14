//! Channel factory.
//!
//! Creates `ChannelRuntime` instances from configuration.

use crate::protocols::core::error::{GatewayError, Result};
use crate::protocols::core::point::PointConfig;

use super::config::ChannelConfig;
use super::parse_address;
use super::runtime::ChannelRuntime;

/// Create a channel from configuration.
pub fn create_channel(config: &ChannelConfig) -> Result<Box<dyn ChannelRuntime>> {
    let protocol = &config.protocol;

    // Use eq_ignore_ascii_case to avoid String allocation from to_lowercase()
    #[cfg(feature = "modbus")]
    if protocol.eq_ignore_ascii_case("modbus") {
        return create_modbus_channel(config);
    }

    #[cfg(feature = "iec104")]
    if protocol.eq_ignore_ascii_case("iec104") {
        return create_iec104_channel(config);
    }

    #[cfg(feature = "opcua")]
    if protocol.eq_ignore_ascii_case("opcua") {
        return create_opcua_channel(config);
    }

    #[cfg(all(feature = "can", target_os = "linux"))]
    if protocol.eq_ignore_ascii_case("can") {
        return create_can_channel(config);
    }

    #[cfg(all(feature = "gpio", target_os = "linux"))]
    if protocol.eq_ignore_ascii_case("gpio") {
        return create_gpio_channel(config);
    }

    #[cfg(feature = "dl645")]
    if protocol.eq_ignore_ascii_case("dl645") {
        return create_dl645_channel(config);
    }

    #[cfg(feature = "aether_485")]
    if protocol.eq_ignore_ascii_case("aether_485") {
        return create_aether_485_channel(config);
    }

    #[cfg(feature = "mqtt")]
    if protocol.eq_ignore_ascii_case("mqtt") {
        return create_mqtt_channel(config);
    }

    #[cfg(feature = "http")]
    if protocol.eq_ignore_ascii_case("http") {
        return create_http_channel(config);
    }

    #[cfg(feature = "ble")]
    if protocol.eq_ignore_ascii_case("ble") {
        return create_ble_channel(config);
    }

    #[cfg(feature = "zigbee")]
    if protocol.eq_ignore_ascii_case("zigbee") {
        return create_zigbee_channel(config);
    }

    #[cfg(feature = "matter")]
    if protocol.eq_ignore_ascii_case("matter") {
        return create_matter_channel(config);
    }

    #[cfg(feature = "iec61850")]
    if protocol.eq_ignore_ascii_case("iec61850") {
        return create_iec61850_channel(config);
    }

    if protocol.eq_ignore_ascii_case("virtual") {
        return create_virtual_channel(config);
    }

    Err(GatewayError::Config(format!(
        "Unsupported protocol: {}. Check if the required feature is enabled.",
        protocol
    )))
}

/// Convert PointDef list to PointConfig list.
fn build_point_configs(config: &ChannelConfig) -> Result<Vec<PointConfig>> {
    // Pre-allocate with upper bound (some points may be disabled)
    let mut points = Vec::with_capacity(config.points.len());

    for point_def in &config.points {
        if !point_def.enabled {
            continue;
        }

        let address = parse_address(&config.protocol, &point_def.address)?;

        points.push(PointConfig {
            id: point_def.id,
            point_type: point_def.point_type,
            name: Some(point_def.name.clone()),
            address,
            transform: point_def.transform.clone(),
            poll_group: None,
            enabled: true,
        });
    }

    Ok(points)
}

// ============================================================================
// Protocol-specific channel creators
// ============================================================================

#[cfg(feature = "modbus")]
fn create_modbus_channel(config: &ChannelConfig) -> Result<Box<dyn ChannelRuntime>> {
    use crate::protocols::adapters::modbus::ModbusChannelParamsConfig;

    // Parse parameters
    let params: ModbusChannelParamsConfig = serde_json::from_value(config.parameters.clone())
        .map_err(|e| GatewayError::Config(format!("Invalid Modbus parameters: {}", e)))?;

    // Build channel config
    let channel_config = params.to_channel_config();

    // Build point configs
    let points = build_point_configs(config)?;
    let channel_config = channel_config.with_points(points);

    // ModbusChannel directly implements ChannelRuntime - no wrapper needed
    let channel = crate::protocols::adapters::modbus::ModbusChannel::new(
        channel_config,
        config.id,
        config.name.clone(),
    );

    Ok(Box::new(channel))
}

#[cfg(feature = "iec104")]
fn create_iec104_channel(config: &ChannelConfig) -> Result<Box<dyn ChannelRuntime>> {
    use crate::protocols::adapters::iec104::Iec104ParamsConfig;

    // Parse parameters
    let params: Iec104ParamsConfig = serde_json::from_value(config.parameters.clone())
        .map_err(|e| GatewayError::Config(format!("Invalid IEC104 parameters: {}", e)))?;

    // Build point configs
    let points = build_point_configs(config)?;

    // Build channel config
    let channel_config = params.to_config().with_points(points);

    // Iec104Channel directly implements ChannelRuntime - no wrapper needed
    let channel = crate::protocols::adapters::iec104::Iec104Channel::new(
        channel_config,
        config.id,
        config.name.clone(),
    );

    Ok(Box::new(channel))
}

#[cfg(feature = "opcua")]
fn create_opcua_channel(config: &ChannelConfig) -> Result<Box<dyn ChannelRuntime>> {
    use crate::protocols::adapters::opcua::OpcUaParamsConfig;

    // Parse parameters
    let params: OpcUaParamsConfig = serde_json::from_value(config.parameters.clone())
        .map_err(|e| GatewayError::Config(format!("Invalid OPC UA parameters: {}", e)))?;

    // Build point configs
    let points = build_point_configs(config)?;

    // Build channel config
    let channel_config = params.to_config().with_points(points);

    // OpcUaChannel directly implements ChannelRuntime - no wrapper needed
    let channel = crate::protocols::adapters::opcua::OpcUaChannel::new(
        channel_config,
        config.id,
        config.name.clone(),
    );

    Ok(Box::new(channel))
}

#[cfg(all(feature = "can", target_os = "linux"))]
fn create_can_channel(config: &ChannelConfig) -> Result<Box<dyn ChannelRuntime>> {
    use crate::protocols::adapters::can::CanChannelParamsConfig;

    // Parse parameters
    let params: CanChannelParamsConfig = serde_json::from_value(config.parameters.clone())
        .map_err(|e| GatewayError::Config(format!("Invalid CAN parameters: {}", e)))?;

    // Build channel config
    let channel_config = params.to_config();

    // CanClient directly implements ChannelRuntime - no wrapper needed
    let channel = crate::protocols::adapters::can::CanClient::new(
        channel_config,
        config.id,
        config.name.clone(),
    );

    Ok(Box::new(channel))
}

#[cfg(all(feature = "gpio", target_os = "linux"))]
fn create_gpio_channel(config: &ChannelConfig) -> Result<Box<dyn ChannelRuntime>> {
    use crate::protocols::adapters::gpio::GpioChannelParamsConfig;

    // Parse parameters
    let params: GpioChannelParamsConfig = serde_json::from_value(config.parameters.clone())
        .map_err(|e| GatewayError::Config(format!("Invalid GPIO parameters: {}", e)))?;

    // Build channel config
    let channel_config = params.to_config();

    // GpioChannel directly implements ChannelRuntime - no wrapper needed
    let channel = crate::protocols::adapters::gpio::GpioChannel::new(
        channel_config,
        config.id,
        config.name.clone(),
    );

    Ok(Box::new(channel))
}

#[cfg(feature = "dl645")]
fn create_dl645_channel(config: &ChannelConfig) -> Result<Box<dyn ChannelRuntime>> {
    use crate::protocols::adapters::dl645::Dl645ChannelParamsConfig;

    // Parse parameters
    let params: Dl645ChannelParamsConfig = serde_json::from_value(config.parameters.clone())
        .map_err(|e| GatewayError::Config(format!("Invalid DL/T 645 parameters: {}", e)))?;

    // Build channel config (no point config needed - uses hardcoded STANDARD_POINTS)
    let channel_config = params.to_channel_config();

    let channel = crate::protocols::adapters::dl645::Dl645Channel::new(
        channel_config,
        config.id,
        config.name.clone(),
    );

    Ok(Box::new(channel))
}

#[cfg(feature = "mqtt")]
fn create_mqtt_channel(config: &ChannelConfig) -> Result<Box<dyn ChannelRuntime>> {
    use crate::protocols::adapters::mqtt::MqttParamsConfig;

    // Parse parameters
    let params: MqttParamsConfig = serde_json::from_value(config.parameters.clone())
        .map_err(|e| GatewayError::Config(format!("Invalid MQTT parameters: {}", e)))?;

    // Build channel config
    let channel_config = params.to_config();

    // MqttChannel loads JSON mappings from point-owned inline protocol_mappings.
    let channel = crate::protocols::adapters::mqtt::MqttChannel::new(
        channel_config,
        config.id,
        config.name.clone(),
    );

    Ok(Box::new(channel))
}

#[cfg(feature = "http")]
fn create_http_channel(config: &ChannelConfig) -> Result<Box<dyn ChannelRuntime>> {
    use crate::protocols::adapters::http::HttpParamsConfig;

    // Parse parameters
    let params: HttpParamsConfig = serde_json::from_value(config.parameters.clone())
        .map_err(|e| GatewayError::Config(format!("Invalid HTTP parameters: {}", e)))?;

    // Build channel config
    let channel_config = params.to_config();

    // HttpChannel loads JSON mappings from point-owned inline protocol_mappings.
    let channel = crate::protocols::adapters::http::HttpChannel::new(
        channel_config,
        config.id,
        config.name.clone(),
    );

    Ok(Box::new(channel))
}

#[cfg(feature = "aether_485")]
fn create_aether_485_channel(config: &ChannelConfig) -> Result<Box<dyn ChannelRuntime>> {
    use crate::protocols::adapters::aether_485::Aether485ParamsConfig;

    let params: Aether485ParamsConfig = serde_json::from_value(config.parameters.clone())
        .map_err(|e| GatewayError::Config(format!("Invalid Aether-485 parameters: {}", e)))?;

    let channel_config = params.to_channel_config();

    // Gateway factory path does not have RuntimeChannelConfig, so we create
    // with an empty poll_targets list. Points will need to be configured
    // via the main channel_creation path instead.
    let channel = crate::protocols::adapters::aether_485::Aether485Channel::new(
        channel_config,
        config.id,
        config.name.clone(),
        Vec::new(),
    );

    Ok(Box::new(channel))
}

#[cfg(feature = "ble")]
fn create_ble_channel(config: &ChannelConfig) -> Result<Box<dyn ChannelRuntime>> {
    use crate::protocols::adapters::ble_config::BleParamsConfig;

    // Parse parameters
    let params: BleParamsConfig = serde_json::from_value(config.parameters.clone())
        .map_err(|e| GatewayError::Config(format!("Invalid BLE parameters: {}", e)))?;

    // Build runtime config
    let ble_config = params.to_config();

    // Build point configs
    let points = build_point_configs(config)?;

    let channel = crate::protocols::adapters::ble::BleChannel::new(
        ble_config,
        config.id,
        config.name.clone(),
        points,
    );

    Ok(Box::new(channel))
}

#[cfg(feature = "zigbee")]
fn create_zigbee_channel(config: &ChannelConfig) -> Result<Box<dyn ChannelRuntime>> {
    use crate::protocols::adapters::zigbee_config::ZigbeeParamsConfig;

    // Parse parameters
    let params: ZigbeeParamsConfig = serde_json::from_value(config.parameters.clone())
        .map_err(|e| GatewayError::Config(format!("Invalid Zigbee parameters: {}", e)))?;

    // Build runtime config
    let zigbee_config = params.to_config();

    // Build point configs
    let points = build_point_configs(config)?;

    let channel = crate::protocols::adapters::zigbee::ZigbeeChannel::new(
        zigbee_config,
        config.id,
        config.name.clone(),
        points,
    );

    Ok(Box::new(channel))
}

#[cfg(feature = "matter")]
fn create_matter_channel(config: &ChannelConfig) -> Result<Box<dyn ChannelRuntime>> {
    use crate::protocols::adapters::matter_config::MatterParamsConfig;

    // Parse parameters
    let params: MatterParamsConfig = serde_json::from_value(config.parameters.clone())
        .map_err(|e| GatewayError::Config(format!("Invalid Matter parameters: {}", e)))?;

    // Build channel config
    let matter_config = params.to_config();

    // Build point configs
    let points = build_point_configs(config)?;

    // MatterChannel is event-driven with per-point Matter addresses
    let channel = crate::protocols::adapters::matter::MatterChannel::new(
        matter_config,
        config.id,
        config.name.clone(),
    )
    .with_points(points);

    Ok(Box::new(channel))
}

#[cfg(feature = "iec61850")]
fn create_iec61850_channel(config: &ChannelConfig) -> Result<Box<dyn ChannelRuntime>> {
    use crate::protocols::adapters::iec61850::{Iec61850Channel, Iec61850ParamsConfig};

    let params: Iec61850ParamsConfig = serde_json::from_value(config.parameters.clone())
        .map_err(|e| GatewayError::Config(format!("Invalid IEC 61850 parameters: {}", e)))?;

    let points = build_point_configs(config)?;

    let channel = Iec61850Channel::new(config.id, config.name.clone(), &params, points);

    Ok(Box::new(channel))
}

fn create_virtual_channel(config: &ChannelConfig) -> Result<Box<dyn ChannelRuntime>> {
    use crate::protocols::adapters::virtual_channel::{VirtualChannel, VirtualChannelParamsConfig};

    // Parse parameters (optional for virtual)
    let params: VirtualChannelParamsConfig =
        serde_json::from_value(config.parameters.clone()).unwrap_or_default();

    // Build point configs
    let points = build_point_configs(config)?;

    // Build channel config
    let channel_config = params.to_config().with_points(points);

    // VirtualChannel now directly implements ChannelRuntime - no wrapper needed
    let channel = VirtualChannel::new(channel_config, config.id);

    Ok(Box::new(channel))
}
