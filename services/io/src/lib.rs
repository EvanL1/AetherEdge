//! Communication Service Library
//!
//! Industrial communication service providing unified interface for various protocols

// Allow large error types - AppError contains rich error context by design
#![allow(clippy::result_large_err)]

// Module declarations
pub mod automatic_reconciliation;
pub mod channel_mutator;
pub mod error;
pub mod home_assistant;
#[cfg(feature = "home-assistant-integration-control")]
mod home_assistant_control;
pub mod point_topology;
pub mod protocols;
pub mod utils;

pub mod api {
    //! REST API Module
    //!
    //! Provides HTTP API endpoints for the communication service.

    pub mod command_cache;
    pub mod dto;
    pub mod routes;

    pub mod handlers {
        pub mod admin_handlers;
        pub mod channel_handlers;
        pub mod channel_management_handlers;
        pub mod control_handlers;
        pub mod health;
        pub mod mapping_handlers;
        pub mod network_handlers;
        pub mod point_handlers;
        pub mod protocol_handlers;
        pub mod provision_handlers;
        pub mod template_handlers;
    }
}

// Inline module declarations to avoid extra thin shell files
pub mod core {
    pub mod bootstrap;
    pub mod channels;
    pub mod config;
}

pub mod store;

// Re-export the authoritative live-state writer.
pub use store::ShmDataStore;

// Protocol implementations are in crate::protocols:
// - modbus_tcp/rtu: protocols::ModbusChannel (TCP and RTU modes)
// - iec104: protocols::Iec104Channel
// - opcua: protocols::OpcuaChannel
// - mqtt: protocols::MqttChannel
// - http: protocols::HttpChannel
// - dl645: protocols::Dl645Channel
// - can/j1939: protocols::CanChannel
// - gpio: protocols::GpioChannel
// - virtual: protocols::VirtualChannel

pub mod runtime {
    //! Runtime Orchestration Layer
    //!
    //! Provides runtime lifecycle management, service orchestration, reconnection mechanisms,
    //! and maintenance tasks for the communication service.

    pub mod lifecycle;
    pub mod reconnect;

    #[cfg(test)]
    pub mod test_utils;

    // Re-export common types
    pub use lifecycle::{
        shutdown_handler, shutdown_services, start_cleanup_task, start_communication_service,
        wait_for_shutdown,
    };
    pub use reconnect::{ReconnectContext, ReconnectError, ReconnectHelper, ReconnectPolicy};
}

// Re-export dto at crate root for compatibility
pub use crate::api::dto;
pub use channel_mutator::{ChannelRuntimeLifecycle, SqliteChannelMutator};

// Re-export commonly used types
pub use error::{ErrorExt, IoError, Result};

// Re-export core functionality
pub use core::bootstrap::ServiceArgs;
pub use core::channels::ChannelManager;
pub use core::config::ConfigManager;

// Re-export runtime helpers for convenience
pub use runtime::{shutdown_services, wait_for_shutdown};

#[cfg(test)]
pub use runtime::test_utils;
