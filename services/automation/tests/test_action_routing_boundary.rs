//! Governed action-routing persistence and runtime-publication contracts.

#![allow(clippy::disallowed_methods)]

use std::sync::Arc;

use aether_application::{ActionRoutingApplication, ControlApplication, SafetyPolicy};
use aether_automation::app_state::AppState;
use aether_automation::infra::action_routing::SqliteActionRoutingMutator;
use aether_automation::infra::application_control::{
    AutomationCommandDispatcher, ControlAuthenticator,
};
use aether_automation::{InstanceManager, ProductLoader};
use aether_domain::{ChannelCommandAddress, ChannelId, InstanceId, PointId, PointKind};
use aether_model::product_lib::ProductLibrary;
use aether_ports::{
    ActionRoute, ActionRouteKey, ActionRoutingMutation, AuditSink, AutomationActionRoutingMutator,
    CommandDispatcher, DeviceCommandSink, PortErrorKind,
};
use aether_routing::RoutingCache;
use aether_shm_bridge::ShmDeviceCommandSink;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};
use serde::Serialize;
use sqlx::SqlitePool;
use tower::ServiceExt;

const JWT_SECRET: &str = "0123456789abcdef0123456789abcdef";

struct RoutingFixture {
    _models: tempfile::TempDir,
    pool: SqlitePool,
    cache: Arc<RoutingCache>,
    manager: Arc<InstanceManager>,
    mutator: SqliteActionRoutingMutator,
}

impl RoutingFixture {
    async fn complete() -> Self {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("open routing database");
        common::test_utils::schema::init_automation_schema(&pool)
            .await
            .expect("automation schema");
        common::test_utils::schema::init_io_schema(&pool)
            .await
            .expect("IO schema");
        Self::with_pool(pool).await
    }

    async fn missing_measurement_table() -> Self {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("open degraded routing database");
        for statement in [
            common::test_utils::schema::CHANNELS_TABLE,
            common::test_utils::schema::ADJUSTMENT_POINTS_TABLE,
            common::test_utils::schema::INSTANCES_TABLE,
            common::test_utils::schema::ACTION_ROUTING_TABLE,
        ] {
            sqlx::query(statement)
                .execute(&pool)
                .await
                .expect("minimal schema");
        }
        Self::with_pool(pool).await
    }

    async fn with_pool(pool: SqlitePool) -> Self {
        sqlx::query(
            "INSERT INTO instances (instance_id, instance_name, product_name) \
             VALUES (7, 'actuator_7', 'GenericActuator')",
        )
        .execute(&pool)
        .await
        .expect("instance fixture");
        sqlx::query(
            "INSERT INTO channels (channel_id, name, protocol, enabled) \
             VALUES (3, 'fieldbus_3', 'virtual', 1)",
        )
        .execute(&pool)
        .await
        .expect("channel fixture");
        sqlx::query(
            "INSERT INTO adjustment_points \
             (channel_id, point_id, signal_name, min_value, max_value, step) \
             VALUES (3, 5, 'speed_setpoint', 0.0, 100.0, 1.0)",
        )
        .execute(&pool)
        .await
        .expect("physical action fixture");

        let models = tempfile::tempdir().expect("model directory");
        std::fs::write(
            models.path().join("GenericActuator.json"),
            r#"{
                "name": "GenericActuator",
                "M": [],
                "A": [{"id": 1, "name": "Speed", "unit": "%", "type": "number"}],
                "P": []
            }"#,
        )
        .expect("generic model fixture");
        let library = ProductLibrary::load(Some(models.path())).expect("load model fixture");
        let cache = Arc::new(RoutingCache::from_maps(
            Default::default(),
            std::collections::HashMap::from([("7:A:1".to_string(), "99:A:99".to_string())]),
            Default::default(),
        ));
        let manager = Arc::new(InstanceManager::new(
            pool.clone(),
            Arc::clone(&cache),
            Arc::new(ProductLoader::with_library(pool.clone(), Arc::new(library))),
        ));
        let mutator = SqliteActionRoutingMutator::new(Arc::clone(&manager));
        Self {
            _models: models,
            pool,
            cache,
            manager,
            mutator,
        }
    }

    async fn router(&self) -> axum::Router {
        let audit = Arc::new(aether_store_local::MemoryAuditSink::new());
        let audit_port: Arc<dyn AuditSink> = audit;
        let physical_sink = Arc::new(ShmDeviceCommandSink::new());
        let dispatcher: Arc<dyn CommandDispatcher> = Arc::new(AutomationCommandDispatcher::new(
            Arc::clone(&self.manager),
            physical_sink.clone() as Arc<dyn DeviceCommandSink>,
        ));
        let control = Arc::new(ControlApplication::new(
            dispatcher,
            Arc::clone(&audit_port),
            SafetyPolicy,
        ));
        let action_routing = Arc::new(ActionRoutingApplication::new(
            Arc::new(SqliteActionRoutingMutator::new(Arc::clone(&self.manager))),
            audit_port,
            SafetyPolicy,
        ));
        let authenticator =
            Arc::new(ControlAuthenticator::new(JWT_SECRET, None).expect("routing authenticator"));
        let state = Arc::new(AppState::new(
            Arc::new(aether_automation::config::AutomationConfig::default()),
            Arc::clone(&self.manager),
            control,
            action_routing,
            authenticator,
            physical_sink,
        ));
        aether_automation::routes::create_routes(state)
    }
}

