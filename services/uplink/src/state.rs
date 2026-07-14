use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use aether_ports::DurableOutbox;
use sqlx::SqlitePool;
use tokio::sync::{Mutex, Notify, RwLock};

use crate::config::EnvConfig;
use crate::device::{DeviceIdentity, Topics};
use crate::live_values::UplinkTopologyHandle;
use crate::models::NetConfig;

/// Shared application state.
pub struct AppState {
    /// Shared SQLite pool – uplink_config table.
    pub sqlite: SqlitePool,
    /// Local durable queue between data collection and the MQTT uplink.
    pub outbox: Arc<dyn DurableOutbox>,
    /// Static env config.
    pub env: Arc<EnvConfig>,
    /// Atomically replaceable SQLite + committed point/health read generation.
    pub live_topology: Arc<UplinkTopologyHandle>,
    /// Dynamic config reloaded from `uplink_config`.
    pub config: Arc<RwLock<NetConfig>>,
    /// Resolved device identity (product SN + device SN).
    pub device: Arc<DeviceIdentity>,
    /// MQTT topics derived from device identity.
    pub topics: Arc<Topics>,
    /// Current MQTT publish client – None while disconnected.
    pub mqtt_client: Arc<Mutex<Option<rumqttc::AsyncClient>>>,
    /// True when MQTT is connected and ready.
    pub mqtt_connected: Arc<AtomicBool>,
    /// Signal for the MQTT task to reconnect (config changed or explicit API call).
    pub reconnect_signal: Arc<Notify>,
    /// When true the MQTT loop stays idle after a disconnect instead of auto-reconnecting.
    /// Set by `POST /netApi/mqtt/disconnect`, cleared by `POST /netApi/mqtt/reconnect`.
    pub disconnect_requested: Arc<AtomicBool>,
    /// HTTP client for outbound calls (call-alarm → alarm).
    pub http_client: reqwest::Client,
}
