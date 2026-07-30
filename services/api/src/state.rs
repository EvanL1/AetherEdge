use std::sync::Arc;

use aether_application::DataProcessingApplication;
use dashmap::DashMap;
use sqlx::SqlitePool;

use crate::config::GatewayConfig;
use crate::read_models::RefreshTokenInfo;
use crate::ws::WsHub;

pub struct AppState {
    pub db: SqlitePool,
    pub config: Arc<GatewayConfig>,
    pub ws_hub: Arc<WsHub>,
    /// Optional, explicitly commissioned Data Processing application.
    ///
    /// The HTTP surface is not mounted when this is `None`.
    pub data_processing: Option<Arc<DataProcessingApplication>>,
    /// In-memory refresh token store: token_id -> RefreshTokenInfo
    pub refresh_tokens: DashMap<String, RefreshTokenInfo>,
    /// Shared connection pool for the fixed internal application-service
    /// gateway. Upstream hosts come only from trusted runtime configuration.
    pub service_client: reqwest::Client,
}
