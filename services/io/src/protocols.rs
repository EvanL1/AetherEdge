//! Industrial Communication Protocol Layer
//!
//! This module provides the shared runtime boundary for the maintained Modbus,
//! MQTT, HTTP, raw CAN, GPIO, IEC 61850, and Aether-485 adapters.
//!
//! ## Design Principles
//!
//! - **Protocol-agnostic**: Unified data model and point addressing
//! - **Dual-mode support**: Polling and event-driven communication
//! - **Zero business coupling**: Pure protocol layer, free of SCADA concepts

pub mod adapters;
pub mod codec;
pub mod core;
pub mod gateway;

/// Prelude module for convenient imports
pub mod prelude {
    pub use crate::protocols::core::{
        data::*,
        error::{GatewayError, Result},
        logging::*,
        point::*,
        quality::*,
        traits::*,
    };
}

// Re-export core types at module root for convenience
pub use self::core::data::{DataBatch, DataPoint, Value};
pub use self::core::error::{GatewayError, Result};
pub use self::core::logging::{
    ChannelLogConfig, ChannelLogEvent, ChannelLogHandler, LogContext, LogEventType,
    PacketDirection, PacketMetadata,
};
pub use self::core::metadata::{
    DriverMetadata, ParameterMetadata, ParameterType, ProtocolMetadata, ProtocolRegistry,
    get_protocol_registry,
};
pub use self::core::quality::Quality;
pub use self::core::traits::{
    CommunicationMode, ConnectionState, Protocol, ProtocolCapabilities, ProtocolClient,
};

// Re-export the object-safe runtime boundary.
pub use self::gateway::{ChannelMode, ChannelRuntime};
