//! Home Assistant delegated-device provider for AetherEdge.
//!
//! Home Assistant wire DTOs terminate inside this extension. Core Aether
//! packages see only vendor-neutral integration topology and observations.

mod config;
#[cfg(feature = "integration-control")]
mod control;
mod mapping;
mod model;
mod provider;
mod secret;
mod transport;
mod websocket;

pub use config::HomeAssistantConnectionConfig;
pub use model::{
    HomeAssistantArea, HomeAssistantDevice, HomeAssistantEntity, HomeAssistantSnapshot,
    HomeAssistantState, HomeAssistantStateChanged,
};
pub use provider::HomeAssistantBridge;
pub use secret::EnvironmentSecretResolver;
pub use transport::HomeAssistantTransport;
pub use websocket::WebSocketHomeAssistantTransport;
