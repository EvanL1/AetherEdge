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
//! - Virtual Channel
//!
//! ## Design Principles
//!
//! - **Protocol-agnostic**: Unified data model and point addressing
//! - **Dual-mode support**: Polling and event-driven communication
//! - **Zero business coupling**: Pure protocol layer, free of SCADA concepts

pub mod adapters;
pub mod codec;
pub mod config;
pub mod core;
pub mod gateway;

#[cfg(feature = "modbus")]
pub mod sunspec;

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
    LoggableProtocol, PacketDirection, PacketMetadata,
};
pub use self::core::metadata::{
    DriverMetadata, HasMetadata, ParameterMetadata, ParameterType, ProtocolMetadata,
    ProtocolRegistry, get_protocol_registry,
};
pub use self::core::quality::Quality;
pub use self::core::traits::{
    CommunicationMode, ConnectionState, Protocol, ProtocolCapabilities, ProtocolClient,
};

// Re-export config types
pub use self::config::ChannelBuildResult;

// Re-export gateway types
pub use self::gateway::{
    ChannelConfig, ChannelMode, ChannelModeConfig, ChannelRuntime, ConfigError, GatewayConfig,
    GatewayGlobalConfig, PointDef, parse_address,
};
