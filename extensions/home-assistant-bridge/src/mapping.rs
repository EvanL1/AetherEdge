//! Explicit, bounded Home Assistant entity-to-point mapping.

use std::collections::BTreeMap;

use aether_domain::{
    DomainError, EntityPointDescriptor, IntegrationPointKey, IntegrationPointKind, ObservedValue,
    ObservedValueType,
};
use aether_ports::{PortError, PortErrorKind, PortResult};
use serde_json::Value;

pub(crate) fn point_descriptors(
    domain: &str,
    _state: Option<&str>,
    attributes: &BTreeMap<String, Value>,
) -> PortResult<Vec<EntityPointDescriptor>> {
    let primary_key = primary_state_key(domain);
    let mut descriptors = vec![descriptor(
        primary_key,
        if primary_key == "is_on" {
            "Is on"
        } else {
            "State"
        },
        IntegrationPointKind::State,
        state_value_type(domain, attributes),
        state_unit(domain, attributes),
    )?];

    match domain {
        "light" => {
            push_if_present(
                &mut descriptors,
                attributes,
                "brightness",
                ObservedValueType::UInt64,
                None,
            )?;
            push_if_present(
                &mut descriptors,
                attributes,
                "color_temp_kelvin",
                ObservedValueType::UInt64,
                Some("K"),
            )?;
        },
        "climate" => {
            push_if_present(
                &mut descriptors,
                attributes,
                "current_temperature",
                ObservedValueType::Float64,
                state_unit(domain, attributes),
            )?;
            push_if_present(
                &mut descriptors,
                attributes,
                "temperature",
                ObservedValueType::Float64,
                state_unit(domain, attributes),
            )?;
            push_if_present(
                &mut descriptors,
                attributes,
                "current_humidity",
                ObservedValueType::Float64,
                Some("%"),
            )?;
            push_if_present(
                &mut descriptors,
                attributes,
                "hvac_action",
                ObservedValueType::Enum,
                None,
            )?;
        },
        "cover" => {
            push_if_present(
                &mut descriptors,
                attributes,
                "current_position",
                ObservedValueType::UInt64,
                Some("%"),
            )?;
            push_if_present(
                &mut descriptors,
                attributes,
                "current_tilt_position",
                ObservedValueType::UInt64,
                Some("%"),
            )?;
        },
        "fan" => {
            push_if_present(
                &mut descriptors,
                attributes,
                "percentage",
                ObservedValueType::UInt64,
                Some("%"),
            )?;
            push_if_present(
                &mut descriptors,
                attributes,
                "preset_mode",
                ObservedValueType::Enum,
                None,
            )?;
        },
        "vacuum" => {
            push_if_present(
                &mut descriptors,
                attributes,
                "battery_level",
                ObservedValueType::UInt64,
                Some("%"),
            )?;
        },
        "media_player" => {
            push_if_present(
                &mut descriptors,
                attributes,
                "volume_level",
                ObservedValueType::Float64,
                None,
            )?;
            push_if_present(
                &mut descriptors,
                attributes,
                "is_volume_muted",
                ObservedValueType::Boolean,
                None,
            )?;
        },
        "event" => {
            push_if_present(
                &mut descriptors,
                attributes,
                "event_type",
                ObservedValueType::Enum,
                None,
            )?;
        },
        _ => {},
    }

    Ok(descriptors)
}

fn primary_state_key(domain: &str) -> &'static str {
    match domain {
        "light" | "switch" | "fan" => "is_on",
        _ => "state",
    }
}

fn state_value_type(domain: &str, attributes: &BTreeMap<String, Value>) -> ObservedValueType {
    match domain {
        "light" | "switch" | "binary_sensor" | "fan" => ObservedValueType::Boolean,
        "number" => ObservedValueType::Float64,
        "sensor" if sensor_metadata_is_numeric(attributes) => ObservedValueType::Float64,
        "alarm_control_panel"
        | "climate"
        | "cover"
        | "lock"
        | "media_player"
        | "select"
        | "vacuum" => ObservedValueType::Enum,
        _ => ObservedValueType::String,
    }
}

fn sensor_metadata_is_numeric(attributes: &BTreeMap<String, Value>) -> bool {
    attributes
        .get("unit_of_measurement")
        .and_then(Value::as_str)
        .is_some_and(|unit| !unit.is_empty())
        || attributes
            .get("state_class")
            .and_then(Value::as_str)
            .is_some_and(|state_class| {
                matches!(state_class, "measurement" | "total" | "total_increasing")
            })
}

fn state_unit<'a>(domain: &str, attributes: &'a BTreeMap<String, Value>) -> Option<&'a str> {
    if matches!(domain, "sensor" | "number" | "climate") {
        return attributes
            .get("unit_of_measurement")
            .and_then(Value::as_str);
    }
    None
}

