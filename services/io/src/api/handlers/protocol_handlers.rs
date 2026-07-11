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
        }
    }
}

/// List all available protocols and drivers
///
/// Returns metadata about all protocols supported by this service,
/// including their drivers, configuration parameters, and example configs.
#[utoipa::path(
    get,
    path = "/api/protocols",
    responses(
        (status = 200, description = "List of available protocols", body = Vec<ProtocolInfo>)
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
