//! Historical sample persistence capability.

use aether_domain::PointSample;
use async_trait::async_trait;

use crate::PortResult;

/// Append-only destination for historical samples.
#[async_trait]
pub trait HistorySink: Send + Sync + 'static {
    /// Persists a batch and returns the accepted sample count.
    async fn append(&self, samples: &[PointSample]) -> PortResult<usize>;
}
