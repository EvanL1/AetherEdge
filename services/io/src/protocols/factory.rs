//! Compile-time protocol composition for the IO service.
//!
//! This is the single registry for discovery metadata, channel parameter
//! validation, point-mapping validation, and runtime construction. Entries use
//! synchronous function pointers: protocol clients are compiled during channel
//! activation, while device/network I/O remains owned by the unified runtime
//! task.

use std::collections::HashMap;
use std::sync::LazyLock;

use aether_config::io::MAX_CHANNEL_TIMING_MS;
use aether_core::PointType;
use common::{ValidationLevel, ValidationResult};
use serde_json::{Map, Value};

use crate::core::channels::RuntimeChannelConfig;
use crate::core::channels::runtime_policy::{ChannelRuntimePolicy, RUNTIME_PARAMETER_KEYS};
use crate::core::config::{ChannelConfig, Point};
use crate::error::{IoError, Result};
use crate::protocols::core::metadata::{HasMetadata, ProtocolMetadata};
use crate::protocols::runtime::ChannelRuntime;

type ParameterValidator = fn(&ChannelConfig) -> Result<()>;
type MappingValidator = fn(PointType, u32, &Value) -> Result<()>;
type RuntimeBuilder = fn(&RuntimeChannelConfig) -> Result<Box<dyn ChannelRuntime>>;

/// One statically composed production protocol.
///
/// Function pointers avoid a second trait-object allocation. The only dynamic
/// object retained at runtime is the heterogeneous [`ChannelRuntime`] itself.
pub(crate) struct ProtocolFactory {
    metadata: ProtocolMetadata,
    aliases: &'static [&'static str],
    default_poll_interval_ms: u64,
    numeric_mapping_fields: &'static [&'static str],
    validate_parameters: ParameterValidator,
    validate_mapping: MappingValidator,
    build_runtime: RuntimeBuilder,
}

impl ProtocolFactory {
    fn new(
        mut metadata: ProtocolMetadata,
        aliases: &'static [&'static str],
        default_poll_interval_ms: u64,
        numeric_mapping_fields: &'static [&'static str],
        validate_parameters: ParameterValidator,
        validate_mapping: MappingValidator,
        build_runtime: RuntimeBuilder,
    ) -> Self {
        for driver in &mut metadata.drivers {
            driver
                .parameters
                .retain(|parameter| !RUNTIME_PARAMETER_KEYS.contains(&parameter.name));
            driver
                .parameters
                .extend(ChannelRuntimePolicy::parameter_metadata(
                    default_poll_interval_ms,
                ));
        }
        Self {
            metadata,
            aliases,
            default_poll_interval_ms,
            numeric_mapping_fields,
            validate_parameters,
            validate_mapping,
            build_runtime,
        }
    }

    fn matches(&self, protocol: &str) -> bool {
        normalized_eq(protocol, self.metadata.protocol_type)
            || normalized_eq(protocol, self.metadata.name)
            || self
                .aliases
                .iter()
                .any(|alias| normalized_eq(protocol, alias))
    }

    pub(crate) fn metadata(&self) -> &ProtocolMetadata {
        &self.metadata
    }

    pub(crate) fn default_poll_interval_ms(&self) -> u64 {
        self.default_poll_interval_ms
    }

    pub(crate) fn numeric_mapping_fields(&self) -> &'static [&'static str] {
        self.numeric_mapping_fields
    }

    pub(crate) fn validate_channel(&self, config: &ChannelConfig) -> Result<()> {
        validate_common_channel(config)?;
        ChannelRuntimePolicy::compile(&config.parameters, self.default_poll_interval_ms)?;
        (self.validate_parameters)(config)
    }

    pub(crate) fn validate_point_mapping(
        &self,
        point_type: PointType,
        point_id: u32,
        mapping: &Value,
    ) -> Result<()> {
        if mapping.is_null() || mapping.as_object().is_some_and(Map::is_empty) {
            return Ok(());
        }
        (self.validate_mapping)(point_type, point_id, mapping)
    }

    pub(crate) fn build(&self, config: &RuntimeChannelConfig) -> Result<Box<dyn ChannelRuntime>> {
        // Startup/reconciliation may consume persisted state that did not pass
        // the current command boundary, so validation remains mandatory.
        validate_common_channel(config.channel_config())?;
        (self.build_runtime)(config)
    }
}

/// Registry of the exact protocol factories compiled into this IO binary.
pub struct ProtocolRegistry {
    factories: Vec<ProtocolFactory>,
}

impl ProtocolRegistry {
    fn new() -> Self {
        Self {
            factories: Vec::new(),
        }
    }

    fn register(&mut self, factory: ProtocolFactory) {
        self.factories.push(factory);
    }

    /// Iterate discovery metadata without cloning it into a second registry.
    pub fn protocols(&self) -> impl ExactSizeIterator<Item = &ProtocolMetadata> {
        self.factories.iter().map(ProtocolFactory::metadata)
    }

    /// Resolve a canonical identifier or compatibility alias.
    #[must_use]
    pub fn resolve(&self, protocol: &str) -> Option<&ProtocolMetadata> {
        self.factory(protocol).map(ProtocolFactory::metadata)
    }

    pub(crate) fn factory(&self, protocol: &str) -> Option<&ProtocolFactory> {
        self.factories
            .iter()
            .find(|factory| factory.matches(protocol))
    }
}

fn normalized_eq(left: &str, right: &str) -> bool {
    fn normalized(value: &str) -> impl Iterator<Item = u8> + '_ {
        value
            .trim()
            .bytes()
            .filter(|byte| !matches!(byte, b'-' | b'_' | b' ' | b'.'))
            .map(|byte| byte.to_ascii_lowercase())
    }
    normalized(left).eq(normalized(right))
}

fn validate_common_channel(config: &ChannelConfig) -> Result<()> {
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

/// Strictly decode the adapter-owned part of a flat channel parameter object.
///
/// `MapDeserializer` borrows JSON values directly, avoiding the previous
/// `HashMap -> Value -> typed DTO` deep clone.
fn decode_parameters<T>(parameters: &HashMap<String, Value>, protocol: &str) -> Result<T>
where
    T: serde::de::DeserializeOwned,
{
    let entries = parameters
        .iter()
        .filter(|(key, _)| !RUNTIME_PARAMETER_KEYS.contains(&key.as_str()))
        .map(|(key, value)| (key.as_str(), value));
    let deserializer = serde::de::value::MapDeserializer::<_, serde_json::Error>::new(entries);
    T::deserialize(deserializer)
        .map_err(|error| IoError::config(format!("invalid {protocol} parameters: {error}")))
}

fn required_string_parameter<'a>(
    parameters: &'a HashMap<String, Value>,
    parameter: &str,
) -> Result<&'a str> {
    parameters
        .get(parameter)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| IoError::config(format!("'{parameter}' must be a non-empty string")))
}

fn required_u16_parameter(parameters: &HashMap<String, Value>, parameter: &str) -> Result<u16> {
    let value = required_positive_u64(parameters, parameter)?;
    u16::try_from(value)
        .map_err(|_| IoError::config(format!("'{parameter}' exceeds the u16 range")))
}

fn required_u32_parameter(parameters: &HashMap<String, Value>, parameter: &str) -> Result<u32> {
    let value = required_positive_u64(parameters, parameter)?;
    u32::try_from(value)
        .map_err(|_| IoError::config(format!("'{parameter}' exceeds the u32 range")))
}

fn required_positive_u64(parameters: &HashMap<String, Value>, parameter: &str) -> Result<u64> {
    parameters
        .get(parameter)
        .and_then(Value::as_u64)
        .filter(|value| *value > 0)
        .ok_or_else(|| IoError::config(format!("'{parameter}' must be a positive integer")))
}

