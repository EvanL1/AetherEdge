//! Automation composition adapter for authoritative rule live-state reads.

use std::sync::Arc;

use aether_domain::PointKind;
use aether_routing::RoutingCache;
use aether_rules::RuleLiveState;
use aether_shm_bridge::ShmChannelReaderHandle;

/// Rule live-state adapter backed by the current SHM generation and routing
/// snapshot owned by the Automation service composition root.
pub struct ShmRuleLiveState {
    reader: Arc<ShmChannelReaderHandle>,
    routing_cache: Arc<RoutingCache>,
}

impl ShmRuleLiveState {
    /// Creates a read-only rule input over the current SHM reader and routing snapshot.
    #[must_use]
    pub fn new(reader: Arc<ShmChannelReaderHandle>, routing_cache: Arc<RoutingCache>) -> Self {
        Self {
            reader,
            routing_cache,
        }
    }
}

impl RuleLiveState for ShmRuleLiveState {
    fn get_instance(
        &self,
        instance_id: u32,
        instance_type: u8,
        point_id: u32,
    ) -> Option<(f64, u64)> {
        read_instance_point(
            &self.reader,
            &self.routing_cache,
            instance_id,
            instance_type,
            point_id,
        )
    }
}

/// Resolves one logical instance point through the service-owned routing cache
/// and reads the resulting physical channel address from SHM.
pub(crate) fn read_instance_point(
    reader: &ShmChannelReaderHandle,
    routing_cache: &RoutingCache,
    instance_id: u32,
    instance_type: u8,
    point_id: u32,
) -> Option<(f64, u64)> {
    let (channel_id, kind, channel_point_id) = if instance_type == 0 {
        let (channel_id, point_type, channel_point_id) =
            routing_cache.lookup_c2m_reverse(instance_id, point_id)?;
        let kind = match point_type {
            aether_model::PointType::Telemetry => PointKind::Telemetry,
            aether_model::PointType::Signal => PointKind::Status,
            aether_model::PointType::Control | aether_model::PointType::Adjustment => return None,
        };
        (channel_id, kind, channel_point_id)
    } else {
        let target = routing_cache
            .lookup_m2c_by_parts(instance_id, aether_model::PointType::Control, point_id)
            .or_else(|| {
                routing_cache.lookup_m2c_by_parts(
                    instance_id,
                    aether_model::PointType::Adjustment,
                    point_id,
                )
            })?;
        let kind = match target.point_type {
            aether_model::PointType::Control => PointKind::Command,
            aether_model::PointType::Adjustment => PointKind::Action,
            aether_model::PointType::Telemetry | aether_model::PointType::Signal => return None,
        };
        (target.channel_id, kind, target.point_id)
    };
    reader
        .read_channel(channel_id, kind, channel_point_id)
        .ok()
        .flatten()
        .map(|value| (value.value(), value.timestamp_ms()))
}
