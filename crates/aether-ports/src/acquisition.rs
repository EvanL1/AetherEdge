//! Acquisition-owned writes into the authoritative live data plane.

use aether_domain::AcquiredPointSample;
use async_trait::async_trait;

use crate::PortResult;

/// Writes physical telemetry/status samples for the single acquisition owner.
///
/// Application interfaces receive [`crate::LiveState`] instead. Keeping the
/// physical channel address in this port makes it impossible for HTTP, CLI, or
/// AI code to masquerade an instance identifier as an acquisition channel.
#[async_trait]
pub trait AcquisitionStateWriter: Send + Sync + 'static {
    /// Commits a validated batch and returns the number of written samples.
    ///
    /// Implementations must reject the batch before the first write when any
    /// address is unknown or owned by another data-plane writer.
    async fn write_batch(&self, samples: &[AcquiredPointSample]) -> PortResult<usize>;
}
