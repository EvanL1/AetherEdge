//! Versioned beta facade for embedding the Aether edge kernel.

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

/// Versioned, fail-closed domain-pack loading contract.
pub mod pack {
    pub use aether_pack::*;
}

/// Capability ports implemented by user-selected adapters.
pub mod ports {
    pub use aether_ports::*;
}

/// Zero-external-service adapters for composing one local edge runtime.
#[cfg(feature = "local-runtime")]
pub mod local {
    pub use aether_store_local::*;
}
