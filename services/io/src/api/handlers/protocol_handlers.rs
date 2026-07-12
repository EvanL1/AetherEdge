//! Protocol Metadata Handlers
//!
//! Provides endpoints for discovering available protocols and their configuration options.

use crate::protocols::{DriverMetadata, ProtocolMetadata, get_protocol_registry};
use axum::response::Json;
use serde::Serialize;

use crate::dto::{AppError, SuccessResponse};

/// Protocol information for API response.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct ProtocolInfo {
    /// Protocol name.
    pub name: String,
    /// Human-readable display name.
    pub display_name: String,
    /// Description of the protocol.
    pub description: String,
    /// Protocol type identifier (e.g., "modbus_tcp", "di_do").
    pub protocol_type: String,
    /// Whether this protocol supports point configuration.
    pub supports_points: bool,
    /// Available drivers for this protocol.
    pub drivers: Vec<DriverInfo>,
}

/// Driver information for API response.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct DriverInfo {
    /// Driver name.
    pub name: String,
    /// Human-readable display name.
    pub display_name: String,
    /// Description of the driver.
    pub description: String,
    /// Whether this is the recommended driver.
    pub is_recommended: bool,
    /// Example configuration JSON.
    pub example_config: serde_json::Value,
    /// Available configuration parameters.
    pub parameters: Vec<ParameterInfo>,
}

/// Parameter information for API response.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct ParameterInfo {
    /// Parameter name.
    pub name: String,
    /// Human-readable display name.
    pub display_name: String,
    /// Description of the parameter.
    pub description: String,
    /// Whether this parameter is required.
    pub required: bool,
    /// Default value if not specified.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_value: Option<serde_json::Value>,
    /// Type of the parameter.
    pub param_type: String,
    /// Inclusive numeric minimum when `param_type` is `integer`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub minimum: Option<u64>,
    /// Inclusive numeric maximum when `param_type` is `integer`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub maximum: Option<u64>,
    /// Minimum string length when `param_type` is `string`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min_length: Option<usize>,
}

impl From<&ProtocolMetadata> for ProtocolInfo {
    fn from(meta: &ProtocolMetadata) -> Self {
        Self {
            name: meta.name.to_string(),
            display_name: meta.display_name.to_string(),
            description: meta.description.to_string(),
            protocol_type: meta.protocol_type.to_string(),
            supports_points: meta.supports_points,
            drivers: meta.drivers.iter().map(DriverInfo::from).collect(),
        }
    }
}

impl From<&DriverMetadata> for DriverInfo {
    fn from(meta: &DriverMetadata) -> Self {
        Self {
            name: meta.name.to_string(),
            display_name: meta.display_name.to_string(),
            description: meta.description.to_string(),
            is_recommended: meta.is_recommended,
            example_config: meta.example_config.clone(),
            parameters: meta.parameters.iter().map(ParameterInfo::from).collect(),
        }
    }
}

impl From<&crate::protocols::ParameterMetadata> for ParameterInfo {
    fn from(meta: &crate::protocols::ParameterMetadata) -> Self {
        Self {
            name: meta.name.to_string(),
            display_name: meta.display_name.to_string(),
            description: meta.description.to_string(),
            required: meta.required,
            default_value: meta.default_value.clone(),
            param_type: format!("{:?}", meta.param_type).to_lowercase(),
            minimum: meta.minimum,
            maximum: meta.maximum,
            min_length: meta.min_length,
        }
    }
}

/// List all available protocols and drivers
///
/// Returns metadata about all protocols supported by this service,
/// including their drivers, configuration parameters, and example configs.
/// Parameter types and bounds are validation rules, with no type coercion or
/// fallback for missing/invalid required values. The Modbus contracts are:
/// TCP `host: non-empty string`, `port: integer 1..65535`; RTU
/// `device: non-empty string`, `baud_rate: integer 1..4294967295`.
/// Both transports accept optional `poll_interval_ms: integer 1..86400000`
/// and `read_timeout_ms: integer 1..86400000`.
#[utoipa::path(
    get,
    path = "/api/protocols",
    responses(
        (status = 200, description = "Available protocols and their exact validated configuration contracts", body = common::SuccessResponse<Vec<ProtocolInfo>>)
    ),
    tag = "io"
)]
pub async fn list_protocols() -> Result<Json<SuccessResponse<Vec<ProtocolInfo>>>, AppError> {
    let registry = get_protocol_registry();
    let protocols: Vec<ProtocolInfo> = registry
        .protocols()
        .iter()
        .map(ProtocolInfo::from)
        .collect();
    Ok(Json(SuccessResponse::new(protocols)))
}

