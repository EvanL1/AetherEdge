//! Automation service configuration structures (re-exported from aether-config).
//!
//! Config schema lives in the platform-agnostic `aether-config` crate so that
//! `aether.exe` can be built on Windows without pulling in Linux-only
//! service runtime code (SHM, UDS).

pub use aether_config::automation::*;