fn timing_parameter(parameters: &HashMap<String, Value>, parameter: &str) -> Result<Option<u64>> {
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

fn runtime_mapping(point: &Point) -> Result<Option<Value>> {
    let Some(raw_mapping) = point.protocol_mappings.as_deref() else {
        return Ok(None);
    };
    let mapping: Value = serde_json::from_str(raw_mapping)
        .map_err(|error| invalid_mapping(point.point_id, &error.to_string()))?;
    if mapping.is_null() || mapping.as_object().is_some_and(Map::is_empty) {
        Ok(None)
    } else {
        Ok(Some(mapping))
    }
}

#[cfg(any(
    feature = "dl645",
    feature = "cjt188",
    feature = "iec101",
    feature = "gb32960",
    feature = "jt808",
    all(feature = "j1939", target_os = "linux"),
    all(feature = "gpio", target_os = "linux"),
))]
fn reject_mapped_points<'a>(
    protocol: &str,
    point_type: PointType,
    points: impl Iterator<Item = &'a Point>,
) -> Result<()> {
    for point in points {
        if runtime_mapping(point)?.is_some() {
            return Err(invalid_mapping(
                point.point_id,
                &format!("{protocol} does not support {point_type:?} mappings"),
            ));
        }
    }
    Ok(())
}

#[cfg(feature = "modbus")]
fn validate_modbus_tcp_parameters(config: &ChannelConfig) -> Result<()> {
    required_string_parameter(&config.parameters, "host")?;
    required_u16_parameter(&config.parameters, "port")?;
    timing_parameter(&config.parameters, "read_timeout_ms")?;
    reject_unknown_parameters(config, &["host", "port", "read_timeout_ms"])
}

#[cfg(feature = "modbus")]
fn validate_modbus_rtu_parameters(config: &ChannelConfig) -> Result<()> {
    required_string_parameter(&config.parameters, "device")?;
    required_u32_parameter(&config.parameters, "baud_rate")?;
    timing_parameter(&config.parameters, "read_timeout_ms")?;
    reject_unknown_parameters(config, &["device", "baud_rate", "read_timeout_ms"])
}

fn reject_unknown_parameters(config: &ChannelConfig, adapter_keys: &[&str]) -> Result<()> {
    if let Some(key) = config.parameters.keys().find(|key| {
        !adapter_keys.contains(&key.as_str()) && !RUNTIME_PARAMETER_KEYS.contains(&key.as_str())
    }) {
        return Err(IoError::config(format!(
            "unknown {} parameter '{key}'",
            config.protocol()
        )));
    }
    Ok(())
}

#[cfg(feature = "mqtt")]
fn validate_mqtt_parameters(config: &ChannelConfig) -> Result<()> {
    let parameters: crate::protocols::adapters::mqtt::MqttParamsConfig =
        decode_parameters(&config.parameters, "MQTT")?;
    parameters.validate().map_err(Into::into)
}

#[cfg(feature = "http")]
fn validate_http_parameters(config: &ChannelConfig) -> Result<()> {
    let parameters: crate::protocols::adapters::http::HttpParamsConfig =
        decode_parameters(&config.parameters, "HTTP")?;
    parameters.validate().map_err(Into::into)
}

#[cfg(feature = "ble")]
fn validate_ble_parameters(config: &ChannelConfig) -> Result<()> {
    let parameters: crate::protocols::adapters::ble_config::BleParamsConfig =
        decode_parameters(&config.parameters, "BLE")?;
    parameters.into_config().map(|_| ()).map_err(Into::into)
}

#[cfg(feature = "zigbee")]
fn validate_zigbee_parameters(config: &ChannelConfig) -> Result<()> {
    let parameters: crate::protocols::adapters::zigbee_config::ZigbeeParamsConfig =
        decode_parameters(&config.parameters, "Zigbee")?;
    parameters.into_config().map(|_| ()).map_err(Into::into)
}

#[cfg(all(feature = "j1939", target_os = "linux"))]
fn validate_j1939_parameters(config: &ChannelConfig) -> Result<()> {
    let parameters: crate::protocols::adapters::can::J1939Config =
        decode_parameters(&config.parameters, "J1939")?;
    parameters.validate().map_err(Into::into)
}

#[cfg(feature = "iec104")]
fn validate_iec104_parameters(config: &ChannelConfig) -> Result<()> {
    let parameters: crate::protocols::adapters::iec104::Iec104ParamsConfig =
        decode_parameters(&config.parameters, "IEC 104")?;
    parameters.validate().map_err(Into::into)
}

#[cfg(feature = "opcua")]
fn validate_opcua_parameters(config: &ChannelConfig) -> Result<()> {
    let parameters: crate::protocols::adapters::opcua::OpcUaParamsConfig =
        decode_parameters(&config.parameters, "OPC UA")?;
    parameters.validate().map_err(Into::into)
}

#[cfg(feature = "dl645")]
fn validate_dl645_parameters(config: &ChannelConfig) -> Result<()> {
    let parameters: crate::protocols::adapters::dl645::Dl645ChannelParamsConfig =
        decode_parameters(&config.parameters, "DL/T 645")?;
    parameters.validate().map_err(Into::into)
}

#[cfg(feature = "bacnet")]
fn validate_bacnet_parameters(config: &ChannelConfig) -> Result<()> {
    let parameters: crate::protocols::adapters::bacnet::BacnetParamsConfig =
        decode_parameters(&config.parameters, "BACnet/IP")?;
    parameters.validate().map_err(Into::into)
}

#[cfg(feature = "cjt188")]
fn validate_cjt188_parameters(config: &ChannelConfig) -> Result<()> {
    let parameters: crate::protocols::adapters::cjt188::Cjt188ParamsConfig =
        decode_parameters(&config.parameters, "CJ/T 188")?;
    parameters.validate().map_err(Into::into)
}

#[cfg(feature = "iec101")]
fn validate_iec101_parameters(config: &ChannelConfig) -> Result<()> {
    let parameters: crate::protocols::adapters::iec101::Iec101ParamsConfig =
        decode_parameters(&config.parameters, "IEC 101")?;
    parameters.validate().map_err(Into::into)
}

#[cfg(feature = "gb32960")]
fn validate_gb32960_parameters(config: &ChannelConfig) -> Result<()> {
    let parameters: crate::protocols::adapters::gb32960::Gb32960ParamsConfig =
        decode_parameters(&config.parameters, "GB/T 32960")?;
    parameters.validate().map_err(Into::into)
}

#[cfg(feature = "jt808")]
fn validate_jt808_parameters(config: &ChannelConfig) -> Result<()> {
    let parameters: crate::protocols::adapters::jt808::Jt808ParamsConfig =
        decode_parameters(&config.parameters, "JT/T 808")?;
    parameters.validate().map_err(Into::into)
}

#[cfg(all(feature = "gpio", target_os = "linux"))]
fn validate_gpio_parameters(config: &ChannelConfig) -> Result<()> {
    let parameters: crate::protocols::adapters::gpio::GpioChannelParamsConfig =
        decode_parameters(&config.parameters, "GPIO")?;
    parameters.validate().map_err(Into::into)
}

#[cfg(all(feature = "can", target_os = "linux"))]
fn validate_can_parameters(config: &ChannelConfig) -> Result<()> {
    let parameters: crate::protocols::adapters::can::CanChannelParamsConfig =
        decode_parameters(&config.parameters, "CAN")?;
    parameters.validate().map_err(Into::into)
}

#[cfg(feature = "iec61850")]
fn validate_iec61850_parameters(config: &ChannelConfig) -> Result<()> {
    let parameters: crate::protocols::adapters::iec61850::Iec61850ParamsConfig =
        decode_parameters(&config.parameters, "IEC 61850")?;
    parameters.validate().map_err(Into::into)
}

