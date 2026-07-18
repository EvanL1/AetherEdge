//! Bounded normalized DTOs returned by the Home Assistant transport layer.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// One Home Assistant area registry entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HomeAssistantArea {
    /// Stable area registry identifier.
    pub id: String,
    /// Current display name.
    pub name: String,
}

/// One Home Assistant device registry entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HomeAssistantDevice {
    /// Stable device registry identifier.
    pub id: String,
    /// Current display name.
    pub name: String,
    /// Directly assigned area.
    pub area_id: Option<String>,
}

/// One Home Assistant entity registry entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HomeAssistantEntity {
    /// Stable entity registry identifier.
    pub id: String,
    /// Mutable Home Assistant entity name such as `light.kitchen`.
    pub entity_id: String,
    /// Current display name.
    pub name: String,
    /// Entity domain such as `light` or `climate`.
    pub domain: String,
    /// Associated device registry identifier.
    pub device_id: Option<String>,
    /// Entity-level area override.
    pub area_id: Option<String>,
}

/// One current Home Assistant state.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HomeAssistantState {
    /// Mutable Home Assistant entity name.
    pub entity_id: String,
    /// Provider state text.
    pub state: String,
    /// Untrusted attributes. Only explicitly mapped keys cross the adapter boundary.
    pub attributes: BTreeMap<String, Value>,
    /// Parsed provider observation time.
    pub observed_at_ms: u64,
    /// Home Assistant context identifier used only as execution evidence.
    pub context_id: Option<String>,
}

/// Complete registry and current-state snapshot.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HomeAssistantSnapshot {
    /// Area registry.
    pub areas: Vec<HomeAssistantArea>,
    /// Device registry.
    pub devices: Vec<HomeAssistantDevice>,
    /// Entity registry.
    pub entities: Vec<HomeAssistantEntity>,
    /// Current states keyed by mutable `entity_id`.
    pub states: Vec<HomeAssistantState>,
}

/// One subscribed `state_changed` event after transport decoding.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HomeAssistantStateChanged {
    /// New state. State removal is handled by a complete topology resync.
    pub new_state: HomeAssistantState,
}
