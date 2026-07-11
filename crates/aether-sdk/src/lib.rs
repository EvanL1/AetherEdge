//! Stable public facade for embedding the Aether edge kernel.

mod builder;

pub use builder::{AetherBuilder, BuildError};

/// Transport-neutral application API.
pub mod application {
    pub use aether_application::*;
}

/// Industry-neutral domain types.
pub mod domain {
    pub use aether_domain::*;
}

/// Capability ports implemented by user-selected adapters.
pub mod ports {
    pub use aether_ports::*;
}
