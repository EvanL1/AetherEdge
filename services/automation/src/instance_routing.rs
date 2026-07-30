//! Read-only instance routing queries.
//!
//! Logical routing mutations enter the typed measurement/action routing
//! applications. `InstanceManager` deliberately exposes no direct mutation
//! helper, including in test builds.

use anyhow::Result;

use crate::routing_loader::{RoutingScope, RoutingSnapshot, load_routing_snapshot};

use super::instance_manager::InstanceManager;

impl InstanceManager {
    /// Load desired routes and their CAS head from one SQLite read snapshot.
    pub(crate) async fn routing_snapshot(&self, scope: RoutingScope) -> Result<RoutingSnapshot> {
        load_routing_snapshot(&self.pool, scope).await
    }
}
