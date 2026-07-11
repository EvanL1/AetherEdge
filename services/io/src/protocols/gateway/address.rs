//! Protocol address parsing.
//!
//! Converts shorthand address strings to `ProtocolAddress` enum variants.

use crate::protocols::core::error::{GatewayError, Result};
use crate::protocols::core::point::{
    ByteOrder, DataFormat, Iec104Address, ModbusAddress, OpcUaAddress, ProtocolAddress,
    VirtualAddress,
};

#[cfg(feature = "gpio")]
use crate::protocols::core::point::GpioAddress;

#[cfg(feature = "can")]
use crate::protocols::core::point::CanAddress;

#[cfg(feature = "dl645")]
use crate::protocols::core::point::Dl645Address;

#[cfg(feature = "ble")]
use crate::protocols::core::point::BleAddress;

#[cfg(feature = "zigbee")]
use crate::protocols::core::point::ZigbeeAddress;

#[cfg(feature = "matter")]
use crate::protocols::core::point::MatterAddress;

/// Parse a numeric field from a string, returning a config error on failure.
fn parse_field<T: std::str::FromStr>(s: &str, field: &str) -> Result<T> {
    s.parse::<T>()
        .map_err(|_| GatewayError::Config(format!("Invalid {field}: {s}")))
}

/// Parse a shorthand address string into a `ProtocolAddress`.
///
/// # Address Formats
///
/// - **Modbus**: `"slave_id:register"` or `"slave_id:register:function_code"`
///   - Example: `"1:100"` → slave_id=1, register=100, function_code=3 (default)
///   - Example: `"1:100:4"` → slave_id=1, register=100, function_code=4
///
/// - **IEC104**: `"ioa"` or `"ioa:type_id"`
///   - Example: `"1001"` → ioa=1001
///   - Example: `"1001:13"` → ioa=1001, type_id=13
///
/// - **OPC UA**: Standard OPC UA node ID format
///   - Example: `"ns=2;i=1234"` → namespace=2, node_id="i=1234"
///   - Example: `"ns=2;s=Temperature"` → namespace=2, node_id="s=Temperature"
///   - Example: `"i=1234"` → namespace=0, node_id="i=1234"
///
/// - **CAN**: `"can_id:byte_offset:bit_pos:bit_len"`
///   - Example: `"0x100:0:0:16"` → can_id=0x100, byte_offset=0, bit_pos=0, bit_len=16
///
/// - **GPIO**: `"pin_number"` or `"pin_number:direction"`
///   - Example: `"17"` → pin=17, direction=input (default)
///   - Example: `"18:output"` → pin=18, direction=output
///
/// - **Virtual**: Any string key
///   - Example: `"temperature"` → key="temperature"
pub fn parse_address(protocol: &str, address: &str) -> Result<ProtocolAddress> {
    // Use eq_ignore_ascii_case to avoid String allocation from to_lowercase()
    if crate::utils::is_modbus_family(protocol) {
        parse_modbus_address(address)
    } else if protocol.eq_ignore_ascii_case("iec104") {
        parse_iec104_address(address)
    } else if protocol.eq_ignore_ascii_case("opcua") {
        parse_opcua_address(address)
    } else if protocol.eq_ignore_ascii_case("can") {
        parse_can_address(address)
    } else if protocol.eq_ignore_ascii_case("virtual") {
        Ok(ProtocolAddress::Virtual(VirtualAddress::new(address)))
    } else {
        #[cfg(feature = "gpio")]
        if protocol.eq_ignore_ascii_case("gpio") {
            return parse_gpio_address(address);
        }
        #[cfg(feature = "dl645")]
        if protocol.eq_ignore_ascii_case("dl645") {
            return parse_dl645_address(address);
        }
        #[cfg(feature = "ble")]
        if protocol.eq_ignore_ascii_case("ble") {
            return parse_ble_address(address);
        }
        #[cfg(feature = "zigbee")]
        if protocol.eq_ignore_ascii_case("zigbee") {
            return parse_zigbee_address(address);
        }
        #[cfg(feature = "matter")]
        if protocol.eq_ignore_ascii_case("matter") {
            return parse_matter_address(address);
        }
        #[cfg(feature = "iec61850")]
        if protocol.eq_ignore_ascii_case("iec61850") {
            return parse_iec61850_address(address);
        }
        Err(GatewayError::Config(format!(
            "Unknown protocol: {}",
            protocol
        )))
    }
}

