//! Protocol-name helpers for adapters shipped by this IO runtime.

use std::borrow::Cow;

/// Returns true for the built-in Modbus protocols.
pub fn is_modbus_family(protocol: &str) -> bool {
    let protocol = protocol.trim();
    protocol.eq_ignore_ascii_case("modbus")
        || protocol.eq_ignore_ascii_case("modbus_tcp")
        || protocol.eq_ignore_ascii_case("modbus_rtu")
}

/// Normalizes supported aliases and preserves an unknown name for rejection.
pub fn normalize_protocol_name(name: &str) -> Cow<'static, str> {
    let normalized = name.trim().to_lowercase().replace(['-', ' ', '.'], "_");
    match normalized.as_str() {
        "modbus" | "modbus_tcp" | "modbustcp" => Cow::Borrowed("modbus_tcp"),
        "modbus_rtu" | "modbusrtu" => Cow::Borrowed("modbus_rtu"),
        "mqtt" | "mqtt_protocol" => Cow::Borrowed("mqtt"),
        "aether_485" | "aether485" | "v485" => Cow::Borrowed("aether_485"),
        "iec61850" | "iec_61850" => Cow::Borrowed("iec61850"),
        "di_do" | "dido" | "gpio" => Cow::Borrowed("gpio"),
        "can" => Cow::Borrowed("can"),
        "http" | "https" => Cow::Borrowed("http"),
        _ => Cow::Owned(normalized),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_supported_aliases() {
        for (input, expected) in [
            ("MODBUS-TCP", "modbus_tcp"),
            ("Modbus RTU", "modbus_rtu"),
            ("Aether485", "aether_485"),
            ("IEC-61850", "iec61850"),
            ("DIDO", "gpio"),
        ] {
            assert_eq!(normalize_protocol_name(input), expected);
        }
    }

    #[test]
    fn preserves_unknown_protocol_for_literal_rejection() {
        assert_eq!(
            normalize_protocol_name("Custom.Protocol"),
            "custom_protocol"
        );
    }

    #[test]
    fn recognizes_only_modbus_family_names() {
        assert!(is_modbus_family("modbus_tcp"));
        assert!(is_modbus_family("MODBUS_RTU"));
        assert!(!is_modbus_family("sunspec_tcp"));
        assert!(!is_modbus_family("can"));
    }
}
