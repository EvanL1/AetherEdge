//! Industrial Communication Protocol Layer
//!
//! This module provides a unified industrial protocol abstraction supporting multiple communication protocols:
//! - Modbus TCP/RTU
//! - IEC 60870-5-104
//! - OPC UA
//! - MQTT
//! - HTTP
//! - DL/T 645-2007
//! - CAN/J1939
//! - GPIO
//!
//! ## Design Principles
//!
//! - **Protocol-agnostic**: Unified data model and point addressing
//! - **Dual-mode support**: Polling and event-driven communication
//! - **Zero business coupling**: Pure protocol layer, free of SCADA concepts

pub mod adapters;
pub mod codec;
pub mod core;
pub(crate) mod factory;
pub mod runtime;

/// Prelude module for convenient imports
pub mod prelude {
    pub use crate::protocols::core::{
        data::*,
        error::{GatewayError, Result},
        logging::*,
        point::*,
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
    DriverMetadata, HasMetadata, ParameterMetadata, ParameterType, ProtocolMetadata,
};
pub use self::core::traits::{CommunicationMode, ConnectionState};

pub use self::runtime::ChannelRuntime;
pub use factory::{ProtocolRegistry, get_protocol_registry};
