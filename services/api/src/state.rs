use std::sync::Arc;

use dashmap::DashMap;
use sqlx::SqlitePool;

use crate::config::GatewayConfig;
use crate::models::RefreshTokenInfo;
use crate::ws::WsHub;

pub struct AppState {
    pub db: SqlitePool,
    pub config: Arc<GatewayConfig>,
    pub ws_hub: Arc<WsHub>,
    /// In-memory refresh token store: token_id -> RefreshTokenInfo
    pub refresh_tokens: DashMap<String, RefreshTokenInfo>,
}