/// Parse Modbus address: "slave_id:register" or "slave_id:register:function_code"
fn parse_modbus_address(address: &str) -> Result<ProtocolAddress> {
    let mut parts = address.splitn(3, ':');

    let slave_id_str = parts
        .next()
        .ok_or_else(|| GatewayError::Config("Missing slave_id".into()))?;
    let register_str = parts.next().ok_or_else(|| {
        GatewayError::Config(format!(
            "Invalid Modbus address format: {}. Expected 'slave_id:register'",
            address
        ))
    })?;

    let slave_id = parse_field::<u8>(slave_id_str, "slave_id")?;
    let register = parse_field::<u16>(register_str, "register")?;
    let function_code = match parts.next() {
        Some(fc) => parse_field::<u8>(fc, "function_code")?,
        None => 3, // Default: holding register (FC03)
    };

    Ok(ProtocolAddress::Modbus(ModbusAddress {
        slave_id,
        register,
        function_code,
        format: DataFormat::default(),
        byte_order: ByteOrder::default(),
        bit_position: None,
    }))
}

/// Parse IEC104 address: "ioa" or "ioa:type_id"
fn parse_iec104_address(address: &str) -> Result<ProtocolAddress> {
    let (ioa_str, type_id_str) = address.split_once(':').unwrap_or((address, ""));

    let ioa = parse_field::<u32>(ioa_str, "IOA")?;
    let type_id = if type_id_str.is_empty() {
        0 // Will be inferred from data
    } else {
        parse_field::<u8>(type_id_str, "type_id")?
    };

    Ok(ProtocolAddress::Iec104(Iec104Address {
        ioa,
        type_id,
        common_address: 1,
    }))
}

/// Parse OPC UA address: "ns=N;i=ID" or "ns=N;s=Name" or "i=ID"
fn parse_opcua_address(address: &str) -> Result<ProtocolAddress> {
    let (namespace_index, node_id_str) = if address.starts_with("ns=") {
        let semi_pos = address.find(';').ok_or_else(|| {
            GatewayError::Config(format!(
                "Invalid OPC UA address format: {}. Expected 'ns=N;i=ID' or 'ns=N;s=Name'",
                address
            ))
        })?;
        let ns_idx = parse_field::<u16>(&address[3..semi_pos], "namespace")?;
        (ns_idx, &address[semi_pos + 1..])
    } else {
        (0u16, address)
    };

    if !matches!(
        node_id_str.as_bytes(),
        [b'i' | b's' | b'g' | b'b', b'=', ..]
    ) {
        return Err(GatewayError::Config(format!(
            "Invalid OPC UA node ID: {}. Expected 'i=N', 's=Name', 'g=GUID', or 'b=Base64'",
            node_id_str
        )));
    }

    Ok(ProtocolAddress::OpcUa(OpcUaAddress {
        node_id: node_id_str.to_string(),
        namespace_index,
    }))
}

/// Parse CAN address: "can_id:byte_offset:bit_pos:bit_len"
#[cfg(feature = "can")]
fn parse_can_address(address: &str) -> Result<ProtocolAddress> {
    let can_addr = CanAddress::parse(address)?;
    Ok(ProtocolAddress::Can(can_addr))
}

/// Parse CAN address (fallback when `can` feature is disabled).
#[cfg(not(feature = "can"))]
fn parse_can_address(address: &str) -> Result<ProtocolAddress> {
    // Store as Generic when CAN feature is disabled
    Ok(ProtocolAddress::Generic(address.to_string()))
}

