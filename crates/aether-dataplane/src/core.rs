//! Pure SHM infrastructure: slot storage + bitmap allocator.
//!
//! This module is the **infra boundary**: types here MUST NOT depend on
//! business concepts (channel, instance, point type, routing, action dispatch).
//! This module is the stable physical-data boundary and stays independent of
//! `aether-model`, `aether-routing`, databases, and service code.
//!
//! Business-aware code lives at the parent module (channel_index, instance_index,
//! reverse_index, dispatch, notifier, etc.) and consumes `core` as an adapter.

pub mod authority;
pub mod bitmap;
pub mod config;
pub mod header;
pub mod reader;
pub mod slot;
pub mod slot_io;
pub mod snapshot_load;
pub mod snapshot_save;
pub mod writer;
