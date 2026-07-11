//! Authoritative shared-memory store for protocol data.
//!
//! # Architecture
//!
//! ```text
//! Protocol Layer (with TransformConfig)
//!       ↓ poll_once() returns already-transformed DataBatch
//! ShmDataStore
//!       ↓
//! SHM slots + in-memory C2C routing
//! ```
//!
//! Note: Data transformation (scale/offset/reverse) is handled by the protocol layer's
//! TransformConfig in poll_once(), so `ShmDataStore` receives pre-transformed values.

mod shm_store;

pub use shm_store::ShmDataStore;