/// Parse GPIO address: "pin_number" or "chip:pin" or "chip:pin:direction"
#[cfg(feature = "gpio")]
fn parse_gpio_address(address: &str) -> Result<ProtocolAddress> {
    // Use splitn to avoid Vec allocation
    let mut parts = address.splitn(3, ':');

    let first = parts
        .next()
        .ok_or_else(|| GatewayError::Config("Empty GPIO address".into()))?;

    match parts.next() {
        None => {
            let pin = parse_field::<u32>(first, "GPIO pin")?;
            Ok(ProtocolAddress::Gpio(GpioAddress::digital_input(
                "gpiochip0",
                pin,
            )))
        },
        Some(pin_str) => {
            let chip = first.to_string();
            let pin = parse_field::<u32>(pin_str, "GPIO pin")?;

            match parts.next() {
                None => {
                    // chip:pin
                    Ok(ProtocolAddress::Gpio(GpioAddress::digital_input(chip, pin)))
                },
                Some(dir) => {
                    // chip:pin:direction - use eq_ignore_ascii_case to avoid allocation
                    let addr = if dir.eq_ignore_ascii_case("input")
                        || dir.eq_ignore_ascii_case("in")
                        || dir.eq_ignore_ascii_case("di")
                    {
                        GpioAddress::digital_input(chip, pin)
                    } else if dir.eq_ignore_ascii_case("output")
                        || dir.eq_ignore_ascii_case("out")
                        || dir.eq_ignore_ascii_case("do")
                    {
                        GpioAddress::digital_output(chip, pin)
                    } else {
                        return Err(GatewayError::Config(format!(
                            "Invalid GPIO direction: {}. Expected 'input' or 'output'",
                            dir
                        )));
                    };
                    Ok(ProtocolAddress::Gpio(addr))
                },
            }
        },
    }
}

/// Parse DL/T 645 address: "meter_addr:data_id"
///
/// Format:
/// - meter_addr: 12-digit BCD meter address
/// - data_id: 8-character hex data identifier
///
/// Example: "123456789012:00010000" for total positive active energy
#[cfg(feature = "dl645")]
fn parse_dl645_address(address: &str) -> Result<ProtocolAddress> {
    let dl645_addr = Dl645Address::parse(address)?;
    Ok(ProtocolAddress::Dl645(dl645_addr))
}

/// Parse BLE address: "service_uuid/char_uuid" or "service_uuid/char_uuid:notify"
///
/// The UUID fields support short format (e.g., "180f") which gets expanded to
/// full 128-bit UUID at runtime by the BLE adapter.
///
/// # Examples
///
/// - `"180f/2a19"` → Battery Service / Battery Level (poll-read)
/// - `"180f/2a19:notify"` → Battery Service / Battery Level (notify subscription)
/// - `"12345678-1234-1234-1234-123456789abc/abcdef01-1234-5678-abcd-123456789abc:notify"`
#[cfg(feature = "ble")]
fn parse_ble_address(address: &str) -> Result<ProtocolAddress> {
    let address = address.trim();

    let (uuid_part, notify) = if let Some(stripped) = address.strip_suffix(":notify") {
        (stripped, true)
    } else {
        (address, false)
    };

    let (service_str, char_str) = uuid_part.split_once('/').ok_or_else(|| {
        GatewayError::Config(format!(
            "Invalid BLE address format: '{}'. Expected 'service_uuid/char_uuid' or 'service_uuid/char_uuid:notify'",
            address
        ))
    })?;

    let service_uuid = service_str.trim().to_string();
    let characteristic_uuid = char_str.trim().to_string();

    if service_uuid.is_empty() || characteristic_uuid.is_empty() {
        return Err(GatewayError::Config(format!(
            "BLE address has empty UUID component: '{}'",
            address
        )));
    }

    Ok(ProtocolAddress::Ble(BleAddress {
        service_uuid,
        characteristic_uuid,
        data_format: crate::protocols::core::point::DataFormat::default(),
        notify,
    }))
}