#[cfg(feature = "modbus")]
fn build_modbus_tcp(config: &RuntimeChannelConfig) -> Result<Box<dyn ChannelRuntime>> {
    use std::time::Duration;

    use crate::core::channels::converters::convert_to_modbus_point_configs;
    use crate::protocols::adapters::modbus::{ModbusChannel, ModbusChannelConfig, ReconnectConfig};

    validate_modbus_tcp_parameters(config.channel_config())?;
    let parameters = &config.channel_config().parameters;
    let host = required_string_parameter(parameters, "host")?;
    let port = required_u16_parameter(parameters, "port")?;
    let timeout = timing_parameter(parameters, "read_timeout_ms")?;
    let address = format!("{host}:{port}");
    let mut channel_config = ModbusChannelConfig::tcp(&address)
        .with_points(convert_to_modbus_point_configs(config)?)
        .with_reconnect(ReconnectConfig::default());
    if let Some(timeout) = timeout {
        channel_config = channel_config.with_io_timeout(Duration::from_millis(timeout));
    }
    Ok(Box::new(ModbusChannel::new(channel_config, config.id())))
}

#[cfg(feature = "modbus")]
fn build_modbus_rtu(config: &RuntimeChannelConfig) -> Result<Box<dyn ChannelRuntime>> {
    use std::time::Duration;

    use crate::core::channels::converters::convert_to_modbus_point_configs;
    use crate::protocols::adapters::modbus::{ModbusChannel, ModbusChannelConfig, ReconnectConfig};

    validate_modbus_rtu_parameters(config.channel_config())?;
    let parameters = &config.channel_config().parameters;
    let device = required_string_parameter(parameters, "device")?;
    let baud_rate = required_u32_parameter(parameters, "baud_rate")?;
    let timeout = timing_parameter(parameters, "read_timeout_ms")?;
    let mut channel_config = ModbusChannelConfig::rtu(device, baud_rate)
        .with_points(convert_to_modbus_point_configs(config)?)
        .with_reconnect(ReconnectConfig::default());
    if let Some(timeout) = timeout {
        channel_config = channel_config.with_io_timeout(Duration::from_millis(timeout));
    }
    Ok(Box::new(ModbusChannel::new(channel_config, config.id())))
}

#[cfg(feature = "mqtt")]
fn build_mqtt(config: &RuntimeChannelConfig) -> Result<Box<dyn ChannelRuntime>> {
    use crate::protocols::adapters::mqtt::{MqttChannel, MqttParamsConfig};

    let parameters: MqttParamsConfig =
        decode_parameters(&config.channel_config().parameters, "MQTT")?;
    Ok(Box::new(MqttChannel::new(parameters, config)?))
}

#[cfg(feature = "http")]
fn build_http(config: &RuntimeChannelConfig) -> Result<Box<dyn ChannelRuntime>> {
    use crate::protocols::adapters::http::{HttpChannel, HttpParamsConfig};

    let parameters: HttpParamsConfig =
        decode_parameters(&config.channel_config().parameters, "HTTP")?;
    Ok(Box::new(HttpChannel::new(parameters, config)?))
}

#[cfg(feature = "ble")]
fn build_ble(config: &RuntimeChannelConfig) -> Result<Box<dyn ChannelRuntime>> {
    use crate::core::channels::converters::convert_to_ble_point_configs;
    use crate::protocols::adapters::ble::BleChannel;
    use crate::protocols::adapters::ble_config::BleParamsConfig;

    let parameters: BleParamsConfig =
        decode_parameters(&config.channel_config().parameters, "BLE")?;
    Ok(Box::new(BleChannel::new(
        parameters.into_config()?,
        config.id(),
        convert_to_ble_point_configs(config)?,
    )?))
}

#[cfg(feature = "zigbee")]
fn build_zigbee(config: &RuntimeChannelConfig) -> Result<Box<dyn ChannelRuntime>> {
    use crate::core::channels::converters::convert_to_zigbee_point_configs;
    use crate::protocols::adapters::zigbee::ZigbeeChannel;
    use crate::protocols::adapters::zigbee_config::ZigbeeParamsConfig;

    let parameters: ZigbeeParamsConfig =
        decode_parameters(&config.channel_config().parameters, "Zigbee")?;
    Ok(Box::new(ZigbeeChannel::new(
        parameters.into_config()?,
        config.id(),
        convert_to_zigbee_point_configs(config)?,
    )?))
}

#[cfg(feature = "iec104")]
fn build_iec104(config: &RuntimeChannelConfig) -> Result<Box<dyn ChannelRuntime>> {
    use crate::core::channels::converters::convert_to_iec104_point_configs;
    use crate::protocols::adapters::iec104::{Iec104Channel, Iec104ParamsConfig};

    let parameters: Iec104ParamsConfig =
        decode_parameters(&config.channel_config().parameters, "IEC 104")?;
    parameters.validate()?;
    Ok(Box::new(Iec104Channel::new(
        parameters
            .into_config()
            .with_points(convert_to_iec104_point_configs(config)?),
    )))
}

#[cfg(feature = "opcua")]
fn build_opcua(config: &RuntimeChannelConfig) -> Result<Box<dyn ChannelRuntime>> {
    use crate::core::channels::converters::convert_to_opcua_point_configs;
    use crate::protocols::adapters::opcua::{OpcUaChannel, OpcUaParamsConfig};

    let parameters: OpcUaParamsConfig =
        decode_parameters(&config.channel_config().parameters, "OPC UA")?;
    parameters.validate()?;
    Ok(Box::new(OpcUaChannel::new(
        parameters
            .into_config()
            .with_points(convert_to_opcua_point_configs(config)?),
    )))
}

#[cfg(feature = "dl645")]
fn build_dl645(config: &RuntimeChannelConfig) -> Result<Box<dyn ChannelRuntime>> {
    use crate::protocols::adapters::dl645::{Dl645Channel, Dl645ChannelParamsConfig};

    reject_mapped_points(
        "DL/T 645",
        PointType::Telemetry,
        config.telemetry_points.iter().map(|point| &point.base),
    )?;
    reject_mapped_points(
        "DL/T 645",
        PointType::Signal,
        config.signal_points.iter().map(|point| &point.base),
    )?;
    reject_mapped_points(
        "DL/T 645",
        PointType::Control,
        config.control_points.iter().map(|point| &point.base),
    )?;
    reject_mapped_points(
        "DL/T 645",
        PointType::Adjustment,
        config.adjustment_points.iter().map(|point| &point.base),
    )?;
    let parameters: Dl645ChannelParamsConfig =
        decode_parameters(&config.channel_config().parameters, "DL/T 645")?;
    parameters.validate()?;
    Ok(Box::new(Dl645Channel::new(
        parameters.into_channel_config(),
        config.id(),
    )))
}

