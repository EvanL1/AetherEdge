//! Governed measurement-routing CAS, validation, audit, and publication contracts.

#![allow(clippy::disallowed_methods)]

use std::sync::Arc;

use aether_application::{
    ActionRoutingApplication, ControlApplication, MeasurementRoutingApplication, SafetyPolicy,
};
use aether_automation::app_state::AppState;
use aether_automation::infra::action_routing::SqliteActionRoutingMutator;
use aether_automation::infra::application_control::{
    AutomationCommandDispatcher, ControlAuthenticator,
};
use aether_automation::infra::measurement_routing::SqliteMeasurementRoutingMutator;
use aether_automation::{InstanceManager, ProductLoader};
use aether_model::product_lib::ProductLibrary;
use aether_ports::{AuditSink, CommandDispatcher, DeviceCommandSink};
use aether_shm_bridge::ShmDeviceCommandSink;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};
use serde::Serialize;
use sqlx::SqlitePool;
use tower::ServiceExt;

const JWT_SECRET: &str = "0123456789abcdef0123456789abcdef";

struct Fixture {
    _models: tempfile::TempDir,
    pool: SqlitePool,
    router: axum::Router,
}

impl Fixture {
    async fn new() -> Self {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("routing database");
        common::test_utils::schema::init_automation_schema(&pool)
            .await
            .expect("automation schema");
        common::test_utils::schema::init_io_schema(&pool)
            .await
            .expect("IO schema");
        for statement in [
            "INSERT INTO instances (instance_id, instance_name, product_name) \
             VALUES (7, 'sensor_7', 'GenericSensor')",
            "INSERT INTO channels (channel_id, name, protocol, enabled) \
             VALUES (3, 'fieldbus_3', 'virtual', 1)",
            "INSERT INTO telemetry_points (channel_id, point_id, signal_name) \
             VALUES (3, 5, 'temperature')",
            "INSERT INTO signal_points (channel_id, point_id, signal_name) \
             VALUES (3, 6, 'running')",
        ] {
            sqlx::query(statement)
                .execute(&pool)
                .await
                .expect("routing fixture statement");
        }

        let models = tempfile::tempdir().expect("model directory");
        std::fs::write(
            models.path().join("GenericSensor.json"),
            r#"{
                "name": "GenericSensor",
                "M": [{"id": 1, "name": "Temperature", "unit": "C", "type": "number"}],
                "A": [],
                "P": []
            }"#,
        )
        .expect("model fixture");
        let library = ProductLibrary::load(Some(models.path())).expect("load model");
        let manager = Arc::new(InstanceManager::new(
            pool.clone(),
            Arc::new(ProductLoader::with_library(pool.clone(), Arc::new(library))),
        ));
        let audit: Arc<dyn AuditSink> = Arc::new(
            aether_store_local::SqliteAuditSink::initialize(pool.clone())
                .await
                .expect("audit sink"),
        );
        let physical_sink = Arc::new(ShmDeviceCommandSink::new());
        let dispatcher: Arc<dyn CommandDispatcher> = Arc::new(AutomationCommandDispatcher::new(
            Arc::clone(&manager),
            physical_sink.clone() as Arc<dyn DeviceCommandSink>,
        ));
        let state = Arc::new(AppState::new(
            Arc::new(aether_automation::config::AutomationConfig::default()),
            Arc::clone(&manager),
            Arc::new(ControlApplication::new(
                dispatcher,
                Arc::clone(&audit),
                SafetyPolicy,
            )),
            Arc::new(ActionRoutingApplication::new(
                Arc::new(SqliteActionRoutingMutator::new(Arc::clone(&manager))),
                Arc::clone(&audit),
                SafetyPolicy,
            )),
            Arc::new(MeasurementRoutingApplication::new(
                Arc::new(SqliteMeasurementRoutingMutator::new(Arc::clone(&manager))),
                Arc::clone(&audit),
                SafetyPolicy,
            )),
            Arc::new(
                aether_automation::instance_configuration::InstanceConfigurationApplication::new(
                    manager, audit,
                ),
            ),
            Arc::new(ControlAuthenticator::new(JWT_SECRET, None).expect("authenticator")),
            physical_sink,
        ));
        let router = aether_automation::routes::create_routes(state);
        Self {
            _models: models,
            pool,
            router,
        }
    }

    async fn put(
        &self,
        expected_revision: u64,
        channel_point_id: u32,
    ) -> (StatusCode, serde_json::Value) {
        let body = serde_json::json!({
            "channel_id": 3,
            "four_remote": "T",
            "channel_point_id": channel_point_id,
            "enabled": true,
            "expected_revision": expected_revision,
            "confirmed": true
        });
        self.request("PUT", body).await
    }

    async fn request(
        &self,
        method: &str,
        body: serde_json::Value,
    ) -> (StatusCode, serde_json::Value) {
        let response = self
            .router
            .clone()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri("/api/instances/7/measurements/1/routing")
                    .header("content-type", "application/json")
                    .header("authorization", format!("Bearer {}", access_token()))
                    .header("x-request-id", uuid::Uuid::new_v4().to_string())
                    .body(Body::from(
                        serde_json::to_vec(&body).expect("serialize request"),
                    ))
                    .expect("request"),
            )
            .await
            .expect("response");
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
    .expect("access token")
}

