//! In-memory routing indexes and SHM data-plane routing metadata.
//!
//! Routing configuration is loaded from SQLite and atomically published in
//! [`RoutingCache`]. This crate performs no live-value storage and contains no
//! Redis fallback.

pub mod loader;
pub mod routing_cache;

pub use loader::{RoutingMaps, load_routing_maps};
pub use routing_cache::{C2CTarget, C2MTarget, M2CTarget, RoutingCache, RoutingCacheStats};

/// Maximum number of C2C forwarding hops.
pub const MAX_C2C_CASCADE_DEPTH: u8 = 2;