#[cfg(feature = "bacnet")]
fn build_bacnet(config: &RuntimeChannelConfig) -> Result<Box<dyn ChannelRuntime>> {
    use crate::protocols::adapters::bacnet::{
        BacnetChannel, BacnetParamsConfig, BacnetPointConfig, parse_point_mapping,
    };

    let parameters: BacnetParamsConfig =
        decode_parameters(&config.channel_config().parameters, "BACnet/IP")?;
    parameters.validate()?;
    let mut points = Vec::with_capacity(config.point_count());

    for point in &config.telemetry_points {
        let mapping = runtime_mapping(&point.base)?.ok_or_else(|| {
            invalid_mapping(
                point.base.point_id,
                "BACnet telemetry point requires a mapping",
            )
        })?;
        points.push(BacnetPointConfig {
            point_id: point.base.point_id,
            point_type: PointType::Telemetry,
            mapping: parse_point_mapping(&mapping, PointType::Telemetry)?,
            scale: point.scale,
            offset: point.offset,
            reverse: false,
        });
    }
    for point in &config.signal_points {
        let mapping = runtime_mapping(&point.base)?.ok_or_else(|| {
            invalid_mapping(
                point.base.point_id,
                "BACnet signal point requires a mapping",
            )
        })?;
        points.push(BacnetPointConfig {
            point_id: point.base.point_id,
            point_type: PointType::Signal,
            mapping: parse_point_mapping(&mapping, PointType::Signal)?,
            scale: 1.0,
            offset: 0.0,
            reverse: point.reverse,
        });
    }
    for point in &config.control_points {
        let mapping = runtime_mapping(&point.base)?.ok_or_else(|| {
            invalid_mapping(
                point.base.point_id,
                "BACnet control point requires a mapping",
            )
        })?;
        points.push(BacnetPointConfig {
            point_id: point.base.point_id,
            point_type: PointType::Control,
            mapping: parse_point_mapping(&mapping, PointType::Control)?,
            scale: 1.0,
            offset: 0.0,
            reverse: point.reverse,
        });
    }
    for point in &config.adjustment_points {
        let mapping = runtime_mapping(&point.base)?.ok_or_else(|| {
            invalid_mapping(
                point.base.point_id,
                "BACnet adjustment point requires a mapping",
            )
        })?;
        if point.scale == 0.0 {
            return Err(invalid_mapping(
                point.base.point_id,
                "BACnet adjustment scale must not be zero",
            ));
        }
        points.push(BacnetPointConfig {
            point_id: point.base.point_id,
            point_type: PointType::Adjustment,
            mapping: parse_point_mapping(&mapping, PointType::Adjustment)?,
            scale: point.scale,
            offset: point.offset,
            reverse: false,
        });
    }

    Ok(Box::new(BacnetChannel::new(
        parameters,
        config.id(),
        points,
    )?))
}

#[cfg(feature = "cjt188")]
fn build_cjt188(config: &RuntimeChannelConfig) -> Result<Box<dyn ChannelRuntime>> {
    use crate::protocols::adapters::cjt188::{
        Cjt188Channel, Cjt188ParamsConfig, Cjt188PointConfig, parse_point_mapping,
    };

    reject_mapped_points(
        "CJ/T 188",
        PointType::Control,
        config.control_points.iter().map(|point| &point.base),
    )?;
    reject_mapped_points(
        "CJ/T 188",
        PointType::Adjustment,
        config.adjustment_points.iter().map(|point| &point.base),
    )?;
    let parameters: Cjt188ParamsConfig =
        decode_parameters(&config.channel_config().parameters, "CJ/T 188")?;
    parameters.validate()?;
    let mut points = Vec::with_capacity(config.telemetry_points.len() + config.signal_points.len());
    for point in &config.telemetry_points {
        let mapping = runtime_mapping(&point.base)?.ok_or_else(|| {
            invalid_mapping(
                point.base.point_id,
                "CJ/T 188 telemetry point requires a mapping",
            )
        })?;
        points.push(Cjt188PointConfig {
            point_id: point.base.point_id,
            point_type: PointType::Telemetry,
            mapping: parse_point_mapping(&mapping, PointType::Telemetry)?,
            scale: point.scale,
            offset: point.offset,
            reverse: false,
        });
    }
    for point in &config.signal_points {
        let mapping = runtime_mapping(&point.base)?.ok_or_else(|| {
            invalid_mapping(
                point.base.point_id,
                "CJ/T 188 signal point requires a mapping",
            )
        })?;
        points.push(Cjt188PointConfig {
            point_id: point.base.point_id,
            point_type: PointType::Signal,
            mapping: parse_point_mapping(&mapping, PointType::Signal)?,
            scale: 1.0,
            offset: 0.0,
            reverse: point.reverse,
        });
    }

    Ok(Box::new(Cjt188Channel::new(
        parameters,
        config.id(),
        points,
    )?))
}

#[cfg(feature = "iec101")]
fn build_iec101(config: &RuntimeChannelConfig) -> Result<Box<dyn ChannelRuntime>> {
    use crate::protocols::adapters::iec101::{
        Iec101Channel, Iec101ParamsConfig, Iec101PointConfig, parse_point_mapping,
    };

    reject_mapped_points(
        "IEC 101",
        PointType::Control,
        config.control_points.iter().map(|point| &point.base),
    )?;
    reject_mapped_points(
        "IEC 101",
        PointType::Adjustment,
        config.adjustment_points.iter().map(|point| &point.base),
    )?;
    let parameters: Iec101ParamsConfig =
        decode_parameters(&config.channel_config().parameters, "IEC 101")?;
    parameters.validate()?;
    let mut points = Vec::with_capacity(config.telemetry_points.len() + config.signal_points.len());
    for point in &config.telemetry_points {
        let mapping = runtime_mapping(&point.base)?.ok_or_else(|| {
            invalid_mapping(
                point.base.point_id,
                "IEC 101 telemetry point requires a mapping",
            )
        })?;
        points.push(Iec101PointConfig {
            point_id: point.base.point_id,
            point_type: PointType::Telemetry,
            mapping: parse_point_mapping(&mapping, PointType::Telemetry)?,
            scale: point.scale,
            offset: point.offset,
            reverse: false,
        });
    }
    for point in &config.signal_points {
        let mapping = runtime_mapping(&point.base)?.ok_or_else(|| {
            invalid_mapping(
                point.base.point_id,
                "IEC 101 signal point requires a mapping",
            )
        })?;
        points.push(Iec101PointConfig {
            point_id: point.base.point_id,
            point_type: PointType::Signal,
            mapping: parse_point_mapping(&mapping, PointType::Signal)?,
            scale: 1.0,
            offset: 0.0,
            reverse: point.reverse,
        });
    }

    Ok(Box::new(Iec101Channel::new(
        parameters,
        config.id(),
        points,
    )?))
}

#[cfg(feature = "gb32960")]
fn build_gb32960(config: &RuntimeChannelConfig) -> Result<Box<dyn ChannelRuntime>> {
    use crate::protocols::adapters::gb32960::{
        Gb32960Channel, Gb32960ParamsConfig, Gb32960PointConfig, parse_point_mapping,
    };

    reject_mapped_points(
        "GB/T 32960",
        PointType::Control,
        config.control_points.iter().map(|point| &point.base),
    )?;
    reject_mapped_points(
        "GB/T 32960",
        PointType::Adjustment,
        config.adjustment_points.iter().map(|point| &point.base),
    )?;
    let parameters: Gb32960ParamsConfig =
        decode_parameters(&config.channel_config().parameters, "GB/T 32960")?;
    parameters.validate()?;
    let mut points = Vec::with_capacity(config.telemetry_points.len() + config.signal_points.len());
    for point in &config.telemetry_points {
        let mapping = runtime_mapping(&point.base)?.ok_or_else(|| {
            invalid_mapping(
                point.base.point_id,
                "GB/T 32960 telemetry point requires a mapping",
            )
        })?;
        points.push(Gb32960PointConfig {
            point_id: point.base.point_id,
            point_type: PointType::Telemetry,
            mapping: parse_point_mapping(&mapping, PointType::Telemetry)?,
            scale: point.scale,
            offset: point.offset,
            reverse: false,
        });
    }
    for point in &config.signal_points {
        let mapping = runtime_mapping(&point.base)?.ok_or_else(|| {
            invalid_mapping(
                point.base.point_id,
                "GB/T 32960 signal point requires a mapping",
            )
        })?;
        points.push(Gb32960PointConfig {
            point_id: point.base.point_id,
            point_type: PointType::Signal,
            mapping: parse_point_mapping(&mapping, PointType::Signal)?,
            scale: 1.0,
            offset: 0.0,
            reverse: point.reverse,
        });
    }

    Ok(Box::new(Gb32960Channel::new(
        parameters,
        config.id(),
        points,
    )?))
}