/// Parse Zigbee address: "ieee_addr/endpoint/cluster_id/attr_id"
///
/// Supports hex (0x prefix) and decimal for all fields.
///
/// # Examples
///
/// - `"0x00124B0018ED1234/1/0x0402/0x0000"` -- temperature measurement
/// - `"5124095622791732/1/1026/0"` -- same address in decimal
#[cfg(feature = "zigbee")]
fn parse_zigbee_address(address: &str) -> Result<ProtocolAddress> {
    let parts: Vec<&str> = address.split('/').collect();
    if parts.len() != 4 {
        return Err(GatewayError::Config(format!(
            "Invalid Zigbee address format: '{}'. Expected 'ieee_addr/endpoint/cluster_id/attr_id'",
            address
        )));
    }

    let ieee_address = parse_zigbee_u64(parts[0], "ieee_addr")?;
    let endpoint = parse_zigbee_u8(parts[1], "endpoint")?;
    let cluster_id = parse_zigbee_u16(parts[2], "cluster_id")?;
    let attribute_id = parse_zigbee_u16(parts[3], "attr_id")?;

    Ok(ProtocolAddress::Zigbee(ZigbeeAddress {
        ieee_address,
        endpoint,
        cluster_id,
        attribute_id,
    }))
}

/// Parse a u64 field with optional 0x prefix.
#[cfg(feature = "zigbee")]
fn parse_zigbee_u64(s: &str, field: &str) -> Result<u64> {
    let s = s.trim();
    if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        u64::from_str_radix(hex, 16)
            .map_err(|_| GatewayError::Config(format!("Invalid hex {field}: {s}")))
    } else {
        s.parse::<u64>()
            .map_err(|_| GatewayError::Config(format!("Invalid {field}: {s}")))
    }
}

/// Parse a u16 field with optional 0x prefix.
#[cfg(feature = "zigbee")]
fn parse_zigbee_u16(s: &str, field: &str) -> Result<u16> {
    let s = s.trim();
    if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        u16::from_str_radix(hex, 16)
            .map_err(|_| GatewayError::Config(format!("Invalid hex {field}: {s}")))
    } else {
        s.parse::<u16>()
            .map_err(|_| GatewayError::Config(format!("Invalid {field}: {s}")))
    }
}

/// Parse a u8 field with optional 0x prefix.
#[cfg(feature = "zigbee")]
fn parse_zigbee_u8(s: &str, field: &str) -> Result<u8> {
    let s = s.trim();
    if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        u8::from_str_radix(hex, 16)
            .map_err(|_| GatewayError::Config(format!("Invalid hex {field}: {s}")))
    } else {
        s.parse::<u8>()
            .map_err(|_| GatewayError::Config(format!("Invalid {field}: {s}")))
    }
}

/// Parse Matter address: "endpoint/cluster_id/attribute_id"
///
/// Supports hex (0x prefix) and decimal for cluster and attribute IDs.
///
/// # Examples
///
/// - `"1/0x0402/0x0000"` - Temperature measurement on endpoint 1
/// - `"1/6/0"` - On/Off cluster in decimal
/// - `"2/0x0201/0x0012"` - Thermostat occupied heating setpoint
#[cfg(feature = "matter")]
fn parse_matter_address(address: &str) -> Result<ProtocolAddress> {
    let parts: Vec<&str> = address.split('/').collect();
    if parts.len() != 3 {
        return Err(GatewayError::Config(format!(
            "Invalid Matter address format: '{}'. Expected 'endpoint/cluster_id/attribute_id'",
            address
        )));
    }

    let endpoint = parse_field::<u16>(parts[0], "endpoint")?;
    let cluster_id = parse_matter_id(parts[1], "cluster_id")?;
    let attribute_id = parse_matter_id(parts[2], "attribute_id")?;

    Ok(ProtocolAddress::Matter(MatterAddress::new(
        endpoint,
        cluster_id,
        attribute_id,
    )))
}

