//! Transport-neutral uplink publishing capability.

use async_trait::async_trait;

use crate::{OutboxMessage, PortResult};

/// Publishes one opaque outbox message to an external destination.
///
/// Implementations define their delivery boundary. A network adapter should
/// return success only when it has accepted responsibility for retrying the
/// message according to its transport contract.
#[async_trait]
pub trait UplinkPublisher: Send + Sync + 'static {
    /// Publishes one message without changing the durable outbox itself.
    async fn publish(&self, message: &OutboxMessage) -> PortResult<()>;
}