#[cfg(feature = "jt808")]
fn build_jt808(config: &RuntimeChannelConfig) -> Result<Box<dyn ChannelRuntime>> {
    use crate::protocols::adapters::jt808::{
        Jt808Channel, Jt808ParamsConfig, Jt808PointConfig, parse_point_mapping,
    };

    reject_mapped_points(
        "JT/T 808",
        PointType::Control,
        config.control_points.iter().map(|point| &point.base),
    )?;
    reject_mapped_points(
        "JT/T 808",
        PointType::Adjustment,
        config.adjustment_points.iter().map(|point| &point.base),
    )?;
    let parameters: Jt808ParamsConfig =
        decode_parameters(&config.channel_config().parameters, "JT/T 808")?;
    parameters.validate()?;
    let mut points = Vec::with_capacity(config.telemetry_points.len() + config.signal_points.len());
    for point in &config.telemetry_points {
        let mapping = runtime_mapping(&point.base)?.ok_or_else(|| {
            invalid_mapping(
                point.base.point_id,
                "JT/T 808 telemetry point requires a mapping",
            )
        })?;
        points.push(Jt808PointConfig {
            point_id: point.base.point_id,
            point_type: PointType::Telemetry,
            mapping: parse_point_mapping(&mapping, PointType::Telemetry)?,
            scale: point.scale,
            offset: point.offset,
            reverse: false,
        });
    }
    for point in &config.signal_points {
        let mapping = runtime_mapping(&point.base)?.ok_or_else(|| {
            invalid_mapping(
                point.base.point_id,
                "JT/T 808 signal point requires a mapping",
            )
        })?;
        points.push(Jt808PointConfig {
            point_id: point.base.point_id,
            point_type: PointType::Signal,
            mapping: parse_point_mapping(&mapping, PointType::Signal)?,
            scale: 1.0,
            offset: 0.0,
            reverse: point.reverse,
        });
    }

    Ok(Box::new(Jt808Channel::new(
        parameters,
        config.id(),
        points,
    )?))
}

#[cfg(all(feature = "can", target_os = "linux"))]
fn build_can(config: &RuntimeChannelConfig) -> Result<Box<dyn ChannelRuntime>> {
    use crate::core::channels::converters::convert_to_can_point_configs;
    use crate::protocols::adapters::can::{CanChannelParamsConfig, CanClient};

    let parameters: CanChannelParamsConfig =
        decode_parameters(&config.channel_config().parameters, "CAN")?;
    parameters.validate()?;
    let mut client = CanClient::new(parameters.into_config());
    client.add_points(convert_to_can_point_configs(config)?)?;
    Ok(Box::new(client))
}

#[cfg(all(feature = "j1939", target_os = "linux"))]
fn build_j1939(config: &RuntimeChannelConfig) -> Result<Box<dyn ChannelRuntime>> {
    use crate::protocols::adapters::can::{J1939Client, J1939Config, J1939PointConfig};
    use crate::protocols::core::point::TransformConfig;

    let parameters: J1939Config = decode_parameters(&config.channel_config().parameters, "J1939")?;
    parameters.validate()?;
    reject_mapped_points(
        "J1939",
        PointType::Control,
        config.control_points.iter().map(|point| &point.base),
    )?;
    reject_mapped_points(
        "J1939",
        PointType::Adjustment,
        config.adjustment_points.iter().map(|point| &point.base),
    )?;
    let mut points = Vec::with_capacity(config.telemetry_points.len() + config.signal_points.len());
    for point in &config.telemetry_points {
        let Some(mapping) = runtime_mapping(&point.base)? else {
            continue;
        };
        points.push(
            J1939PointConfig::from_mapping(
                point.base.point_id,
                PointType::Telemetry,
                &mapping,
                TransformConfig {
                    scale: point.scale,
                    offset: point.offset,
                    reverse: point.reverse,
                    ..TransformConfig::default()
                },
            )
            .map_err(|error| IoError::config(error.to_string()))?,
        );
    }
    for point in &config.signal_points {
        let Some(mapping) = runtime_mapping(&point.base)? else {
            continue;
        };
        points.push(
            J1939PointConfig::from_mapping(
                point.base.point_id,
                PointType::Signal,
                &mapping,
                TransformConfig {
                    reverse: point.reverse,
                    ..TransformConfig::default()
                },
            )
            .map_err(|error| IoError::config(error.to_string()))?,
        );
    }
    Ok(Box::new(J1939Client::new(parameters, points)?))
}

#[cfg(all(feature = "gpio", target_os = "linux"))]
fn build_gpio(config: &RuntimeChannelConfig) -> Result<Box<dyn ChannelRuntime>> {
    use crate::protocols::adapters::gpio::{
        GpioChannel, GpioChannelConfig, GpioChannelParamsConfig, GpioDriverType, GpioPinConfig,
    };

    let parameters: GpioChannelParamsConfig =
        decode_parameters(&config.channel_config().parameters, "GPIO")?;
    parameters.validate()?;
    let GpioChannelParamsConfig {
        driver,
        gpio_chip,
        sysfs_base_path,
    } = parameters;
    let is_sysfs = driver.eq_ignore_ascii_case("sysfs");
    let driver = if is_sysfs {
        GpioDriverType::Sysfs {
            base_path: sysfs_base_path,
        }
    } else {
        GpioDriverType::Gpiod
    };
    let mut channel_config = GpioChannelConfig {
        driver,
        pins: Vec::new(),
    };
    reject_mapped_points(
        "GPIO",
        PointType::Telemetry,
        config.telemetry_points.iter().map(|point| &point.base),
    )?;
    reject_mapped_points(
        "GPIO",
        PointType::Adjustment,
        config.adjustment_points.iter().map(|point| &point.base),
    )?;
    let mut add_point =
        |point_type: PointType, point_id: u32, reverse: bool, point: &Point| -> Result<()> {
            let Some(mapping) = runtime_mapping(point)? else {
                return Ok(());
            };
            let gpio_number =
                crate::protocols::adapters::gpio::parse_point_mapping(&mapping, point_type)
                    .map_err(|error| invalid_mapping(point_id, &error.to_string()))?
                    .gpio_number;
            let pin = match (is_sysfs, point_type) {
                (true, PointType::Signal) => {
                    GpioPinConfig::digital_input_sysfs(gpio_number, point_id)
                },
                (true, PointType::Control) => {
                    GpioPinConfig::digital_output_sysfs(gpio_number, point_id)
                },
                (false, PointType::Signal) => {
                    let mut pin =
                        GpioPinConfig::digital_input(gpio_chip.as_str(), gpio_number, point_id);
                    pin.gpio_number = Some(gpio_number);
                    pin
                },
                (false, PointType::Control) => {
                    let mut pin =
                        GpioPinConfig::digital_output(gpio_chip.as_str(), gpio_number, point_id);
                    pin.gpio_number = Some(gpio_number);
                    pin
                },
                _ => return Err(invalid_mapping(point_id, "unsupported GPIO point type")),
            }
            .with_active_low(reverse);
            channel_config.pins.push(pin);
            Ok(())
        };

    for point in &config.signal_points {
        add_point(
            PointType::Signal,
            point.base.point_id,
            point.reverse,
            &point.base,
        )?;
    }
    for point in &config.control_points {
        add_point(
            PointType::Control,
            point.base.point_id,
            point.reverse,
            &point.base,
        )?;
    }
    Ok(Box::new(GpioChannel::new(channel_config, config.id())))
}

