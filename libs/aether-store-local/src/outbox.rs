//! Bounded in-memory outbox for SDK embedding and tests.

use std::collections::VecDeque;
use std::sync::Mutex;

use aether_ports::{
    DurableOutbox, OutboxEntry, OutboxId, OutboxMessage, PortError, PortErrorKind, PortResult,
};
use async_trait::async_trait;

use crate::lock_error;

#[derive(Debug)]
struct OutboxState {
    next_id: u64,
    entries: VecDeque<OutboxEntry>,
}

/// Process-local bounded outbox.
///
/// This adapter is useful for tests and embedded library use where the host
/// provides lifecycle persistence. A production offline gateway should select
/// an embedded durable adapter implementing the same [`DurableOutbox`] port.
#[derive(Debug)]
pub struct MemoryOutbox {
    capacity: usize,
    state: Mutex<OutboxState>,
}

impl MemoryOutbox {
    /// Creates an outbox that stores at most `capacity` pending entries.
    #[must_use]
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            capacity,
            state: Mutex::new(OutboxState {
                next_id: 1,
                entries: VecDeque::with_capacity(capacity),
            }),
        }
    }

    /// Returns the number of unacknowledged entries.
    pub fn len(&self) -> PortResult<usize> {
        self.state
            .lock()
            .map(|state| state.entries.len())
            .map_err(|_| lock_error("outbox"))
    }

    /// Returns whether the outbox contains no pending entries.
    pub fn is_empty(&self) -> PortResult<bool> {
        self.len().map(|length| length == 0)
    }
}

impl Default for MemoryOutbox {
    fn default() -> Self {
        Self::with_capacity(1_024)
    }
}

#[async_trait]
impl DurableOutbox for MemoryOutbox {
    async fn enqueue(&self, message: OutboxMessage) -> PortResult<OutboxId> {
        let mut state = self.state.lock().map_err(|_| lock_error("outbox"))?;
        if state.entries.len() >= self.capacity {
            return Err(PortError::new(
                PortErrorKind::Unavailable,
                format!("outbox capacity {} reached", self.capacity),
            ));
        }

        let id = OutboxId::new(state.next_id);
        state.next_id = state.next_id.checked_add(1).ok_or_else(|| {
            PortError::new(PortErrorKind::Permanent, "outbox identifier exhausted")
        })?;
        state.entries.push_back(OutboxEntry::new(id, message, 0));
        Ok(id)
    }

    async fn peek(&self, limit: usize) -> PortResult<Vec<OutboxEntry>> {
        self.state
            .lock()
            .map(|state| state.entries.iter().take(limit).cloned().collect())
            .map_err(|_| lock_error("outbox"))
    }

    async fn acknowledge(&self, ids: &[OutboxId]) -> PortResult<usize> {
        let mut state = self.state.lock().map_err(|_| lock_error("outbox"))?;
        let before = state.entries.len();
        state.entries.retain(|entry| !ids.contains(&entry.id()));
        Ok(before - state.entries.len())
    }
}
