//! Automation library exports for testing

pub mod config;

pub mod api {
    //! API Module Aggregation
    //!
    //! Organizes API handlers by functional domain under the `api/` directory.
    //!
    //! Handler groups:
    //! - routing (management + query)
    //! - instance (management + query + action)
    //! - product
    //! - health
    //! - single point APIs
    //! - admin (log level management)
    //! - cloud sync (cloud-edge synchronization)
    pub mod action_routing_boundary;
    pub mod admin_handlers;
    pub mod audit_handlers;
    pub mod cloud_sync;
    pub mod dto;
    pub mod error_response;
    pub mod global_routing_handlers;
    pub mod health_handlers;
    pub mod http_boundary;
    pub mod instance_management_handlers;
    pub mod instance_query_handlers;
    pub mod measurement_routing_boundary;
    pub mod product_handlers;
    pub mod property_handlers;
    pub mod routing_query_handlers;
    pub mod rule_routes;
    pub mod single_point_handlers;

    // Re-export dto/routes for convenience
    pub use crate::routes;
}
pub mod infra {
    //! Infrastructure layer — SHM-backed external side effects
    pub mod action_routing;
    pub mod application_control;
    pub mod measurement_routing;
    pub mod rule_live_state;
    pub mod rule_mutation;
    pub mod rule_queries;
    pub mod rule_runtime;
    pub mod runtime_topology;
}
pub mod app_state;
pub mod bootstrap;
pub mod error;
pub mod instance_configuration;
pub mod instance_manager;
pub mod instance_query;
// Extension impl blocks for InstanceManager (split for maintainability)
mod instance_data;
mod instance_routing;
pub mod product_loader;
pub mod routes;
pub mod routing_loader;

// Re-export commonly used types
pub use error::{AutomationError, Result};
pub use instance_manager::InstanceManager;
pub use product_loader::{
    ActionPoint, CreateInstanceRequest, Instance, MeasurementPoint, PointRole, Product,
    ProductLoader, PropertyTemplate,
};