#[cfg(feature = "iec61850")]
fn build_iec61850(config: &RuntimeChannelConfig) -> Result<Box<dyn ChannelRuntime>> {
    use crate::protocols::adapters::iec61850::{
        Iec61850Address, Iec61850Channel, Iec61850ParamsConfig,
    };
    use crate::protocols::core::point::{PointConfig, TransformConfig};

    let parameters: Iec61850ParamsConfig =
        decode_parameters(&config.channel_config().parameters, "IEC 61850")?;
    parameters.validate()?;
    let mut points = Vec::with_capacity(config.point_count());

    let parse_address =
        |point_type: PointType, point_id: u32, point: &Point| -> Result<Option<Iec61850Address>> {
            let Some(value) = runtime_mapping(point)? else {
                return Ok(None);
            };
            crate::protocols::adapters::iec61850::parse_point_mapping(&value, point_type)
                .map(Some)
                .map_err(|error| invalid_mapping(point_id, &error.to_string()))
        };

    for point in &config.telemetry_points {
        if let Some(address) =
            parse_address(PointType::Telemetry, point.base.point_id, &point.base)?
        {
            points.push(PointConfig {
                id: point.base.point_id,
                point_type: PointType::Telemetry,
                address,
                transform: TransformConfig {
                    scale: point.scale,
                    offset: point.offset,
                    reverse: point.reverse,
                    ..TransformConfig::default()
                },
            });
        }
    }
    for point in &config.signal_points {
        if let Some(address) = parse_address(PointType::Signal, point.base.point_id, &point.base)? {
            points.push(PointConfig {
                id: point.base.point_id,
                point_type: PointType::Signal,
                address,
                transform: TransformConfig {
                    reverse: point.reverse,
                    ..TransformConfig::default()
                },
            });
        }
    }
    for point in &config.control_points {
        if let Some(address) = parse_address(PointType::Control, point.base.point_id, &point.base)?
        {
            points.push(PointConfig {
                id: point.base.point_id,
                point_type: PointType::Control,
                address,
                transform: TransformConfig {
                    reverse: point.reverse,
                    ..TransformConfig::default()
                },
            });
        }
    }
    for point in &config.adjustment_points {
        if let Some(address) =
            parse_address(PointType::Adjustment, point.base.point_id, &point.base)?
        {
            points.push(PointConfig {
                id: point.base.point_id,
                point_type: PointType::Adjustment,
                address,
                transform: TransformConfig {
                    scale: point.scale,
                    offset: point.offset,
                    ..TransformConfig::default()
                },
            });
        }
    }
    Ok(Box::new(Iec61850Channel::new(
        config.name(),
        &parameters,
        points,
    )))
}

fn invalid_mapping(point_id: u32, reason: &str) -> IoError {
    IoError::config(format!(
        "point {point_id} mapping validation failed: {reason}"
    ))
}

#[cfg(feature = "modbus")]
fn validate_modbus_mapping(point_type: PointType, point_id: u32, mapping: &Value) -> Result<()> {
    crate::protocols::adapters::modbus_config::parse_point_mapping(point_type, point_id, mapping)
        .map(|_| ())
        .map_err(|error| invalid_mapping(point_id, &error.to_string()))
}

#[cfg(all(feature = "gpio", target_os = "linux"))]
fn validate_gpio_mapping(point_type: PointType, point_id: u32, mapping: &Value) -> Result<()> {
    crate::protocols::adapters::gpio::parse_point_mapping(mapping, point_type)
        .map(|_| ())
        .map_err(|error| invalid_mapping(point_id, &error.to_string()))
}

#[cfg(all(feature = "can", target_os = "linux"))]
fn validate_can_mapping(point_type: PointType, point_id: u32, mapping: &Value) -> Result<()> {
    crate::protocols::adapters::can::config::parse_point_mapping(mapping, point_type, point_id)
        .map(|_| ())
        .map_err(|error| invalid_mapping(point_id, &error.to_string()))
}

#[cfg(feature = "iec61850")]
fn validate_iec61850_mapping(point_type: PointType, point_id: u32, mapping: &Value) -> Result<()> {
    crate::protocols::adapters::iec61850::parse_point_mapping(mapping, point_type)
        .map(|_| ())
        .map_err(|error| invalid_mapping(point_id, &error.to_string()))
}

#[cfg(feature = "iec104")]
fn validate_iec104_mapping(_point_type: PointType, point_id: u32, mapping: &Value) -> Result<()> {
    crate::protocols::adapters::iec104::parse_point_mapping_value(mapping)
        .map(|_| ())
        .map_err(|error| invalid_mapping(point_id, &error.to_string()))
}

#[cfg(feature = "opcua")]
fn validate_opcua_mapping(_point_type: PointType, point_id: u32, mapping: &Value) -> Result<()> {
    crate::protocols::adapters::opcua::parse_point_mapping_value(mapping)
        .map(|_| ())
        .map_err(|error| invalid_mapping(point_id, &error.to_string()))
}

#[cfg(any(feature = "mqtt", feature = "http"))]
fn validate_json_mapping(point_type: PointType, point_id: u32, mapping: &Value) -> Result<()> {
    crate::protocols::adapters::json_mapper::validate_point_mapping(point_type, point_id, mapping)
        .map_err(|error| invalid_mapping(point_id, &error.to_string()))
}

#[cfg(feature = "ble")]
fn validate_ble_mapping(point_type: PointType, point_id: u32, mapping: &Value) -> Result<()> {
    crate::protocols::adapters::ble::validate_point_mapping(point_type, mapping)
        .map_err(|error| invalid_mapping(point_id, &error.to_string()))
}

#[cfg(feature = "zigbee")]
fn validate_zigbee_mapping(point_type: PointType, point_id: u32, mapping: &Value) -> Result<()> {
    crate::protocols::adapters::zigbee::validate_point_mapping(point_type, mapping)
        .map_err(|error| invalid_mapping(point_id, &error.to_string()))
}

#[cfg(all(feature = "j1939", target_os = "linux"))]
fn validate_j1939_mapping(point_type: PointType, point_id: u32, mapping: &Value) -> Result<()> {
    crate::protocols::adapters::can::j1939::validate_point_mapping(point_type, mapping)
        .map_err(|error| invalid_mapping(point_id, &error.to_string()))
}

#[cfg(feature = "dl645")]
fn reject_inline_mapping(_point_type: PointType, point_id: u32, _mapping: &Value) -> Result<()> {
    Err(invalid_mapping(
        point_id,
        "this protocol does not consume inline point mappings",
    ))
}

#[cfg(feature = "bacnet")]
fn validate_bacnet_mapping(point_type: PointType, point_id: u32, mapping: &Value) -> Result<()> {
    crate::protocols::adapters::bacnet::parse_point_mapping(mapping, point_type)
        .map(|_| ())
        .map_err(|error| invalid_mapping(point_id, &error.to_string()))
}

#[cfg(feature = "cjt188")]
fn validate_cjt188_mapping(point_type: PointType, point_id: u32, mapping: &Value) -> Result<()> {
    crate::protocols::adapters::cjt188::parse_point_mapping(mapping, point_type)
        .map(|_| ())
        .map_err(|error| invalid_mapping(point_id, &error.to_string()))
}

#[cfg(feature = "iec101")]
fn validate_iec101_mapping(point_type: PointType, point_id: u32, mapping: &Value) -> Result<()> {
    crate::protocols::adapters::iec101::parse_point_mapping(mapping, point_type)
        .map(|_| ())
        .map_err(|error| invalid_mapping(point_id, &error.to_string()))
}

#[cfg(feature = "gb32960")]
fn validate_gb32960_mapping(point_type: PointType, point_id: u32, mapping: &Value) -> Result<()> {
    crate::protocols::adapters::gb32960::parse_point_mapping(mapping, point_type)
        .map(|_| ())
        .map_err(|error| invalid_mapping(point_id, &error.to_string()))
}

#[cfg(feature = "jt808")]
fn validate_jt808_mapping(point_type: PointType, point_id: u32, mapping: &Value) -> Result<()> {
    crate::protocols::adapters::jt808::parse_point_mapping(mapping, point_type)
        .map(|_| ())
        .map_err(|error| invalid_mapping(point_id, &error.to_string()))
}

