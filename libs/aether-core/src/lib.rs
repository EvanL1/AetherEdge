//! # aether-core
//!
//! Core types and codecs for AetherEMS firmware and gateway.
//!
//! This crate is `no_std` compatible by default, enabling it to run on:
//! - Bare-metal MCU firmware (Cortex-M, RISC-V)
//! - RTOS environments (FreeRTOS, Zephyr)
//! - Linux user-space gateway services
//!
//! ## Features
//!
//! - `std` (default): Enable standard library support
//! - `serde`: Enable serialization (requires `std`)
//!
//! ## Module Structure
//!
//! ```text
//! aether-core/
//! ├── types    - Basic types (PointType, Value, Quality)
//! ├── codec    - Protocol encoders/decoders (DL645, CAN frames)
//! ├── frame    - Protocol frame definitions
//! └── shm      - Shared memory layout definitions
//! ```

#![cfg_attr(not(feature = "std"), no_std)]

// Re-export core types for no_std compatibility
#[cfg(not(feature = "std"))]
extern crate core;

pub mod codec;
pub mod frame;
pub mod shm;
pub mod types;

// Re-exports for convenience
pub use shm::{HEADER_SIZE, PointSlot, SHM_MAGIC, SLOT_SIZE, ShmHeader};
pub use types::{ParsePointTypeError, PointType, Quality, Value};
