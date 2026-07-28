//! Reusable conformance checks and deterministic test doubles for adapter authors.

use aether_domain::PointSample;
use aether_ports::{
    CloudLinkTransport, CloudLinkTransportEvent, CloudLinkTransportMessage, DurableOutbox,
    LiveState, LiveStateWriter, OutboxMessage, PortError, PortErrorKind, PortResult,
};
use async_trait::async_trait;
use tokio::sync::{Mutex as AsyncMutex, mpsc};

/// One endpoint of a bounded in-memory CloudLink transport pair.
///
/// `send` delivers an inbound event to the peer. Durable messages also produce
/// transport-published evidence for the sender; no application ACK is invented.
pub struct MemoryCloudLinkTransport {
    own_events: mpsc::Sender<PortResult<CloudLinkTransportEvent>>,
    peer_events: mpsc::Sender<PortResult<CloudLinkTransportEvent>>,
    events: AsyncMutex<mpsc::Receiver<PortResult<CloudLinkTransportEvent>>>,
}

impl MemoryCloudLinkTransport {
    /// Creates two connected bounded endpoints.
    pub fn pair(capacity: usize) -> PortResult<(Self, Self)> {
        if capacity == 0 {
            return Err(contract_error(
                "memory CloudLink transport capacity must be greater than zero",
            ));
        }
        let (a_tx, a_rx) = mpsc::channel(capacity);
        let (b_tx, b_rx) = mpsc::channel(capacity);
        a_tx.try_send(Ok(CloudLinkTransportEvent::Connected))
            .map_err(|_| contract_error("cannot initialize memory CloudLink endpoint A"))?;
        b_tx.try_send(Ok(CloudLinkTransportEvent::Connected))
            .map_err(|_| contract_error("cannot initialize memory CloudLink endpoint B"))?;
        Ok((
            Self {
                own_events: a_tx.clone(),
                peer_events: b_tx.clone(),
                events: AsyncMutex::new(a_rx),
            },
            Self {
                own_events: b_tx,
                peer_events: a_tx,
                events: AsyncMutex::new(b_rx),
            },
        ))
    }

    /// Injects a deterministic disconnect observation at both endpoints.
    pub async fn disconnect(&self) -> PortResult<()> {
        self.own_events
            .send(Ok(CloudLinkTransportEvent::Disconnected))
            .await
            .map_err(|_| contract_error("memory CloudLink endpoint is closed"))?;
        self.peer_events
            .send(Ok(CloudLinkTransportEvent::Disconnected))
            .await
            .map_err(|_| contract_error("memory CloudLink peer is closed"))
    }
}

#[async_trait]
impl CloudLinkTransport for MemoryCloudLinkTransport {
    async fn send(&self, message: CloudLinkTransportMessage) -> PortResult<()> {
        self.peer_events
            .send(Ok(CloudLinkTransportEvent::Inbound(message.clone())))
            .await
            .map_err(|_| contract_error("memory CloudLink peer is closed"))?;
        if let Some(identity) = message.delivery().cloned() {
            self.own_events
                .send(Ok(CloudLinkTransportEvent::TransportPublished(identity)))
                .await
                .map_err(|_| contract_error("memory CloudLink endpoint is closed"))?;
        }
        Ok(())
    }

    async fn receive(&self) -> PortResult<CloudLinkTransportEvent> {
        self.events.lock().await.recv().await.unwrap_or_else(|| {
            Err(PortError::new(
                PortErrorKind::Unavailable,
                "memory CloudLink event stream ended",
            ))
        })
    }
}

/// Verifies the required read/write and ordered batch behavior of `LiveState`.
pub async fn assert_live_state_round_trip(
    reader: &dyn LiveState,
    writer: &dyn LiveStateWriter,
    first: PointSample,
    second: PointSample,
) -> PortResult<()> {
    writer.write(first).await?;
    writer.write(second).await?;

    if reader.read(first.address()).await? != Some(first) {
        return Err(contract_error("live-state single read did not round trip"));
    }

    let actual = reader
        .read_many(&[second.address(), first.address()])
        .await?;
    if actual != vec![Some(second), Some(first)] {
        return Err(contract_error(
            "live-state batch read did not preserve input order",
        ));
    }

    Ok(())
}

/// Verifies FIFO visibility and acknowledgement behavior of `DurableOutbox`.
pub async fn assert_outbox_fifo(
    outbox: &dyn DurableOutbox,
    first: OutboxMessage,
    second: OutboxMessage,
) -> PortResult<()> {
    let first_id = outbox.enqueue(first).await?;
    let second_id = outbox.enqueue(second).await?;
    let pending = outbox.peek(2).await?;

    if pending.len() != 2 || pending[0].id() != first_id || pending[1].id() != second_id {
        return Err(contract_error(
            "outbox did not expose entries in FIFO order",
        ));
    }

    if outbox.acknowledge(&[first_id]).await? != 1 {
        return Err(contract_error("outbox did not acknowledge the first entry"));
    }

    let remaining = outbox.peek(2).await?;
    if remaining.len() != 1 || remaining[0].id() != second_id {
        return Err(contract_error(
            "outbox acknowledgement removed the wrong entry",
        ));
    }

    Ok(())
}

fn contract_error(message: &str) -> PortError {
    PortError::new(PortErrorKind::InvalidData, message)
}
