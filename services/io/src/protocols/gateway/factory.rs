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

    #[cfg(all(feature = "can", target_os = "linux"))]
    if protocol.eq_ignore_ascii_case("can") {
        return create_can_channel(config);
    }

    #[cfg(all(feature = "gpio", target_os = "linux"))]
    if protocol.eq_ignore_ascii_case("gpio") {
        return create_gpio_channel(config);
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

    #[cfg(feature = "iec61850")]
    if protocol.eq_ignore_ascii_case("iec61850") {
        return create_iec61850_channel(config);
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

#[cfg(feature = "iec61850")]
fn create_iec61850_channel(config: &ChannelConfig) -> Result<Box<dyn ChannelRuntime>> {
    use crate::protocols::adapters::iec61850::{Iec61850Channel, Iec61850ParamsConfig};

    let params: Iec61850ParamsConfig = serde_json::from_value(config.parameters.clone())
        .map_err(|e| GatewayError::Config(format!("Invalid IEC 61850 parameters: {}", e)))?;

    let points = build_point_configs(config)?;

    let channel = Iec61850Channel::new(config.id, config.name.clone(), &params, points);

    Ok(Box::new(channel))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocols::gateway::config::ChannelModeConfig;

    fn assert_protocol_is_unsupported(protocol: &str) {
        let config = ChannelConfig {
            id: 7,
            name: "unavailable protocol channel".to_string(),
            protocol: protocol.to_string(),
            enabled: false,
            mode: ChannelModeConfig::Polling,
            poll_interval_ms: None,
            parameters: serde_json::json!({}),
            points: Vec::new(),
        };

        let error = match create_channel(&config) {
            Ok(_) => panic!("unavailable protocol unexpectedly created a runtime"),
            Err(error) => error,
        };
        assert!(
            error
                .to_string()
                .contains(&format!("Unsupported protocol: {protocol}"))
        );
    }

    #[test]
    fn retired_virtual_protocol_is_rejected_explicitly() {
        assert_protocol_is_unsupported("virtual");
    }

    #[test]
    fn external_sunspec_plugin_is_rejected_when_absent() {
        assert_protocol_is_unsupported("sunspec_tcp");
        assert_protocol_is_unsupported("sunspec_rtu");
    }
}
