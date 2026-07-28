//! Typed channel-to-channel routing used by the acquisition owner.

use std::hash::{Hash, Hasher};
use std::sync::Arc;

use aether_domain::PointKind;
use aether_ports::{PortError, PortErrorKind, PortResult};
use aether_shm_bridge::PhysicalPointAddress;
use arc_swap::ArcSwap;
use rustc_hash::{FxHashMap, FxHasher};

/// Maximum number of channel-to-channel forwarding hops.
pub const MAX_C2C_CASCADE_DEPTH: u8 = 2;

/// One C2C destination and its optional linear transformation.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct C2CTarget {
    /// Target channel identifier.
    pub channel_id: u32,
    /// Target physical point kind.
    pub point_kind: PointKind,
    /// Target point identifier.
    pub point_id: u32,
    /// Linear scale factor.
    pub scale: f64,
    /// Linear offset.
    pub offset: f64,
}

impl C2CTarget {
    /// Applies `scale * value + offset`.
    #[must_use]
    #[inline]
    pub fn transform(self, value: f64) -> f64 {
        self.scale * value + self.offset
    }

    /// Returns whether this route preserves the source value.
    #[must_use]
    #[inline]
    pub fn is_identity_transform(self) -> bool {
        self.scale == 1.0 && self.offset == 0.0
    }
}

/// One validated C2C routing definition.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ChannelRoute {
    source: PhysicalPointAddress,
    target: C2CTarget,
}

impl ChannelRoute {
    /// Creates a route between acquisition-owned physical points.
    pub fn new(
        source: PhysicalPointAddress,
        target: PhysicalPointAddress,
        scale: f64,
        offset: f64,
    ) -> PortResult<Self> {
        if !source.kind().is_acquisition_owned() || !target.kind().is_acquisition_owned() {
            return Err(PortError::new(
                PortErrorKind::InvalidData,
                "C2C routes must connect acquisition-owned points",
            ));
        }
        if !scale.is_finite() || !offset.is_finite() {
            return Err(PortError::new(
                PortErrorKind::InvalidData,
                "C2C route scale and offset must be finite",
            ));
        }
        Ok(Self {
            source,
            target: C2CTarget {
                channel_id: target.channel_id().get(),
                point_kind: target.kind(),
                point_id: target.point_id().get(),
                scale,
                offset,
            },
        })
    }

    /// Returns the source physical point.
    #[must_use]
    pub const fn source(self) -> PhysicalPointAddress {
        self.source
    }

    /// Returns the destination and transform.
    #[must_use]
    pub const fn target(self) -> C2CTarget {
        self.target
    }
}

type RouteKey = (u32, PointKind, u32);

/// Atomically replaceable typed C2C routing table.
#[derive(Debug)]
pub struct ChannelRoutingCache {
    routes: ArcSwap<FxHashMap<RouteKey, C2CTarget>>,
}

impl ChannelRoutingCache {
    /// Creates an empty routing table.
    #[must_use]
    pub fn new() -> Self {
        Self {
            routes: ArcSwap::from_pointee(FxHashMap::default()),
        }
    }

    /// Creates a routing table from one complete definition snapshot.
    pub fn from_routes(routes: impl IntoIterator<Item = ChannelRoute>) -> PortResult<Self> {
        Ok(Self {
            routes: ArcSwap::from_pointee(build_routes(routes)?),
        })
    }

    /// Atomically replaces the complete routing table.
    pub fn replace(&self, routes: impl IntoIterator<Item = ChannelRoute>) -> PortResult<()> {
        self.routes.store(Arc::new(build_routes(routes)?));
        Ok(())
    }

    /// Resolves one physical source point.
    #[must_use]
    #[inline]
    pub fn lookup_c2c_by_parts(
        &self,
        channel_id: u32,
        point_kind: PointKind,
        point_id: u32,
    ) -> Option<C2CTarget> {
        self.routes
            .load()
            .get(&(channel_id, point_kind, point_id))
            .copied()
    }

    /// Returns the number of active C2C routes.
    #[must_use]
    pub fn len(&self) -> usize {
        self.routes.load().len()
    }

