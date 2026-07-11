//! # aether-config
//!
//! Cross-platform configuration schema for AetherEMS services.
//!
//! This crate contains the YAML / SQLite-backed configuration structs
//! consumed by `io`, `automation`, and the `aether` management CLI.
//! It is deliberately **platform-agnostic** (no SHM, UDS, or service
//! runtime code) so that `aether.exe` can be built on Windows without
//! pulling in the Linux-only service runtimes.
//!
//! ## Layout
//!
//! - [`io`] — `IoConfig`, `ChannelConfig`, point definitions
//! - [`automation`] — `AutomationConfig`, `RulesConfig`, rule metadata
//!
//! Each service crate re-exports from this crate to preserve existing
//! internal paths (`io::core::config::*`, `automation::config::*`).

pub mod automation;
pub mod io;
