//! Application service for durable store-and-forward delivery.

use std::sync::Arc;

use aether_ports::{
    DurableOutbox, OutboxId, PortError, PortErrorKind, PortResult, UplinkPublisher,
};

/// Result of one bounded outbox drain pass.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DrainReport {
    examined: usize,
    delivered: usize,
}

impl DrainReport {
    /// Number of entries read from the outbox in this pass.
    #[must_use]
    pub const fn examined(self) -> usize {
        self.examined
    }

    /// Number of entries published and durably acknowledged.
    #[must_use]
    pub const fn delivered(self) -> usize {
        self.delivered
    }
}

/// Coordinates an outbox and a transport adapter without knowing either
/// implementation.
pub struct OutboxForwarder {
    outbox: Arc<dyn DurableOutbox>,
    publisher: Arc<dyn UplinkPublisher>,
}

impl OutboxForwarder {
    /// Creates a store-and-forward application service.
    #[must_use]
    pub fn new(outbox: Arc<dyn DurableOutbox>, publisher: Arc<dyn UplinkPublisher>) -> Self {
        Self { outbox, publisher }
    }

    /// Publishes and acknowledges at most `limit` entries in FIFO order.
    ///
    /// If publishing fails, the current entry remains durable. If publishing
    /// succeeds but acknowledgement fails, the entry is intentionally retained
    /// and may be delivered again, giving at-least-once rather than at-most-once
    /// behavior across ambiguous failures.
    pub async fn drain_once(&self, limit: usize) -> PortResult<DrainReport> {
        let entries = self.outbox.peek(limit).await?;
        let mut report = DrainReport {
            examined: entries.len(),
            delivered: 0,
        };

        for entry in entries {
            self.publisher.publish(entry.message()).await?;
            ensure_acknowledged(self.outbox.as_ref(), entry.id()).await?;
            report.delivered += 1;
        }

        Ok(report)
    }
}

async fn ensure_acknowledged(outbox: &dyn DurableOutbox, id: OutboxId) -> PortResult<()> {
    let removed = outbox.acknowledge(&[id]).await?;
    if removed != 1 {
        return Err(PortError::new(
            PortErrorKind::Conflict,
            format!(
                "outbox entry {} was published but acknowledgement removed {removed} entries",
                id.get()
            ),
        ));
    }
    Ok(())
}
