//! Parsing for the protocol addresses shipped by the standard IO runtime.

use crate::protocols::core::error::{GatewayError, Result};
use crate::protocols::core::point::{ByteOrder, DataFormat, ModbusAddress, ProtocolAddress};

#[cfg(feature = "can")]
use crate::protocols::core::point::CanAddress;
#[cfg(feature = "gpio")]
use crate::protocols::core::point::GpioAddress;

fn parse_field<T: std::str::FromStr>(value: &str, field: &str) -> Result<T> {
    value
        .parse::<T>()
        .map_err(|_| GatewayError::Config(format!("Invalid {field}: {value}")))
}

/// Parses one address for a protocol included in this IO build.
pub fn parse_address(protocol: &str, address: &str) -> Result<ProtocolAddress> {
    if crate::utils::is_modbus_family(protocol) {
        return parse_modbus_address(address);
    }
    if protocol.eq_ignore_ascii_case("can") {
        return parse_can_address(address);
    }
    #[cfg(feature = "gpio")]
    if protocol.eq_ignore_ascii_case("gpio") {
        return parse_gpio_address(address);
    }
    #[cfg(feature = "iec61850")]
    if protocol.eq_ignore_ascii_case("iec61850") {
        return parse_iec61850_address(address);
    }

    Err(GatewayError::Config(format!(
        "Unknown protocol: {protocol}"
    )))
}

fn parse_modbus_address(address: &str) -> Result<ProtocolAddress> {
    let mut parts = address.splitn(3, ':');
    let slave_id = parse_field::<u8>(
        parts
            .next()
            .ok_or_else(|| GatewayError::Config("Missing slave_id".into()))?,
        "slave_id",
    )?;
    let register = parse_field::<u16>(
        parts.next().ok_or_else(|| {
            GatewayError::Config(format!(
                "Invalid Modbus address format: {address}. Expected 'slave_id:register'"
            ))
        })?,
        "register",
    )?;
    let function_code = parts
        .next()
        .map(|value| parse_field::<u8>(value, "function_code"))
        .transpose()?
        .unwrap_or(3);

    Ok(ProtocolAddress::Modbus(ModbusAddress {
        slave_id,
        register,
        function_code,
        format: DataFormat::default(),
        byte_order: ByteOrder::default(),
        bit_position: None,
    }))
}

#[cfg(feature = "can")]
fn parse_can_address(address: &str) -> Result<ProtocolAddress> {
    Ok(ProtocolAddress::Can(CanAddress::parse(address)?))
}

#[cfg(not(feature = "can"))]
fn parse_can_address(address: &str) -> Result<ProtocolAddress> {
    Ok(ProtocolAddress::Generic(address.to_string()))
}

#[cfg(feature = "gpio")]
fn parse_gpio_address(address: &str) -> Result<ProtocolAddress> {
    let mut parts = address.splitn(3, ':');
    let first = parts
        .next()
        .ok_or_else(|| GatewayError::Config("Empty GPIO address".into()))?;

    let Some(pin_value) = parts.next() else {
        let pin = parse_field::<u32>(first, "GPIO pin")?;
        return Ok(ProtocolAddress::Gpio(GpioAddress::digital_input(
            "gpiochip0",
            pin,
        )));
    };

    let pin = parse_field::<u32>(pin_value, "GPIO pin")?;
    let Some(direction) = parts.next() else {
        return Ok(ProtocolAddress::Gpio(GpioAddress::digital_input(
            first, pin,
        )));
    };

    let address = if direction.eq_ignore_ascii_case("input")
        || direction.eq_ignore_ascii_case("in")
        || direction.eq_ignore_ascii_case("di")
    {
        GpioAddress::digital_input(first, pin)
    } else if direction.eq_ignore_ascii_case("output")
        || direction.eq_ignore_ascii_case("out")
        || direction.eq_ignore_ascii_case("do")
    {
        GpioAddress::digital_output(first, pin)
    } else {
        return Err(GatewayError::Config(format!(
            "Invalid GPIO direction: {direction}. Expected 'input' or 'output'"
        )));
    };
    Ok(ProtocolAddress::Gpio(address))
}

#[cfg(feature = "iec61850")]
fn parse_iec61850_address(address: &str) -> Result<ProtocolAddress> {
    use crate::protocols::core::point::Iec61850Address;

    Ok(ProtocolAddress::Iec61850(Iec61850Address::parse(address)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_modbus_address_and_optional_function() {
        let ProtocolAddress::Modbus(default_function) =
            parse_modbus_address("1:100").expect("parse Modbus address")
        else {
            panic!("expected Modbus address")
        };
        assert_eq!(default_function.slave_id, 1);
        assert_eq!(default_function.register, 100);
        assert_eq!(default_function.function_code, 3);

        let ProtocolAddress::Modbus(explicit_function) =
            parse_modbus_address("2:200:4").expect("parse Modbus address with function")
        else {
            panic!("expected Modbus address")
        };
        assert_eq!(explicit_function.function_code, 4);
    }

    #[test]
    fn rejects_invalid_modbus_addresses() {
        for address in ["1", "abc:100", "1:xyz", "1:100:bad", "256:100"] {
            assert!(parse_modbus_address(address).is_err(), "accepted {address}");
        }
    }

    #[test]
    fn rejects_unknown_protocol() {
        assert!(parse_address("unknown", "value").is_err());
    }
}

#[cfg(all(test, feature = "iec61850"))]
mod iec61850_tests {
    use super::*;

    #[test]
    fn parses_slash_and_colon_formats() {
        for address in [
            "simpleIOGenericIO/GGIO1$MX$AnIn1$mag$f",
            "simpleIOGenericIO:GGIO1.MX.AnIn1.mag.f",
        ] {
            assert!(
                matches!(
                    parse_iec61850_address(address),
                    Ok(ProtocolAddress::Iec61850(_))
                ),
                "rejected {address}"
            );
        }
    }
}