/// Parse a Matter ID field that supports both decimal and hex (0x prefix).
#[cfg(feature = "matter")]
fn parse_matter_id(s: &str, field: &str) -> Result<u32> {
    let s = s.trim();
    if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        u32::from_str_radix(hex, 16)
            .map_err(|_| GatewayError::Config(format!("Invalid hex {}: {}", field, s)))
    } else {
        s.parse::<u32>()
            .map_err(|_| GatewayError::Config(format!("Invalid {}: {}", field, s)))
    }
}

#[cfg(test)]
#[allow(clippy::disallowed_methods)] // unwrap in tests
mod tests {
    use super::*;

    #[test]
    fn test_parse_modbus_address() {
        let addr = parse_modbus_address("1:100").unwrap();
        let ProtocolAddress::Modbus(m) = addr else {
            unreachable!("parse_modbus_address always returns Modbus variant")
        };
        assert_eq!(m.slave_id, 1);
        assert_eq!(m.register, 100);
        assert_eq!(m.function_code, 3);
    }

    #[test]
    fn test_parse_modbus_address_with_function() {
        let addr = parse_modbus_address("2:200:4").unwrap();
        let ProtocolAddress::Modbus(m) = addr else {
            unreachable!("parse_modbus_address always returns Modbus variant")
        };
        assert_eq!(m.slave_id, 2);
        assert_eq!(m.register, 200);
        assert_eq!(m.function_code, 4);
    }

    #[test]
    fn test_parse_iec104_address() {
        let addr = parse_iec104_address("1001").unwrap();
        let ProtocolAddress::Iec104(i) = addr else {
            unreachable!("parse_iec104_address always returns Iec104 variant")
        };
        assert_eq!(i.ioa, 1001);
    }

    #[test]
    fn test_parse_opcua_address() {
        let addr = parse_opcua_address("ns=2;i=1234").unwrap();
        let ProtocolAddress::OpcUa(o) = addr else {
            unreachable!("parse_opcua_address always returns OpcUa variant")
        };
        assert_eq!(o.namespace_index, 2);
        assert_eq!(o.node_id, "i=1234");
    }

    #[test]
    fn test_parse_opcua_address_no_namespace() {
        let addr = parse_opcua_address("i=1234").unwrap();
        let ProtocolAddress::OpcUa(o) = addr else {
            unreachable!("parse_opcua_address always returns OpcUa variant")
        };
        assert_eq!(o.namespace_index, 0);
        assert_eq!(o.node_id, "i=1234");
    }

    #[test]
    fn test_parse_virtual_address() {
        let addr = parse_address("virtual", "temperature").unwrap();
        let ProtocolAddress::Virtual(v) = addr else {
            unreachable!("parse_address(\"virtual\", ..) always returns Virtual variant")
        };
        assert_eq!(v.tag, "temperature");
    }

    // ========== Error Path Tests ==========
    // Verify that invalid inputs return Result::Err instead of panicking

