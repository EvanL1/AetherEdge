//! Typed runtime routing independent of persistence and transport adapters.
//!
//! This crate owns immutable C2M/M2C routing generations and the acquisition
//! owner's typed C2C index. It deliberately contains no database client, SQL,
//! table name, configuration-file parser, or cross-process transport.

mod channel;
mod snapshot;

pub use channel::{C2CTarget, ChannelRoute, ChannelRoutingCache, MAX_C2C_CASCADE_DEPTH};
pub use snapshot::{LogicalPointRoutes, PhysicalTopologySnapshot, RoutingSnapshot};
