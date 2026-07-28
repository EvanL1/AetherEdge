//! Automation configuration DTOs shared through `common` without depending on
//! the Linux-only service runtime from cross-platform tooling.

pub use common::automation_config::*;

/// Read-only instance topology projection returned by the automation API.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
pub struct TopologyNode {
    pub instance_id: u32,
    pub instance_name: String,
    pub product_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<u32>,
}