#[derive(Serialize)]
struct AccessClaims<'a> {
    user_id: i64,
    role: &'a str,
    exp: usize,
    iat: usize,
    #[serde(rename = "type")]
    token_type: &'a str,
}

fn access_token() -> String {
    let now = chrono::Utc::now().timestamp();
    encode(
        &Header::new(Algorithm::HS256),
        &AccessClaims {
            user_id: 7,
            role: "Engineer",
            exp: (now + 3_600) as usize,
            iat: now as usize,
            token_type: "access",
        },
        &EncodingKey::from_secret(JWT_SECRET.as_bytes()),
    )
    .expect("encode access token")
}

async fn action_route_request(
    router: &axum::Router,
    authenticated: bool,
    confirmed: bool,
) -> (StatusCode, serde_json::Value) {
    let request_id = "018f0000-0000-7000-8000-000000000077";
    let mut request = Request::builder()
        .method("PUT")
        .uri("/api/instances/7/actions/1/routing")
        .header("content-type", "application/json")
        .header("x-request-id", request_id);
    if authenticated {
        request = request.header("authorization", format!("Bearer {}", access_token()));
    }
    let body = serde_json::json!({
        "channel_id": 3,
        "four_remote": "A",
        "channel_point_id": 5,
        "enabled": true,
        "confirmed": confirmed
    });
    let response = router
        .clone()
        .oneshot(
            request
                .body(Body::from(serde_json::to_vec(&body).expect("request body")))
                .expect("request"),
        )
        .await
        .expect("routing response");
    let status = response.status();
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("response body")
        .to_bytes();
    let body = serde_json::from_slice(&bytes).unwrap_or_else(|_| serde_json::json!({}));
    (status, body)
}

fn route(instance_id: u32, action_id: u32, channel_id: u32, point_id: u32) -> ActionRoute {
    ActionRoute::new(
        ActionRouteKey::new(InstanceId::new(instance_id), PointId::new(action_id)),
        ChannelCommandAddress::new(
            ChannelId::new(channel_id),
            PointKind::Action,
            PointId::new(point_id),
        )
        .expect("command-owned destination"),
        true,
    )
}