fn build_registry() -> ProtocolRegistry {
    let mut registry = ProtocolRegistry::new();

    #[cfg(feature = "modbus")]
    {
        use crate::protocols::adapters::modbus::ModbusChannel;
        registry.register(ProtocolFactory::new(
            ProtocolMetadata {
                name: "modbus",
                display_name: "Modbus TCP",
                description: "Industrial Modbus TCP protocol",
                protocol_type: "modbus_tcp",
                drivers: vec![ModbusChannel::tcp_metadata()],
                supports_points: true,
            },
            &["modbus"],
            1_000,
            &[
                "slave_id",
                "function_code",
                "register_address",
                "bit_position",
            ],
            validate_modbus_tcp_parameters,
            validate_modbus_mapping,
            build_modbus_tcp,
        ));
        registry.register(ProtocolFactory::new(
            ProtocolMetadata {
                name: "modbus_rtu",
                display_name: "Modbus RTU",
                description: "Industrial Modbus RTU protocol over a serial device",
                protocol_type: "modbus_rtu",
                drivers: vec![ModbusChannel::rtu_metadata()],
                supports_points: true,
            },
            &[],
            1_000,
            &[
                "slave_id",
                "function_code",
                "register_address",
                "bit_position",
            ],
            validate_modbus_rtu_parameters,
            validate_modbus_mapping,
            build_modbus_rtu,
        ));
    }

    #[cfg(all(feature = "gpio", target_os = "linux"))]
    {
        use crate::protocols::adapters::gpio::{GpiodDriver, SysfsDriver};
        registry.register(ProtocolFactory::new(
            ProtocolMetadata {
                name: "gpio",
                display_name: "GPIO",
                description: "Digital Input/Output via GPIO pins",
                protocol_type: "di_do",
                drivers: vec![GpiodDriver::metadata(), SysfsDriver::metadata()],
                supports_points: true,
            },
            &["gpio", "dido"],
            200,
            &["gpio_number"],
            validate_gpio_parameters,
            validate_gpio_mapping,
            build_gpio,
        ));
    }

    #[cfg(feature = "iec104")]
    {
        use crate::protocols::adapters::iec104::Iec104Channel;
        registry.register(ProtocolFactory::new(
            ProtocolMetadata {
                name: "iec104",
                display_name: "IEC 60870-5-104",
                description: "IEC 104 telecontrol protocol over TCP/IP",
                protocol_type: "iec104",
                drivers: vec![Iec104Channel::metadata()],
                supports_points: true,
            },
            &[
                "iec_104",
                "iec60870",
                "iec_60870",
                "iec60870_5_104",
                "iec_60870_5_104",
            ],
            100,
            &["ioa", "type_id"],
            validate_iec104_parameters,
            validate_iec104_mapping,
            build_iec104,
        ));
    }

    #[cfg(feature = "opcua")]
    {
        use crate::protocols::adapters::opcua::OpcUaChannel;
        registry.register(ProtocolFactory::new(
            ProtocolMetadata {
                name: "opcua",
                display_name: "OPC UA",
                description: "OPC UA client for industrial automation",
                protocol_type: "opcua",
                drivers: vec![OpcUaChannel::metadata()],
                supports_points: true,
            },
            &["opc_ua"],
            1_000,
            &["namespace_index"],
            validate_opcua_parameters,
            validate_opcua_mapping,
            build_opcua,
        ));
    }

    #[cfg(all(feature = "can", target_os = "linux"))]
    {
        use crate::protocols::adapters::can::CanClient;
        registry.register(ProtocolFactory::new(
            ProtocolMetadata {
                name: "can",
                display_name: "CAN Bus",
                description: "Controller Area Network (CAN) bus protocol",
                protocol_type: "can",
                drivers: vec![CanClient::metadata()],
                supports_points: true,
            },
            &[],
            200,
            &[
                "can_id",
                "start_bit",
                "byte_offset",
                "bit_position",
                "bit_length",
                "scale",
                "offset",
            ],
            validate_can_parameters,
            validate_can_mapping,
            build_can,
        ));
    }

    #[cfg(all(feature = "j1939", target_os = "linux"))]
    {
        use crate::protocols::adapters::can::J1939Client;
        registry.register(ProtocolFactory::new(
            ProtocolMetadata {
                name: "j1939",
                display_name: "SAE J1939",
                description: "SAE J1939 vehicle network protocol over CAN",
                protocol_type: "j1939",
                drivers: vec![J1939Client::metadata()],
                supports_points: true,
            },
            &[],
            1_000,
            &["spn", "active_raw_value"],
            validate_j1939_parameters,
            validate_j1939_mapping,
            build_j1939,
        ));
    }

    #[cfg(feature = "dl645")]
    {
        use crate::protocols::adapters::dl645::Dl645Channel;
        registry.register(ProtocolFactory::new(
            ProtocolMetadata {
                name: "dl645",
                display_name: "DL/T 645-2007",
                description: "DL/T 645-2007 smart meter protocol",
                protocol_type: "dl645",
                drivers: vec![Dl645Channel::metadata()],
                supports_points: true,
            },
            &[],
            1_000,
            &[],
            validate_dl645_parameters,
            reject_inline_mapping,
            build_dl645,
        ));
    }

    #[cfg(feature = "bacnet")]
    {
        use crate::protocols::adapters::bacnet::BacnetChannel;
        registry.register(ProtocolFactory::new(
            ProtocolMetadata {
                name: "bacnet",
                display_name: "BACnet/IP",
                description: "BACnet/IP building automation protocol over UDP",
                protocol_type: "bacnet_ip",
                drivers: vec![BacnetChannel::metadata()],
                supports_points: true,
            },
            &["bacnet"],
            1_000,
            &[
                "object_type",
                "object_instance",
                "property_id",
                "array_index",
            ],
            validate_bacnet_parameters,
            validate_bacnet_mapping,
            build_bacnet,
        ));
    }

    #[cfg(feature = "cjt188")]
    {
        use crate::protocols::adapters::cjt188::Cjt188Channel;
        registry.register(ProtocolFactory::new(
            ProtocolMetadata {
                name: "cjt188",
                display_name: "CJ/T 188",
                description: "CJ/T 188 household meter protocol over serial or TCP",
                protocol_type: "cjt188",
                drivers: vec![Cjt188Channel::metadata()],
                supports_points: true,
            },
            &["cj_t_188", "cjt_188"],
            5_000,
            &["data_id", "byte_offset", "byte_length"],
            validate_cjt188_parameters,
            validate_cjt188_mapping,
            build_cjt188,
        ));
    }

    #[cfg(feature = "iec101")]
    {
        use crate::protocols::adapters::iec101::Iec101Channel;
        registry.register(ProtocolFactory::new(
            ProtocolMetadata {
                name: "iec101",
                display_name: "IEC 60870-5-101",
                description: "IEC 101 unbalanced master over serial or TCP gateway",
                protocol_type: "iec101",
                drivers: vec![Iec101Channel::metadata()],
                supports_points: true,
            },
            &["iec_101", "iec60870_5_101", "iec_60870_5_101"],
            5_000,
            &["ioa", "type_id"],
            validate_iec101_parameters,
            validate_iec101_mapping,
            build_iec101,
        ));
    }

    #[cfg(feature = "gb32960")]
    {
        use crate::protocols::adapters::gb32960::Gb32960Channel;
        registry.register(ProtocolFactory::new(
            ProtocolMetadata {
                name: "gb32960",
                display_name: "GB/T 32960",
                description: "GB/T 32960 vehicle telemetry terminal server",
                protocol_type: "gb32960",
                drivers: vec![Gb32960Channel::metadata()],
                supports_points: true,
            },
            &["gb_t_32960", "gbt32960"],
            1_000,
            &["motor_index"],
            validate_gb32960_parameters,
            validate_gb32960_mapping,
            build_gb32960,
        ));
    }

    #[cfg(feature = "jt808")]
    {
        use crate::protocols::adapters::jt808::Jt808Channel;
        registry.register(ProtocolFactory::new(
            ProtocolMetadata {
                name: "jt808",
                display_name: "JT/T 808",
                description: "Authenticated JT/T 808 positioning terminal server",
                protocol_type: "jt808",
                drivers: vec![Jt808Channel::metadata()],
                supports_points: true,
            },
            &["jt_t_808", "jtt808"],
            1_000,
            &[],
            validate_jt808_parameters,
            validate_jt808_mapping,
            build_jt808,
        ));
    }

    #[cfg(feature = "mqtt")]
    {
        use crate::protocols::adapters::mqtt::MqttChannel;
        registry.register(ProtocolFactory::new(
            ProtocolMetadata {
                name: "mqtt",
                display_name: "MQTT JSON",
                description: "Event-driven MQTT subscriber with JSONPath point mappings",
                protocol_type: "mqtt",
                drivers: vec![MqttChannel::metadata()],
                supports_points: true,
            },
            &["mqtt_protocol"],
            1_000,
            &[],
            validate_mqtt_parameters,
            validate_json_mapping,
            build_mqtt,
        ));
    }

    #[cfg(feature = "http")]
    {
        use crate::protocols::adapters::http::HttpChannel;
        registry.register(ProtocolFactory::new(
            ProtocolMetadata {
                name: "http",
                display_name: "HTTP JSON",
                description: "Polling HTTP client with JSONPath point mappings",
                protocol_type: "http",
                drivers: vec![HttpChannel::metadata()],
                supports_points: true,
            },
            &[],
            5_000,
            &[],
            validate_http_parameters,
            validate_json_mapping,
            build_http,
        ));
    }

    #[cfg(feature = "ble")]
    {
        use crate::protocols::adapters::ble::BleChannel;
        registry.register(ProtocolFactory::new(
            ProtocolMetadata {
                name: "ble",
                display_name: "BLE GATT",
                description: "Bluetooth Low Energy GATT client",
                protocol_type: "ble",
                drivers: vec![BleChannel::metadata()],
                supports_points: true,
            },
            &[],
            1_000,
            &[],
            validate_ble_parameters,
            validate_ble_mapping,
            build_ble,
        ));
    }

    #[cfg(feature = "zigbee")]
    {
        use crate::protocols::adapters::zigbee::ZigbeeChannel;
        registry.register(ProtocolFactory::new(
            ProtocolMetadata {
                name: "zigbee",
                display_name: "Zigbee",
                description: "Zigbee protocol via TCP-connected coordinator gateway",
                protocol_type: "zigbee",
                drivers: vec![ZigbeeChannel::metadata()],
                supports_points: true,
            },
            &[],
            1_000,
            &[],
            validate_zigbee_parameters,
            validate_zigbee_mapping,
            build_zigbee,
        ));
    }

    #[cfg(feature = "iec61850")]
    {
        use crate::protocols::adapters::iec61850::Iec61850Channel;
        registry.register(ProtocolFactory::new(
            ProtocolMetadata {
                name: "iec61850",
                display_name: "IEC 61850 MMS",
                description: "IEC 61850 MMS client over TCP/ISO transport",
                protocol_type: "iec61850",
                drivers: vec![Iec61850Channel::metadata()],
                supports_points: true,
            },
            &[],
            1_000,
            &["ctrl_model"],
            validate_iec61850_parameters,
            validate_iec61850_mapping,
            build_iec61850,
        ));
    }

    registry
}

