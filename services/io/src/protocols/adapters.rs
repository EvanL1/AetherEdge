//! Protocol implementations.
//!
//! This module contains adapters that integrate protocol crates with the protocol layer.

#[cfg(any(feature = "mqtt", feature = "http"))]
pub(crate) mod json_mapper;

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

// In-process Modbus simulator is test-only; production simulation lives in tools/simulator.
#[cfg(all(test, feature = "modbus"))]
pub mod modbus_mock;

#[cfg(feature = "iec104")]
pub mod iec104;

#[cfg(feature = "opcua")]
pub mod opcua;

// CAN configuration and decoding are cross-platform; the socket client remains Linux-only.
#[cfg(feature = "can")]
pub mod can;

#[cfg(all(feature = "gpio", target_os = "linux"))]
pub mod gpio;

#[cfg(feature = "dl645")]
pub mod dl645;

#[cfg(feature = "bacnet")]
pub mod bacnet;

#[cfg(feature = "cjt188")]
pub mod cjt188;

#[cfg(feature = "iec101")]
pub mod iec101;

// Accept-loop, connection-bound and rebind plumbing shared by the two dial-in
// terminal servers below.
#[cfg(any(feature = "gb32960", feature = "jt808"))]
pub(crate) mod tcp_terminal_server;

#[cfg(feature = "gb32960")]
pub mod gb32960;

#[cfg(feature = "jt808")]
pub mod jt808;

#[cfg(feature = "mqtt")]
pub mod mqtt;

#[cfg(feature = "http")]
pub mod http;

#[cfg(feature = "ble")]
pub mod ble;

#[cfg(feature = "ble")]
pub mod ble_config;

#[cfg(feature = "zigbee")]
pub mod zigbee;

#[cfg(feature = "zigbee")]
pub mod zigbee_config;

#[cfg(feature = "zigbee")]
pub mod zigbee_codec;

#[cfg(feature = "iec61850")]
pub mod iec61850;