#[cfg(all(test, feature = "modbus"))]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    fn protocol<'a>(protocols: &'a [ProtocolInfo], protocol_type: &str) -> &'a ProtocolInfo {
        protocols
            .iter()
            .find(|protocol| protocol.protocol_type == protocol_type)
            .unwrap_or_else(|| panic!("missing {protocol_type} protocol metadata"))
    }

    fn parameter<'a>(driver: &'a DriverInfo, name: &str) -> &'a ParameterInfo {
        driver
            .parameters
            .iter()
            .find(|parameter| parameter.name == name)
            .unwrap_or_else(|| panic!("missing {name} parameter in {}", driver.name))
    }

    fn assert_required_string(parameter: &ParameterInfo) {
        assert_eq!(parameter.param_type, "string");
        assert!(parameter.required);
        assert_eq!(parameter.default_value, None);
        assert_eq!(parameter.min_length, Some(1));
        assert_eq!(parameter.minimum, None);
        assert_eq!(parameter.maximum, None);
    }

    fn assert_required_integer(parameter: &ParameterInfo, maximum: u64) {
        assert_eq!(parameter.param_type, "integer");
        assert!(parameter.required);
        assert_eq!(parameter.default_value, None);
        assert_eq!(parameter.minimum, Some(1));
        assert_eq!(parameter.maximum, Some(maximum));
        assert_eq!(parameter.min_length, None);
    }

    fn assert_exact_parameter_surface(driver: &DriverInfo, expected: &[&str]) {
        let expected = expected.iter().copied().collect::<BTreeSet<_>>();
        let parameters = driver
            .parameters
            .iter()
            .map(|parameter| parameter.name.as_str())
            .collect::<BTreeSet<_>>();
        let examples = driver
            .example_config
            .as_object()
            .expect("driver example object")
            .keys()
            .map(String::as_str)
            .collect::<BTreeSet<_>>();
        assert_eq!(parameters, expected, "advertised parameter surface");
        assert_eq!(examples, expected, "example parameter surface");
    }

    #[tokio::test]
    async fn protocol_discovery_separates_strict_modbus_tcp_and_rtu_contracts() {
        let Json(response) = list_protocols().await.expect("protocol discovery");

        for protocol_type in ["modbus_tcp", "sunspec_tcp"] {
            let driver = &protocol(&response.data, protocol_type).drivers[0];
            assert_required_string(parameter(driver, "host"));
            assert_required_integer(parameter(driver, "port"), u64::from(u16::MAX));
            assert!(driver.example_config["host"].is_string());
            assert!(driver.example_config["port"].is_u64());
            assert!(driver.example_config.get("device").is_none());
            assert!(driver.example_config.get("baud_rate").is_none());
            assert_exact_parameter_surface(
                driver,
                &["host", "port", "read_timeout_ms", "poll_interval_ms"],
            );
        }

        for protocol_type in ["modbus_rtu", "sunspec_rtu"] {
            let driver = &protocol(&response.data, protocol_type).drivers[0];
            assert_required_string(parameter(driver, "device"));
            assert_required_integer(parameter(driver, "baud_rate"), u64::from(u32::MAX));
            assert!(driver.example_config["device"].is_string());
            assert!(driver.example_config["baud_rate"].is_u64());
            assert!(driver.example_config.get("host").is_none());
            assert!(driver.example_config.get("port").is_none());
            assert_exact_parameter_surface(
                driver,
                &["device", "baud_rate", "read_timeout_ms", "poll_interval_ms"],
            );
        }

        for protocol_type in ["modbus_tcp", "modbus_rtu", "sunspec_tcp", "sunspec_rtu"] {
            let driver = &protocol(&response.data, protocol_type).drivers[0];
            let poll = parameter(driver, "poll_interval_ms");
            assert_eq!(poll.minimum, Some(1));
            assert_eq!(poll.maximum, Some(86_400_000));
            let timeout = parameter(driver, "read_timeout_ms");
            assert_eq!(timeout.minimum, Some(1));
            assert_eq!(timeout.maximum, Some(86_400_000));
        }
    }
}
