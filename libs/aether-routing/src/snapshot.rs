//! Immutable typed routing generations independent of their persistence adapter.

use std::collections::BTreeMap;
use std::hash::{Hash, Hasher};

use aether_ports::{PortError, PortErrorKind, PortResult};
use aether_shm_bridge::{ChannelHealthManifest, ChannelPointManifest, PhysicalPointAddress};
use rustc_hash::FxHasher;

/// Deterministically ordered logical instance route map.
pub type LogicalPointRoutes = BTreeMap<(u32, u32), PhysicalPointAddress>;

/// Physical point and channel-health topology observed as one configuration fact.
#[derive(Debug, Clone)]
pub struct PhysicalTopologySnapshot {
    point_manifest: ChannelPointManifest,
    health_manifest: ChannelHealthManifest,
}

impl PhysicalTopologySnapshot {
    /// Creates one physical topology snapshot.
    #[must_use]
    pub const fn new(
        point_manifest: ChannelPointManifest,
        health_manifest: ChannelHealthManifest,
    ) -> Self {
        Self {
            point_manifest,
            health_manifest,
        }
    }

    /// Returns the deterministic T/S/C/A slot manifest.
    #[must_use]
    pub const fn point_manifest(&self) -> &ChannelPointManifest {
        &self.point_manifest
    }

    /// Returns the configured-channel health manifest.
    #[must_use]
    pub const fn health_manifest(&self) -> &ChannelHealthManifest {
        &self.health_manifest
    }

    /// Splits this snapshot into owned manifests.
    #[must_use]
    pub fn into_manifests(self) -> (ChannelPointManifest, ChannelHealthManifest) {
        (self.point_manifest, self.health_manifest)
    }
}

/// Physical topology and logical C2M/M2C routes published as one immutable generation.
#[derive(Debug, Clone)]
pub struct RoutingSnapshot {
    physical: PhysicalTopologySnapshot,
    configured_physical_points: Vec<PhysicalPointAddress>,
    measurement_routes: LogicalPointRoutes,
    action_routes: LogicalPointRoutes,
    digest: u64,
}

impl RoutingSnapshot {
    /// Validates and creates a routing generation from adapter-independent facts.
    pub fn new(
        physical: PhysicalTopologySnapshot,
        mut configured_physical_points: Vec<PhysicalPointAddress>,
        measurement_routes: LogicalPointRoutes,
        action_routes: LogicalPointRoutes,
    ) -> PortResult<Self> {
        configured_physical_points.sort_unstable_by_key(|address| {
            (
                address.channel_id().get(),
                point_kind_order(address.kind()),
                address.point_id().get(),
            )
        });
        configured_physical_points.dedup();
        validate_routes(
            "measurement routing",
            &measurement_routes,
            false,
            physical.point_manifest(),
            &configured_physical_points,
        )?;
        validate_routes(
            "action routing",
            &action_routes,
            true,
            physical.point_manifest(),
            &configured_physical_points,
        )?;
        let digest = routing_digest(
            &physical,
            &configured_physical_points,
            &measurement_routes,
            &action_routes,
        );
        Ok(Self {
            physical,
            configured_physical_points,
            measurement_routes,
            action_routes,
            digest,
        })
    }

    /// Returns the physical point manifest.
    #[must_use]
    pub const fn point_manifest(&self) -> &ChannelPointManifest {
        self.physical.point_manifest()
    }

    /// Returns the channel-health manifest.
    #[must_use]
    pub const fn health_manifest(&self) -> &ChannelHealthManifest {
        self.physical.health_manifest()
    }

    /// Returns every configured physical point in canonical address order.
    #[must_use]
    pub fn configured_physical_points(&self) -> &[PhysicalPointAddress] {
        &self.configured_physical_points
    }

    /// Resolves one logical measurement point through the C2M index.
    #[must_use]
    pub fn measurement_route(
        &self,
        instance_id: u32,
        point_id: u32,
    ) -> Option<PhysicalPointAddress> {
        self.measurement_routes
            .get(&(instance_id, point_id))
            .copied()
    }

