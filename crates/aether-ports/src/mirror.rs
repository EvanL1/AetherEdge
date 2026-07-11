//! Optional live-state mirroring capability.

use aether_domain::PointSample;
use async_trait::async_trait;

use crate::PortResult;

/// Publishes a non-authoritative external view of live samples.
#[async_trait]
pub trait StateMirror: Send + Sync + 'static {
    /// Mirrors a batch without changing local source-of-truth ownership.
    async fn mirror(&self, samples: &[PointSample]) -> PortResult<usize>;
}
