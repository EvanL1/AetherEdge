//! SHM/UDS Action Dispatch — re-exported from aether-rtdb-shm.
//!
//! The trait and ShmDispatch implementation live in aether-rtdb-shm so the
//! rules executor (libs/aether-rules) can use the same generation-checked
//! writer as automation's HTTP /control path. Without sharing, the rules
//! executor previously held its own raw UnifiedWriter that silently kept
//! writing to stale slots after a io restart.

pub use aether_rtdb_shm::{ActionDispatch, DispatchOutcome, NoopDispatch, ShmDispatch};
