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
    pub mod cloud_sync;
    pub mod global_routing_handlers;
    pub mod health_handlers;
    pub mod instance_management_handlers;
    pub mod instance_query_handlers;
    pub mod product_handlers;
    pub mod property_handlers;
    pub mod routing_management_handlers;
    pub mod routing_query_handlers;
    pub mod single_point_handlers;

    // Re-export dto/routes for convenience
    pub use crate::routes;
}
// Map dto module to api/dto.rs while keeping crate::dto path stable
pub mod infra {
    //! Infrastructure layer — SHM-backed external side effects
    pub mod action_routing;
    pub mod application_control;
    pub mod channel_health;
    pub mod rule_live_state;
    pub mod rule_mutation;
    pub mod shm_manifest;
}
pub mod runtime {
    //! Runtime layer — in-memory caches and SHM slot management
}

pub mod app_state;
pub mod bootstrap;
#[path = "api/dto.rs"]
pub mod dto;
pub mod error;
pub mod instance_manager;
// Extension impl blocks for InstanceManager (split for maintainability)
mod instance_data;
mod instance_routing;
pub mod product_loader;
pub mod reload;
pub mod routes;
pub mod routing_loader;

// Rule Engine - local routes module
pub mod rule_routes;

// Re-export Rule Engine types from aether-rules library
pub use aether_rules::{
    ActionResult, DEFAULT_TICK_MS, Result as RuleResult, RuleError, RuleExecutionResult,
    RuleExecutor, RuleScheduler, SchedulerStatus, TriggerConfig, delete_rule, extract_rule_flow,
    get_rule, get_rule_for_execution, list_rules, load_all_rules, load_enabled_rules,
    set_rule_enabled, upsert_rule,
};

// Re-export routing primitives from the shared library.
pub use aether_routing::{RouteContext, route_context_from_target, validate_action_value};

// Re-export commonly used types
pub use error::{AutomationError, Result};
pub use instance_manager::InstanceManager;
pub use product_loader::{
    ActionPoint, CreateInstanceRequest, Instance, MeasurementPoint, PointRole, Product,
    ProductHierarchy, ProductLoader, PropertyTemplate,
};
pub use routing_loader::{
    ActionRouting, ActionRoutingRow, MeasurementRouting, MeasurementRoutingRow,
};
