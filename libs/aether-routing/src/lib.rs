//! In-memory routing indexes and SHM data-plane routing metadata.
//!
//! Routing configuration is loaded from SQLite and atomically published in
//! [`RoutingCache`]. This crate performs no live-value storage and contains no
//! Redis fallback.

pub mod batch;
pub mod loader;
pub mod routing_cache;

pub use batch::{BatchRoutingResult, ChannelPointUpdate};
pub use loader::{RoutingMaps, load_routing_maps};
pub use routing_cache::{C2CTarget, C2MTarget, M2CTarget, RoutingCache, RoutingCacheStats};

use aether_model::{ValidationConfig, validate_value};
use anyhow::Result;

/// Maximum number of C2C forwarding hops.
pub const MAX_C2C_CASCADE_DEPTH: u8 = 2;

/// Metadata for dispatching one resolved model action to the IO-owned channel
/// slot. The value itself is written through the SHM `ActionDispatch` port.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteContext {
    pub channel_id: String,
    pub point_type: String,
    pub io_point_id: String,
    pub target_channel_id: u32,
    pub target_point_type: u8,
    pub target_point_id: u32,
    pub timestamp_ms: i64,
    pub expires_at_ms: i64,
}

/// Validate a model action before it enters SHM/UDS dispatch.
pub fn validate_action_value(instance_id: u32, point_id: &str, value: f64) -> Result<f64> {
    validate_value(value, &ValidationConfig::default()).map_err(|error| {
        anyhow::anyhow!(
            "M2C data validation failed for inst:{}:A:{}: {}",
            instance_id,
            point_id,
            error
        )
    })
}

/// Convert a cache-resolved target into immutable dispatch metadata.
#[must_use]
pub fn route_context_from_target(target: M2CTarget, timestamp_ms: i64) -> RouteContext {
    let expires_at_ms = timestamp_ms.saturating_add(aether_domain::DEFAULT_COMMAND_TTL_MS as i64);
    RouteContext {
        channel_id: target.channel_id.to_string(),
        point_type: target.point_type.as_str().to_string(),
        io_point_id: target.point_id.to_string(),
        target_channel_id: target.channel_id,
        target_point_type: target.point_type.to_u8(),
        target_point_id: target.point_id,
        timestamp_ms,
        expires_at_ms,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aether_model::PointType;

    #[test]
    fn route_context_preserves_resolved_target() {
        let target = M2CTarget {
            channel_id: 7,
            point_type: PointType::Adjustment,
            point_id: 9,
        };
        let context = route_context_from_target(target, 1234);
        assert_eq!(context.target_channel_id, 7);
        assert_eq!(context.target_point_type, PointType::Adjustment.to_u8());
        assert_eq!(context.target_point_id, 9);
        assert_eq!(context.timestamp_ms, 1234);
        assert_eq!(context.expires_at_ms, 6234);
    }

    #[test]
    fn action_validation_rejects_non_finite_values() {
        assert!(validate_action_value(1, "2", f64::NAN).is_err());
        assert!(validate_action_value(1, "2", f64::INFINITY).is_err());
    }
}
