//! Live point-state capability.

use aether_domain::{PointAddress, PointSample};
use async_trait::async_trait;

use crate::PortResult;

/// Reads the authoritative current point view.
#[async_trait]
pub trait LiveState: Send + Sync + 'static {
    /// Reads one point, returning `None` when no sample has been observed.
    async fn read(&self, address: PointAddress) -> PortResult<Option<PointSample>>;

    /// Reads several points while preserving input order.
    async fn read_many(&self, addresses: &[PointAddress]) -> PortResult<Vec<Option<PointSample>>>;
}

/// Writes samples on behalf of the single acquisition/data-plane owner.
///
/// Application interfaces and AI clients receive [`LiveState`] only. Keeping
/// this capability separate prevents them from bypassing command routing and
/// control safety by writing current state directly.
#[async_trait]
pub trait LiveStateWriter: Send + Sync + 'static {
    /// Stores the newest sample for a point.
    async fn write(&self, sample: PointSample) -> PortResult<()>;
}