fn push_if_present(
    descriptors: &mut Vec<EntityPointDescriptor>,
    attributes: &BTreeMap<String, Value>,
    key: &str,
    value_type: ObservedValueType,
    unit: Option<&str>,
) -> PortResult<()> {
    if attributes.contains_key(key) {
        descriptors.push(descriptor(
            key,
            &humanized(key),
            IntegrationPointKind::Attribute,
            value_type,
            unit,
        )?);
    }
    Ok(())
}

fn descriptor(
    key: &str,
    display_name: &str,
    kind: IntegrationPointKind,
    value_type: ObservedValueType,
    unit: Option<&str>,
) -> PortResult<EntityPointDescriptor> {
    EntityPointDescriptor::new(
        IntegrationPointKey::new(key).map_err(invalid_mapping)?,
        display_name,
        kind,
        value_type,
        unit,
    )
    .map_err(invalid_mapping)
}

fn humanized(key: &str) -> String {
    let mut value = key.replace('_', " ");
    if let Some(first) = value.get_mut(0..1) {
        first.make_ascii_uppercase();
    }
    value
}

pub(crate) fn observed_value(
    point: &EntityPointDescriptor,
    domain: &str,
    state: &str,
    attributes: &BTreeMap<String, Value>,
) -> PortResult<Option<ObservedValue>> {
    if point.key().as_str() == primary_state_key(domain) {
        return map_state(point.value_type(), domain, state).map(Some);
    }
    let Some(value) = attributes.get(point.key().as_str()) else {
        return Ok(None);
    };
    map_json_value(point.value_type(), value).map(Some)
}

fn map_state(
    value_type: ObservedValueType,
    domain: &str,
    state: &str,
) -> PortResult<ObservedValue> {
    match value_type {
        ObservedValueType::Boolean => match state {
            "on" => Ok(ObservedValue::boolean(true)),
            "off" => Ok(ObservedValue::boolean(false)),
            _ if domain == "binary_sensor" => Err(invalid_data(
                "binary Home Assistant state is neither on nor off",
            )),
            _ => Err(invalid_data(
                "boolean Home Assistant state is neither on nor off",
            )),
        },
        ObservedValueType::Float64 => state
            .parse::<f64>()
            .map_err(|_| invalid_data("numeric Home Assistant state is invalid"))
            .and_then(|value| ObservedValue::float64(value).map_err(invalid_mapping)),
        ObservedValueType::Enum => ObservedValue::enumeration(state).map_err(invalid_mapping),
        ObservedValueType::String => ObservedValue::string(state).map_err(invalid_mapping),
        ObservedValueType::Int64
        | ObservedValueType::UInt64
        | ObservedValueType::Decimal
        | ObservedValueType::Bytes => Err(invalid_data(
            "unsupported Home Assistant primary state mapping",
        )),
    }
}

fn map_json_value(value_type: ObservedValueType, value: &Value) -> PortResult<ObservedValue> {
    match value_type {
        ObservedValueType::Boolean => value
            .as_bool()
            .map(ObservedValue::boolean)
            .ok_or_else(|| invalid_data("Home Assistant boolean attribute is invalid")),
        ObservedValueType::Int64 => value
            .as_i64()
            .map(ObservedValue::int64)
            .ok_or_else(|| invalid_data("Home Assistant int64 attribute is invalid")),
        ObservedValueType::UInt64 => value
            .as_u64()
            .map(ObservedValue::uint64)
            .ok_or_else(|| invalid_data("Home Assistant uint64 attribute is invalid")),
        ObservedValueType::Float64 => value
            .as_f64()
            .ok_or_else(|| invalid_data("Home Assistant float64 attribute is invalid"))
            .and_then(|value| ObservedValue::float64(value).map_err(invalid_mapping)),
        ObservedValueType::String => value
            .as_str()
            .ok_or_else(|| invalid_data("Home Assistant string attribute is invalid"))
            .and_then(|value| ObservedValue::string(value).map_err(invalid_mapping)),
        ObservedValueType::Enum => value
            .as_str()
            .ok_or_else(|| invalid_data("Home Assistant enum attribute is invalid"))
            .and_then(|value| ObservedValue::enumeration(value).map_err(invalid_mapping)),
        ObservedValueType::Decimal | ObservedValueType::Bytes => {
            Err(invalid_data("unsupported Home Assistant attribute mapping"))
        },
    }
}

pub(crate) fn invalid_mapping(error: DomainError) -> PortError {
    PortError::new(PortErrorKind::InvalidData, error.to_string())
}

pub(crate) fn invalid_data(message: &str) -> PortError {
    PortError::new(PortErrorKind::InvalidData, message)
}
