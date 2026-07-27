//! Protocol implementations.
//!
//! This module contains adapters that integrate protocol crates with the protocol layer.

// Cross-platform CAN types and decoder (no hardware dependency)
pub mod can_decoder;
pub mod can_types;

// Modbus TCP + RTU support
#[cfg(feature = "modbus")]
pub mod modbus;

#[cfg(feature = "modbus")]
pub mod modbus_config;

#[cfg(feature = "modbus")]
pub mod modbus_client;

#[cfg(feature = "modbus")]
pub mod modbus_logging;

#[cfg(feature = "modbus")]
pub mod modbus_poll;

#[cfg(feature = "modbus")]
pub mod command_batcher;

// In-process Modbus simulator is test-only; production simulation lives in tools/simulator.
#[cfg(all(test, feature = "modbus"))]
pub mod modbus_mock;

#[cfg(all(feature = "can", target_os = "linux"))]
pub mod can;

#[cfg(all(feature = "gpio", target_os = "linux"))]
pub mod gpio;

#[cfg(feature = "aether_485")]
pub mod aether_485;

#[cfg(feature = "mqtt")]
pub mod mqtt;

#[cfg(feature = "http")]
pub mod http;

#[cfg(feature = "iec61850")]
pub mod iec61850;