#[tokio::test]
async fn mutations_publish_the_committed_m2c_view_and_revoke_it_on_disable_or_delete() {
    let fixture = RoutingFixture::complete().await;
    let key = ActionRouteKey::new(InstanceId::new(7), PointId::new(1));

    let upsert = fixture
        .mutator
        .mutate(ActionRoutingMutation::upsert(route(7, 1, 3, 5)))
        .await
        .expect("upsert action route");
    assert_eq!(upsert.affected_routes(), 1);
    let stored: (i64, String, i64, bool) = sqlx::query_as(
        "SELECT channel_id, channel_type, channel_point_id, enabled \
         FROM action_routing WHERE instance_id = 7 AND action_id = 1",
    )
    .fetch_one(&fixture.pool)
    .await
    .expect("stored action route");
    assert_eq!(stored, (3, "A".to_string(), 5, true));
    let published = fixture
        .cache
        .lookup_m2c_by_parts(7, aether_model::PointType::Adjustment, 1)
        .expect("published M2C route");
    assert_eq!(published.channel_id, 3);
    assert_eq!(published.point_id, 5);

    fixture
        .mutator
        .mutate(ActionRoutingMutation::set_enabled(key, false))
        .await
        .expect("disable route");
    assert!(
        fixture
            .cache
            .lookup_m2c_by_parts(7, aether_model::PointType::Adjustment, 1)
            .is_none()
    );

    fixture
        .mutator
        .mutate(ActionRoutingMutation::set_enabled(key, true))
        .await
        .expect("enable route");
    assert!(
        fixture
            .cache
            .lookup_m2c_by_parts(7, aether_model::PointType::Adjustment, 1)
            .is_some()
    );

    fixture
        .mutator
        .mutate(ActionRoutingMutation::delete(key))
        .await
        .expect("delete route");
    let remaining: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM action_routing")
        .fetch_one(&fixture.pool)
        .await
        .expect("route count");
    assert_eq!(remaining, 0);
    assert!(
        fixture
            .cache
            .lookup_m2c_by_parts(7, aether_model::PointType::Adjustment, 1)
            .is_none()
    );
}

#[tokio::test]
async fn invalid_logical_or_physical_targets_are_rejected_without_persistence() {
    let fixture = RoutingFixture::complete().await;

    let logical_error = fixture
        .mutator
        .mutate(ActionRoutingMutation::upsert(route(7, 99, 3, 5)))
        .await
        .expect_err("undeclared logical action must fail");
    assert_eq!(logical_error.kind(), PortErrorKind::InvalidData);

    let physical_error = fixture
        .mutator
        .mutate(ActionRoutingMutation::upsert(route(7, 1, 3, 99)))
        .await
        .expect_err("missing physical action must fail");
    assert_eq!(physical_error.kind(), PortErrorKind::InvalidData);

    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM action_routing")
        .fetch_one(&fixture.pool)
        .await
        .expect("route count");
    assert_eq!(count, 0);
}

#[tokio::test]
async fn publication_failure_revokes_the_previous_runtime_route() {
    let fixture = RoutingFixture::missing_measurement_table().await;
    assert!(
        fixture
            .cache
            .lookup_m2c_by_parts(7, aether_model::PointType::Adjustment, 1)
            .is_some(),
        "fixture starts with a stale route"
    );

    let error = fixture
        .mutator
        .mutate(ActionRoutingMutation::upsert(route(7, 1, 3, 5)))
        .await
        .expect_err("incomplete routing schema must fail publication");
    assert_eq!(error.kind(), PortErrorKind::Unavailable);
    assert!(
        fixture
            .cache
            .lookup_m2c_by_parts(7, aether_model::PointType::Adjustment, 1)
            .is_none(),
        "stale physical command route must be revoked fail-closed"
    );
}

#[tokio::test]
async fn http_action_routing_requires_identity_and_confirmation_before_database_effects() {
    let fixture = RoutingFixture::complete().await;
    let router = fixture.router().await;

    let (denied, _) = action_route_request(&router, false, true).await;
    assert_eq!(denied, StatusCode::FORBIDDEN);
    let (unconfirmed, _) = action_route_request(&router, true, false).await;
    assert_eq!(unconfirmed, StatusCode::UNPROCESSABLE_ENTITY);
    let before: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM action_routing")
        .fetch_one(&fixture.pool)
        .await
        .expect("route count before acceptance");
    assert_eq!(before, 0);

    let (accepted, body) = action_route_request(&router, true, true).await;
    assert_eq!(accepted, StatusCode::OK, "{body}");
    assert_eq!(
        body["data"]["request_id"],
        "018f0000-0000-7000-8000-000000000077"
    );
    assert_eq!(body["data"]["audit"]["status"], "recorded");
    assert_eq!(body["data"]["affected_routes"], 1);
    let after: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM action_routing")
        .fetch_one(&fixture.pool)
        .await
        .expect("route count after acceptance");
    assert_eq!(after, 1);
}
