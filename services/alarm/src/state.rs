//! Shared application state

use std::sync::Arc;

use sqlx::SqlitePool;
use tokio::sync::RwLock;

use crate::broadcast::Broadcaster;
use crate::config::AlarmConfig;
use crate::live_values::AlarmValueSource;
use crate::models::MonitorStatus;

pub struct AppState {
    pub db: SqlitePool,
    pub live_values: Arc<dyn AlarmValueSource>,
    pub config: Arc<AlarmConfig>,
    pub broadcaster: Broadcaster,
    pub monitor_status: Arc<RwLock<MonitorStatus>>,
}
