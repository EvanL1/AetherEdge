//! Read-only channel connectivity capability.

use aether_domain::{ChannelId, TimestampMs};

use crate::PortResult;

/// One observed connectivity state from the acquisition-owned health plane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChannelHealthObservation {
    channel_id: ChannelId,
    online: bool,
    observed_at: TimestampMs,
}

impl ChannelHealthObservation {
    /// Creates a typed channel-health observation.
    #[must_use]
    pub const fn new(channel_id: ChannelId, online: bool, observed_at: TimestampMs) -> Self {
        Self {
            channel_id,
            online,
            observed_at,
        }
    }

    /// Returns the physical channel identity.
    #[must_use]
    pub const fn channel_id(self) -> ChannelId {
        self.channel_id
    }

    /// Returns whether acquisition currently considers the channel online.
    #[must_use]
    pub const fn online(self) -> bool {
        self.online
    }

    /// Returns when this state was observed.
    #[must_use]
    pub const fn observed_at(self) -> TimestampMs {
        self.observed_at
    }
}

/// Read-only query port for per-channel connectivity.
pub trait ChannelHealthSource: Send + Sync + 'static {
    /// Reads the latest state. `None` means unconfigured or not observed yet.
    fn read_channel(&self, channel_id: ChannelId) -> PortResult<Option<ChannelHealthObservation>>;
}
