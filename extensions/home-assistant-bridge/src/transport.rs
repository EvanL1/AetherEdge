//! Home Assistant transport seam.

use aether_ports::PortResult;
use async_trait::async_trait;

use crate::{HomeAssistantSnapshot, HomeAssistantStateChanged};

/// Fetches registry snapshots and subscribed state changes from Home Assistant.
///
/// Production implementations use the WebSocket API. Tests use a scripted
/// transport and never require a real household or Home Assistant instance.
#[async_trait]
pub trait HomeAssistantTransport: Send + Sync + 'static {
    /// Fetches areas, devices, entities, current states, and related metadata.
    async fn fetch_snapshot(&self) -> PortResult<HomeAssistantSnapshot>;

    /// Waits for the next decoded `state_changed` event.
    async fn next_state_changed(&self) -> PortResult<HomeAssistantStateChanged>;
}