    /// Returns whether this table is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.routes.load().is_empty()
    }

    /// Returns a deterministic digest used by reconciliation logging.
    #[must_use]
    pub fn content_hash(&self) -> u64 {
        let routes = self.routes.load();
        let mut entries: Vec<_> = routes
            .iter()
            .map(|(&source, &target)| (source, target))
            .collect();
        entries.sort_unstable_by_key(|((channel_id, kind, point_id), _)| {
            (*channel_id, point_kind_order(*kind), *point_id)
        });
        let mut hasher = FxHasher::default();
        "aether.channel-routing.v1".hash(&mut hasher);
        for ((channel_id, kind, point_id), target) in entries {
            channel_id.hash(&mut hasher);
            point_kind_order(kind).hash(&mut hasher);
            point_id.hash(&mut hasher);
            target.channel_id.hash(&mut hasher);
            point_kind_order(target.point_kind).hash(&mut hasher);
            target.point_id.hash(&mut hasher);
            target.scale.to_bits().hash(&mut hasher);
            target.offset.to_bits().hash(&mut hasher);
        }
        hasher.finish()
    }
}

impl Default for ChannelRoutingCache {
    fn default() -> Self {
        Self::new()
    }
}

fn build_routes(
    routes: impl IntoIterator<Item = ChannelRoute>,
) -> PortResult<FxHashMap<RouteKey, C2CTarget>> {
    let mut table = FxHashMap::default();
    for route in routes {
        let source = route.source();
        let key = (
            source.channel_id().get(),
            source.kind(),
            source.point_id().get(),
        );
        if table.insert(key, route.target()).is_some() {
            return Err(PortError::new(
                PortErrorKind::InvalidData,
                "C2C routing contains a duplicate source point",
            ));
        }
    }
    Ok(table)
}

const fn point_kind_order(kind: PointKind) -> u8 {
    match kind {
        PointKind::Telemetry => 0,
        PointKind::Status => 1,
        PointKind::Command => 2,
        PointKind::Action => 3,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn point(channel: u32, kind: PointKind, point: u32) -> PhysicalPointAddress {
        PhysicalPointAddress::from_legacy_raw(channel, kind, point)
    }

    #[test]
    fn typed_route_is_resolved_without_string_parsing() {
        let route = ChannelRoute::new(
            point(1, PointKind::Telemetry, 2),
            point(3, PointKind::Status, 4),
            2.0,
            1.0,
        )
        .expect("valid C2C route");
        let cache = ChannelRoutingCache::from_routes([route]).expect("routing snapshot");
        let target = cache
            .lookup_c2c_by_parts(1, PointKind::Telemetry, 2)
            .expect("route target");
        assert_eq!(target.channel_id, 3);
        assert_eq!(target.point_kind, PointKind::Status);
        assert_eq!(target.point_id, 4);
        assert_eq!(target.transform(5.0), 11.0);
    }

    #[test]
    fn replacement_is_complete_and_changes_the_digest() {
        let first = ChannelRoute::new(
            point(1, PointKind::Telemetry, 0),
            point(2, PointKind::Telemetry, 0),
            1.0,
            0.0,
        )
        .expect("first route");
        let second = ChannelRoute::new(
            point(1, PointKind::Telemetry, 1),
            point(2, PointKind::Telemetry, 1),
            1.0,
            0.0,
        )
        .expect("second route");
        let cache = ChannelRoutingCache::from_routes([first]).expect("first snapshot");
        let first_digest = cache.content_hash();
        cache.replace([second]).expect("replace snapshot");
        assert_ne!(cache.content_hash(), first_digest);
        assert!(
            cache
                .lookup_c2c_by_parts(1, PointKind::Telemetry, 0)
                .is_none()
        );
    }

    #[test]
    fn command_owned_and_duplicate_sources_fail_closed() {
        assert!(
            ChannelRoute::new(
                point(1, PointKind::Command, 0),
                point(2, PointKind::Telemetry, 0),
                1.0,
                0.0,
            )
            .is_err()
        );
        let route = ChannelRoute::new(
            point(1, PointKind::Telemetry, 0),
            point(2, PointKind::Telemetry, 0),
            1.0,
            0.0,
        )
        .expect("route");
        assert!(ChannelRoutingCache::from_routes([route, route]).is_err());
    }

    #[test]
    fn identity_transform_is_explicit() {
        let route = ChannelRoute::new(
            point(1, PointKind::Telemetry, 0),
            point(2, PointKind::Telemetry, 0),
            1.0,
            0.0,
        )
        .expect("route");
        assert!(route.target().is_identity_transform());
    }
}