    #[test]
    fn test_parse_modbus_missing_register() {
        // Only slave_id, no colon separator → should error
        let result = parse_modbus_address("1");
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_modbus_invalid_slave_id() {
        // Non-numeric slave_id
        let result = parse_modbus_address("abc:100");
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_modbus_invalid_register() {
        // Non-numeric register
        let result = parse_modbus_address("1:xyz");
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_modbus_invalid_function_code() {
        // Non-numeric function code
        let result = parse_modbus_address("1:100:bad");
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_modbus_slave_id_overflow() {
        // u8 overflow (256 > 255)
        let result = parse_modbus_address("256:100");
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_modbus_register_overflow() {
        // u16 overflow (65536 > 65535)
        let result = parse_modbus_address("1:65536");
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_iec104_invalid_ioa() {
        let result = parse_iec104_address("not_a_number");
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_iec104_invalid_type_id() {
        // Valid IOA but invalid type_id
        let result = parse_iec104_address("1001:abc");
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_iec104_type_id_overflow() {
        // u8 overflow for type_id
        let result = parse_iec104_address("1001:256");
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_opcua_missing_semicolon() {
        // Has "ns=" prefix but no semicolon separator
        let result = parse_opcua_address("ns=2i=1234");
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_opcua_invalid_namespace() {
        let result = parse_opcua_address("ns=abc;i=1234");
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_opcua_invalid_node_id_prefix() {
        // Node ID must start with i=, s=, g=, or b=
        let result = parse_opcua_address("ns=2;x=1234");
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_opcua_no_ns_invalid_prefix() {
        // No namespace, but invalid node ID prefix
        let result = parse_opcua_address("x=1234");
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_address_unknown_protocol() {
        let result = parse_address("unknown_proto", "some_addr");
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_address_case_insensitive() {
        // Verify case-insensitive protocol matching
        assert!(parse_address("MODBUS", "1:100").is_ok());
        assert!(parse_address("Modbus", "1:100").is_ok());
        assert!(parse_address("IEC104", "1001").is_ok());
        assert!(parse_address("OPCUA", "i=1234").is_ok());
        assert!(parse_address("Virtual", "key").is_ok());
    }

    #[test]
    fn test_parse_iec104_with_type_id() {
        let addr = parse_iec104_address("2001:13").unwrap();
        let ProtocolAddress::Iec104(i) = addr else {
            unreachable!("parse_iec104_address always returns Iec104 variant")
        };
        assert_eq!(i.ioa, 2001);
        assert_eq!(i.type_id, 13);
        assert_eq!(i.common_address, 1);
    }

    #[test]
    fn test_parse_opcua_string_node_id() {
        let addr = parse_opcua_address("ns=3;s=Temperature").unwrap();
        let ProtocolAddress::OpcUa(o) = addr else {
            unreachable!("parse_opcua_address always returns OpcUa variant")
        };
        assert_eq!(o.namespace_index, 3);
        assert_eq!(o.node_id, "s=Temperature");
    }

    // ========== BLE Address Tests ==========

    #[cfg(feature = "ble")]
    #[test]
    fn test_parse_ble_address_short_uuids() {
        let addr = parse_ble_address("180f/2a19").unwrap();
        let ProtocolAddress::Ble(b) = addr else {
            unreachable!("parse_ble_address always returns Ble variant")
        };
        assert_eq!(b.service_uuid, "180f");
        assert_eq!(b.characteristic_uuid, "2a19");
        assert!(!b.notify);
    }

    #[cfg(feature = "ble")]
    #[test]
    fn test_parse_ble_address_with_notify() {
        let addr = parse_ble_address("180f/2a19:notify").unwrap();
        let ProtocolAddress::Ble(b) = addr else {
            unreachable!("parse_ble_address always returns Ble variant")
        };
        assert_eq!(b.service_uuid, "180f");
        assert_eq!(b.characteristic_uuid, "2a19");
        assert!(b.notify);
    }

    #[cfg(feature = "ble")]
    #[test]
    fn test_parse_ble_address_full_uuids() {
        let addr = parse_ble_address(
            "12345678-1234-1234-1234-123456789abc/abcdef01-1234-5678-abcd-123456789abc:notify",
        )
        .unwrap();
        let ProtocolAddress::Ble(b) = addr else {
            unreachable!("parse_ble_address always returns Ble variant")
        };
        assert_eq!(b.service_uuid, "12345678-1234-1234-1234-123456789abc");
        assert_eq!(
            b.characteristic_uuid,
            "abcdef01-1234-5678-abcd-123456789abc"
        );
        assert!(b.notify);
    }

    #[cfg(feature = "ble")]
    #[test]
    fn test_parse_ble_address_missing_slash() {
        let result = parse_ble_address("180f2a19");
        assert!(result.is_err());
    }

    #[cfg(feature = "ble")]
    #[test]
    fn test_parse_ble_address_empty_uuid() {
        let result = parse_ble_address("/2a19");
        assert!(result.is_err());
        let result = parse_ble_address("180f/");
        assert!(result.is_err());
    }

    #[cfg(feature = "ble")]
    #[test]
    fn test_parse_address_ble_via_dispatch() {
        let addr = parse_address("ble", "180f/2a19:notify").unwrap();
        let ProtocolAddress::Ble(b) = addr else {
            unreachable!("parse_address(\"ble\", ..) should return Ble variant")
        };
        assert_eq!(b.service_uuid, "180f");
        assert!(b.notify);
    }

    #[cfg(feature = "ble")]
    #[test]
    fn test_parse_address_ble_case_insensitive() {
        assert!(parse_address("BLE", "180f/2a19").is_ok());
        assert!(parse_address("Ble", "180f/2a19").is_ok());
    }

    // ========== Zigbee Address Tests ==========

    #[cfg(feature = "zigbee")]
    #[test]
    fn test_parse_zigbee_address_hex() {
        let addr = parse_zigbee_address("0x00124B0018ED1234/1/0x0402/0x0000").unwrap();
        let ProtocolAddress::Zigbee(z) = addr else {
            unreachable!("parse_zigbee_address always returns Zigbee variant")
        };
        assert_eq!(z.ieee_address, 0x00124B0018ED1234);
        assert_eq!(z.endpoint, 1);
        assert_eq!(z.cluster_id, 0x0402);
        assert_eq!(z.attribute_id, 0x0000);
    }

    #[cfg(feature = "zigbee")]
    #[test]
    fn test_parse_zigbee_address_decimal() {
        let addr = parse_zigbee_address("5124095622791732/1/1026/0").unwrap();
        let ProtocolAddress::Zigbee(z) = addr else {
            unreachable!("parse_zigbee_address always returns Zigbee variant")
        };
        assert_eq!(z.ieee_address, 5124095622791732);
        assert_eq!(z.endpoint, 1);
        assert_eq!(z.cluster_id, 1026);
        assert_eq!(z.attribute_id, 0);
    }

    #[cfg(feature = "zigbee")]
    #[test]
    fn test_parse_zigbee_address_mixed() {
        let addr = parse_zigbee_address("0x00124B0018ED1234/2/0x0006/0").unwrap();
        let ProtocolAddress::Zigbee(z) = addr else {
            unreachable!("parse_zigbee_address always returns Zigbee variant")
        };
        assert_eq!(z.ieee_address, 0x00124B0018ED1234);
        assert_eq!(z.endpoint, 2);
        assert_eq!(z.cluster_id, 0x0006);
        assert_eq!(z.attribute_id, 0);
    }

    #[cfg(feature = "zigbee")]
    #[test]
    fn test_parse_zigbee_address_invalid_format() {
        // Too few parts
        assert!(parse_zigbee_address("0x1234/1/0x0402").is_err());
        // Too many parts
        assert!(parse_zigbee_address("0x1234/1/0x0402/0/extra").is_err());
        // Invalid ieee
        assert!(parse_zigbee_address("not_a_number/1/0x0402/0").is_err());
        // Invalid endpoint
        assert!(parse_zigbee_address("0x1234/999/0x0402/0").is_err());
    }

    #[cfg(feature = "zigbee")]
    #[test]
    fn test_parse_zigbee_via_parse_address() {
        let addr = parse_address("zigbee", "0x00124B0018ED1234/1/0x0402/0x0000").unwrap();
        assert!(matches!(addr, ProtocolAddress::Zigbee(_)));
    }

    // ========== Matter Address Tests ==========

    #[cfg(feature = "matter")]
    mod matter_tests {
        use super::*;
        use crate::protocols::core::point::MatterAddress;

        #[test]
        fn test_parse_matter_address_hex() {
            let addr = parse_matter_address("1/0x0402/0x0000").unwrap();
            let ProtocolAddress::Matter(m) = addr else {
                unreachable!("parse_matter_address always returns Matter variant")
            };
            assert_eq!(m.endpoint, 1);
            assert_eq!(m.cluster_id, 0x0402);
            assert_eq!(m.attribute_id, 0x0000);
        }

        #[test]
        fn test_parse_matter_address_decimal() {
            let addr = parse_matter_address("1/6/0").unwrap();
            let ProtocolAddress::Matter(m) = addr else {
                unreachable!("parse_matter_address always returns Matter variant")
            };
            assert_eq!(m.endpoint, 1);
            assert_eq!(m.cluster_id, 6);
            assert_eq!(m.attribute_id, 0);
        }

        #[test]
        fn test_parse_matter_address_mixed() {
            let addr = parse_matter_address("2/0x0201/18").unwrap();
            let ProtocolAddress::Matter(m) = addr else {
                unreachable!("parse_matter_address always returns Matter variant")
            };
            assert_eq!(m.endpoint, 2);
            assert_eq!(m.cluster_id, 0x0201);
            assert_eq!(m.attribute_id, 18);
        }

        #[test]
        fn test_parse_matter_address_via_parse_address() {
            let addr = parse_address("matter", "1/0x0006/0x0000").unwrap();
            let ProtocolAddress::Matter(m) = addr else {
                unreachable!("matter protocol should return Matter variant")
            };
            assert_eq!(m.endpoint, 1);
            assert_eq!(m.cluster_id, 0x0006);
            assert_eq!(m.attribute_id, 0x0000);
        }

        #[test]
        fn test_parse_matter_address_case_insensitive() {
            assert!(parse_address("Matter", "1/6/0").is_ok());
            assert!(parse_address("MATTER", "1/6/0").is_ok());
        }

        #[test]
        fn test_parse_matter_address_invalid_format() {
            // Too few parts
            assert!(parse_matter_address("1/6").is_err());
            // Too many parts
            assert!(parse_matter_address("1/6/0/extra").is_err());
            // Empty
            assert!(parse_matter_address("").is_err());
        }

        #[test]
        fn test_parse_matter_address_invalid_endpoint() {
            assert!(parse_matter_address("abc/6/0").is_err());
        }

        #[test]
        fn test_parse_matter_address_invalid_hex() {
            assert!(parse_matter_address("1/0xGGGG/0").is_err());
        }

        #[test]
        fn test_matter_address_equality() {
            let a = MatterAddress::new(1, 0x0006, 0x0000);
            let b = MatterAddress::new(1, 6, 0);
            assert_eq!(a, b);

            let c = MatterAddress::new(2, 0x0006, 0x0000);
            assert_ne!(a, c);
        }
    }
}

// ── IEC 61850 ────────────────────────────────────────────────────────────────

/// Parse an IEC 61850 MMS address string.
///
/// Accepted formats:
/// - `"domain/item"` — e.g. `"simpleIOGenericIO/GGIO1$MX$AnIn1$mag$f"`
///   (slash separates domain from MMS item ID)
/// - `"domain:item"` — colon separator, already in MMS form
///
/// The item may contain `$` separators (MMS form) or `.` separators which
/// are converted to `$` automatically.
#[cfg(feature = "iec61850")]
fn parse_iec61850_address(address: &str) -> Result<ProtocolAddress> {
    use crate::protocols::core::point::Iec61850Address;

    let addr = Iec61850Address::parse(address)?;
    Ok(ProtocolAddress::Iec61850(addr))
}

#[cfg(all(test, feature = "iec61850"))]
mod iec61850_tests {
    use super::*;

    #[test]
    fn test_parse_slash_format() {
        let addr = parse_iec61850_address("simpleIOGenericIO/GGIO1$MX$AnIn1$mag$f").unwrap();
        if let ProtocolAddress::Iec61850(a) = addr {
            assert_eq!(a.domain, "simpleIOGenericIO");
            assert_eq!(a.item, "GGIO1$MX$AnIn1$mag$f");
        } else {
            panic!("wrong variant");
        }
    }

    #[test]
    fn test_parse_colon_format() {
        let addr = parse_iec61850_address("LD0:LN0$MX$Val$mag$f").unwrap();
        if let ProtocolAddress::Iec61850(a) = addr {
            assert_eq!(a.domain, "LD0");
            assert_eq!(a.item, "LN0$MX$Val$mag$f");
        } else {
            panic!("wrong variant");
        }
    }
}
