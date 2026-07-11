//! In-process current-value store for embedding and tests.

use std::collections::HashMap;
use std::sync::RwLock;

use aether_domain::{PointAddress, PointSample};
use aether_ports::{LiveState, LiveStateWriter, PortResult};
use async_trait::async_trait;

use crate::lock_error;

/// Thread-safe in-memory implementation of [`LiveState`].
#[derive(Debug, Default)]
pub struct MemoryLiveState {
    samples: RwLock<HashMap<PointAddress, PointSample>>,
}

impl MemoryLiveState {
    /// Creates an empty live-state store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the number of observed point addresses.
    pub fn len(&self) -> PortResult<usize> {
        self.samples
            .read()
            .map(|samples| samples.len())
            .map_err(|_| lock_error("live-state read"))
    }

    /// Returns whether no point samples have been observed.
    pub fn is_empty(&self) -> PortResult<bool> {
        self.len().map(|length| length == 0)
    }
}

#[async_trait]
impl LiveState for MemoryLiveState {
    async fn read(&self, address: PointAddress) -> PortResult<Option<PointSample>> {
        self.samples
            .read()
            .map(|samples| samples.get(&address).copied())
            .map_err(|_| lock_error("live-state read"))
    }

    async fn read_many(&self, addresses: &[PointAddress]) -> PortResult<Vec<Option<PointSample>>> {
        self.samples
            .read()
            .map(|samples| {
                addresses
                    .iter()
                    .map(|address| samples.get(address).copied())
                    .collect()
            })
            .map_err(|_| lock_error("live-state batch read"))
    }
}

#[async_trait]
impl LiveStateWriter for MemoryLiveState {
    async fn write(&self, sample: PointSample) -> PortResult<()> {
        self.samples
            .write()
            .map(|mut samples| {
                samples.insert(sample.address(), sample);
            })
            .map_err(|_| lock_error("live-state write"))
    }
}
