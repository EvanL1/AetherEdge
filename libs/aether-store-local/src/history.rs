//! In-memory history sink for embedding and deterministic tests.

use std::sync::Mutex;

use aether_domain::PointSample;
use aether_ports::{HistorySink, PortResult};
use async_trait::async_trait;

use crate::lock_error;

/// Append-only in-memory historical sample sink.
#[derive(Debug, Default)]
pub struct MemoryHistorySink {
    samples: Mutex<Vec<PointSample>>,
}

impl MemoryHistorySink {
    /// Creates an empty history sink.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns a consistent snapshot of stored samples.
    pub fn samples(&self) -> PortResult<Vec<PointSample>> {
        self.samples
            .lock()
            .map(|samples| samples.clone())
            .map_err(|_| lock_error("history"))
    }
}

#[async_trait]
impl HistorySink for MemoryHistorySink {
    async fn append(&self, samples: &[PointSample]) -> PortResult<usize> {
        self.samples
            .lock()
            .map(|mut history| {
                history.extend_from_slice(samples);
                samples.len()
            })
            .map_err(|_| lock_error("history"))
    }
}