#[tokio::test]
async fn upsert_uses_shared_revision_validates_and_publishes_with_durable_audit() {
    let fixture = Fixture::new().await;

    let (status, body) = fixture.put(1, 5).await;

    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"]["resulting_revision"], 2);
    assert_eq!(body["data"]["audit"]["status"], "recorded");
    assert_eq!(body["data"]["runtime"]["status"], "published");
    let stored: (i64, String, i64, bool) = sqlx::query_as(
        "SELECT channel_id, channel_type, channel_point_id, enabled \
         FROM measurement_routing WHERE instance_id = 7 AND measurement_id = 1",
    )
    .fetch_one(&fixture.pool)
    .await
    .expect("stored route");
    assert_eq!(stored, (3, "T".to_string(), 5, true));
    let revision: i64 = sqlx::query_scalar(
        "SELECT revision FROM configuration_revisions WHERE scope = 'logical_routing'",
    )
    .fetch_one(&fixture.pool)
    .await
    .expect("routing revision");
    assert_eq!(revision, 2);
    let outcomes: Vec<String> =
        sqlx::query_scalar("SELECT outcome FROM command_audit_events ORDER BY id")
            .fetch_all(&fixture.pool)
            .await
            .expect("audit events");
    assert_eq!(outcomes, ["attempted", "succeeded"]);
}

#[tokio::test]
async fn stale_or_invalid_upsert_rolls_back_both_route_and_shared_revision() {
    let fixture = Fixture::new().await;
    assert_eq!(fixture.put(1, 5).await.0, StatusCode::OK);

    let (stale, _) = fixture.put(1, 6).await;
    assert_ne!(stale, StatusCode::OK);
    let (invalid, _) = fixture.put(2, 99).await;
    assert_eq!(invalid, StatusCode::UNPROCESSABLE_ENTITY);

    let stored_point: i64 = sqlx::query_scalar(
        "SELECT channel_point_id FROM measurement_routing \
         WHERE instance_id = 7 AND measurement_id = 1",
    )
    .fetch_one(&fixture.pool)
    .await
    .expect("stored point");
    assert_eq!(stored_point, 5);
    let revision: i64 = sqlx::query_scalar(
        "SELECT revision FROM configuration_revisions WHERE scope = 'logical_routing'",
    )
    .fetch_one(&fixture.pool)
    .await
    .expect("routing revision");
    assert_eq!(revision, 2);
}

#[tokio::test]
async fn delete_requires_the_current_revision_and_advances_the_shared_head() {
    let fixture = Fixture::new().await;
    assert_eq!(fixture.put(1, 5).await.0, StatusCode::OK);

    let (status, body) = fixture
        .request(
            "DELETE",
            serde_json::json!({"expected_revision": 2, "confirmed": true}),
        )
        .await;

    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"]["resulting_revision"], 3);
    let remaining: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM measurement_routing")
        .fetch_one(&fixture.pool)
        .await
        .expect("route count");
    assert_eq!(remaining, 0);
}