    /// Resolves one logical action point through the M2C index.
    #[must_use]
    pub fn action_route(&self, instance_id: u32, point_id: u32) -> Option<PhysicalPointAddress> {
        self.action_routes.get(&(instance_id, point_id)).copied()
    }

    /// Iterates C2M routes in deterministic logical-address order.
    pub fn measurement_routes(
        &self,
    ) -> impl Iterator<Item = (u32, u32, PhysicalPointAddress)> + '_ {
        self.measurement_routes
            .iter()
            .map(|(&(instance_id, point_id), &target)| (instance_id, point_id, target))
    }

    /// Iterates M2C routes in deterministic logical-address order.
    pub fn action_routes(&self) -> impl Iterator<Item = (u32, u32, PhysicalPointAddress)> + '_ {
        self.action_routes
            .iter()
            .map(|(&(instance_id, point_id), &target)| (instance_id, point_id, target))
    }

    /// Returns the number of enabled C2M routes.
    #[must_use]
    pub fn measurement_route_count(&self) -> usize {
        self.measurement_routes.len()
    }

    /// Returns the number of enabled M2C routes.
    #[must_use]
    pub fn action_route_count(&self) -> usize {
        self.action_routes.len()
    }

    /// Returns the deterministic physical/logical topology digest.
    #[must_use]
    pub const fn digest(&self) -> u64 {
        self.digest
    }

    /// Splits this generation for composition roots without another adapter read.
    #[must_use]
    pub fn into_parts(
        self,
    ) -> (
        ChannelPointManifest,
        ChannelHealthManifest,
        LogicalPointRoutes,
        LogicalPointRoutes,
    ) {
        let (points, health) = self.physical.into_manifests();
        (points, health, self.measurement_routes, self.action_routes)
    }
}

fn validate_routes(
    role: &str,
    routes: &LogicalPointRoutes,
    writable: bool,
    manifest: &ChannelPointManifest,
    configured_physical_points: &[PhysicalPointAddress],
) -> PortResult<()> {
    for (&(instance_id, point_id), &target) in routes {
        if target.kind().is_writable() != writable {
            return Err(invalid_routing(format!(
                "{role} target for {instance_id}:{point_id} violates read/write ownership"
            )));
        }
        if manifest.slot_for(target).is_none() {
            return Err(invalid_routing(format!(
                "{role} target for {instance_id}:{point_id} is absent from the point manifest"
            )));
        }
        if !configured_physical_points.contains(&target) {
            return Err(invalid_routing(format!(
                "{role} target for {instance_id}:{point_id} is not a configured physical point"
            )));
        }
    }
    Ok(())
}

const fn point_kind_order(kind: aether_domain::PointKind) -> u8 {
    match kind {
        aether_domain::PointKind::Telemetry => 0,
        aether_domain::PointKind::Status => 1,
        aether_domain::PointKind::Command => 2,
        aether_domain::PointKind::Action => 3,
    }
}

fn routing_digest(
    physical: &PhysicalTopologySnapshot,
    configured_physical_points: &[PhysicalPointAddress],
    measurements: &LogicalPointRoutes,
    actions: &LogicalPointRoutes,
) -> u64 {
    let mut hasher = FxHasher::default();
    "aether.routing-snapshot.v1".hash(&mut hasher);
    physical.point_manifest().layout_hash().hash(&mut hasher);
    physical.point_manifest().slot_count().hash(&mut hasher);
    physical.health_manifest().layout_hash().hash(&mut hasher);
    physical.health_manifest().slot_count().hash(&mut hasher);
    configured_physical_points.hash(&mut hasher);
    hash_routes(0, measurements, &mut hasher);
    hash_routes(1, actions, &mut hasher);
    hasher.finish()
}

fn hash_routes(role: u8, routes: &LogicalPointRoutes, hasher: &mut FxHasher) {
    role.hash(hasher);
    for (&(instance_id, point_id), &target) in routes {
        instance_id.hash(hasher);
        point_id.hash(hasher);
        target.hash(hasher);
    }
}

fn invalid_routing(message: impl Into<String>) -> PortError {
    PortError::new(PortErrorKind::InvalidData, message)
}
