//! Durable store-and-forward capability for unreliable uplinks.

use aether_domain::TimestampMs;
use async_trait::async_trait;

use crate::PortResult;

/// Identifier assigned to an outbox entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub struct OutboxId(u64);

impl OutboxId {
    /// Creates an outbox identifier.
    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// Returns the underlying value.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Payload waiting to be delivered to an uplink.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutboxMessage {
    destination: String,
    payload: Vec<u8>,
    created_at: TimestampMs,
}

impl OutboxMessage {
    /// Creates a pending uplink message.
    pub fn new(
        destination: impl Into<String>,
        payload: impl Into<Vec<u8>>,
        created_at: TimestampMs,
    ) -> Self {
        Self {
            destination: destination.into(),
            payload: payload.into(),
            created_at,
        }
    }

    /// Returns the logical destination or topic.
    #[must_use]
    pub fn destination(&self) -> &str {
        &self.destination
    }

    /// Returns the opaque payload.
    #[must_use]
    pub fn payload(&self) -> &[u8] {
        &self.payload
    }

    /// Returns when the message entered the outbox.
    #[must_use]
    pub const fn created_at(&self) -> TimestampMs {
        self.created_at
    }
}

/// Stored message returned to an uplink worker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutboxEntry {
    id: OutboxId,
    message: OutboxMessage,
    attempts: u32,
}

impl OutboxEntry {
    /// Creates a stored outbox entry.
    #[must_use]
    pub const fn new(id: OutboxId, message: OutboxMessage, attempts: u32) -> Self {
        Self {
            id,
            message,
            attempts,
        }
    }

    /// Returns the entry identifier.
    #[must_use]
    pub const fn id(&self) -> OutboxId {
        self.id
    }

    /// Returns the message to deliver.
    #[must_use]
    pub const fn message(&self) -> &OutboxMessage {
        &self.message
    }

    /// Returns the delivery attempt count.
    #[must_use]
    pub const fn attempts(&self) -> u32 {
        self.attempts
    }
}

/// Durable bounded queue used for offline store-and-forward.
#[async_trait]
pub trait DurableOutbox: Send + Sync + 'static {
    /// Enqueues a message and returns its durable identifier.
    async fn enqueue(&self, message: OutboxMessage) -> PortResult<OutboxId>;

    /// Returns up to `limit` oldest unacknowledged messages.
    async fn peek(&self, limit: usize) -> PortResult<Vec<OutboxEntry>>;

    /// Acknowledges delivered entries and returns the removed count.
    async fn acknowledge(&self, ids: &[OutboxId]) -> PortResult<usize>;
}