static PROTOCOL_REGISTRY: LazyLock<ProtocolRegistry> = LazyLock::new(build_registry);

/// Return the exact static protocol composition for this binary.
#[must_use]
pub fn get_protocol_registry() -> &'static ProtocolRegistry {
    &PROTOCOL_REGISTRY
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    #[cfg(any(feature = "dl645", feature = "iec104", feature = "opcua"))]
    use std::collections::HashMap;

    use super::*;
    #[cfg(any(feature = "dl645", feature = "iec104", feature = "opcua"))]
    use crate::core::config::{ChannelCore, ChannelLoggingConfig};

    #[cfg(any(feature = "dl645", feature = "iec104", feature = "opcua"))]
    fn config(protocol: &str, parameters: HashMap<String, Value>) -> ChannelConfig {
        ChannelConfig {
            core: ChannelCore {
                id: 1,
                name: format!("{protocol}-test"),
                description: None,
                protocol: protocol.to_owned(),
                enabled: true,
            },
            parameters,
            logging: ChannelLoggingConfig::default(),
        }
    }

    #[test]
    fn canonical_ids_and_aliases_are_unique() {
        let registry = get_protocol_registry();
        let mut canonical = BTreeSet::new();
        let mut lookup_names = BTreeSet::new();
        for factory in &registry.factories {
            assert!(canonical.insert(factory.metadata.protocol_type));
            for name in std::iter::once(factory.metadata.protocol_type)
                .chain(std::iter::once(factory.metadata.name))
                .chain(factory.aliases.iter().copied())
            {
                let normalized = name
                    .bytes()
                    .filter(|byte| !matches!(byte, b'-' | b'_' | b' ' | b'.'))
                    .map(|byte| byte.to_ascii_lowercase())
                    .collect::<Vec<_>>();
                if !lookup_names.insert(normalized) {
                    assert!(
                        factory.matches(name),
                        "duplicate lookup name does not resolve to its factory"
                    );
                }
                assert!(
                    std::ptr::eq(registry.factory(name).expect("registered name"), factory),
                    "{name} resolves to a different factory"
                );
            }
        }
    }

    #[test]
    fn every_factory_owns_all_runtime_callbacks() {
        for factory in &get_protocol_registry().factories {
            let _ = factory.validate_parameters;
            let _ = factory.validate_mapping;
            let _ = factory.build_runtime;
            assert!(!factory.metadata.drivers.is_empty());
        }
    }

    #[test]
    fn every_driver_exposes_the_single_shared_runtime_policy_schema() {
        for factory in &get_protocol_registry().factories {
            for driver in &factory.metadata.drivers {
                let names = driver
                    .parameters
                    .iter()
                    .map(|parameter| parameter.name)
                    .collect::<Vec<_>>();
                let unique = names.iter().copied().collect::<BTreeSet<_>>();
                assert_eq!(
                    unique.len(),
                    names.len(),
                    "{} contains duplicate parameter metadata",
                    driver.name
                );
                for runtime_key in RUNTIME_PARAMETER_KEYS {
                    assert!(
                        unique.contains(runtime_key),
                        "{} does not advertise shared runtime parameter {runtime_key}",
                        driver.name
                    );
                }
                let poll = driver
                    .parameters
                    .iter()
                    .find(|parameter| parameter.name == "poll_interval_ms")
                    .expect("shared poll interval metadata");
                assert_eq!(
                    poll.default_value.as_ref().and_then(Value::as_u64),
                    Some(factory.default_poll_interval_ms)
                );
            }
        }
    }

    #[cfg(any(feature = "dl645", feature = "iec104", feature = "opcua"))]
    #[test]
    fn semantic_channel_parameters_fail_closed_before_runtime_construction() {
        let registry = get_protocol_registry();

        #[cfg(feature = "iec104")]
        assert!(
            registry
                .factory("iec104")
                .expect("IEC 104 factory")
                .validate_channel(&config(
                    "iec104",
                    HashMap::from([("address".to_owned(), Value::String(String::new()))]),
                ))
                .is_err()
        );

        #[cfg(feature = "opcua")]
        assert!(
            registry
                .factory("opcua")
                .expect("OPC UA factory")
                .validate_channel(&config(
                    "opcua",
                    HashMap::from([
                        ("endpoint_url".to_owned(), Value::String(String::new())),
                        ("username".to_owned(), Value::String("operator".to_owned())),
                    ]),
                ))
                .is_err()
        );

        #[cfg(feature = "dl645")]
        assert!(
            registry
                .factory("dl645")
                .expect("DL/T 645 factory")
                .validate_channel(&config(
                    "dl645",
                    HashMap::from([
                        (
                            "meter_address".to_owned(),
                            Value::String("not-a-meter".to_owned()),
                        ),
                        ("host".to_owned(), Value::String("127.0.0.1".to_owned())),
                        (
                            "device".to_owned(),
                            Value::String("/dev/ttyUSB0".to_owned())
                        ),
                    ]),
                ))
                .is_err()
        );
    }
}
