use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use aether_ports::PortResult;
use aether_shm_bridge::SlotSnapshot;
use axum::http::{HeaderMap, HeaderValue, header};
use dashmap::DashMap;
use sqlx::sqlite::SqlitePoolOptions;

use crate::auth::create_access_token;
use crate::config::GatewayConfig;
use crate::db;
use crate::live_values::GatewayValueSource;
use crate::models::{RoleInfo, UserWithRole};
use crate::state::AppState;
use crate::ws::WsHub;

pub(crate) const TEST_JWT_SECRET: &str = "0123456789abcdef0123456789abcdef";

struct EmptyGatewayValueSource;

impl GatewayValueSource for EmptyGatewayValueSource {
    fn read_group(
        &self,
        _source: &str,
        _owner_id: i64,
        _data_type: &str,
    ) -> PortResult<BTreeMap<String, SlotSnapshot>> {
        Ok(BTreeMap::new())
    }

    fn read_formula(&self, _formula: &str) -> PortResult<Option<SlotSnapshot>> {
        Ok(None)
    }

    fn watched_slots(
        &self,
        _source: &str,
        _owner_ids: &[i64],
        _data_types: &[String],
    ) -> PortResult<BTreeSet<usize>> {
        Ok(BTreeSet::new())
    }

    fn watched_formula_slot(&self, _formula: &str) -> PortResult<Option<usize>> {
        Ok(None)
    }
}

pub(crate) async fn app_state() -> Arc<AppState> {
    app_state_with_public_registration(false).await
}

pub(crate) async fn app_state_with_public_registration(
    allow_public_registration: bool,
) -> Arc<AppState> {
    let database = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .expect("open isolated API test database");
    db::create_tables(&database)
        .await
        .expect("create API test schema");
    db::init_roles(&database)
        .await
        .expect("seed API test roles");

    let config = GatewayConfig {
        jwt_secret: TEST_JWT_SECRET.to_owned(),
        allow_public_registration,
        ..GatewayConfig::default()
    };
    let ws_hub = WsHub::new(Arc::new(EmptyGatewayValueSource), database.clone());

    Arc::new(AppState {
        db: database,
        config: Arc::new(config),
        ws_hub,
        refresh_tokens: DashMap::new(),
    })
}

pub(crate) fn authorization_headers(role_name: &str) -> HeaderMap {
    let role_id = match role_name {
        "Admin" => 1,
        "Engineer" => 2,
        _ => 3,
    };
    let user = UserWithRole {
        id: i64::from(role_id),
        username: format!("{}-test-user", role_name.to_lowercase()),
        is_active: true,
        last_login: None,
        created_at: None,
        updated_at: None,
        role: RoleInfo {
            id: i64::from(role_id),
            name_en: role_name.to_owned(),
            name_zh: role_name.to_owned(),
            description: None,
        },
    };
    let token = create_access_token(&user, TEST_JWT_SECRET, 5).expect("create API test token");
    let mut headers = HeaderMap::new();
    headers.insert(
        header::AUTHORIZATION,
        HeaderValue::from_str(&format!("Bearer {token}")).expect("valid authorization header"),
    );
    headers
}
