//! Write capability granted only to the acquisition/data-plane owner.
//!
//! Keeping this trait in a dedicated crate turns writer authority into a Cargo
//! dependency edge. Read-only applications depend on `aether-ports` and cannot
//! acquire this capability accidentally.

use aether_domain::AcquiredPointSample;
use aether_ports::PortResult;
use async_trait::async_trait;

/// Writes physical telemetry/status samples for the single acquisition owner.
///
/// Application interfaces receive [`aether_ports::LiveState`] instead. The
/// physical channel address prevents HTTP, CLI, or AI code from masquerading
/// an instance identifier as an acquisition channel.
#[async_trait]
pub trait AcquisitionStateWriter: Send + Sync + 'static {
    /// Commits a validated batch and returns the number of written samples.
    ///
    /// Implementations must reject the batch before the first write when any
    /// address is unknown or owned by another data-plane writer.
    async fn write_batch(&self, samples: &[AcquiredPointSample]) -> PortResult<usize>;
}
