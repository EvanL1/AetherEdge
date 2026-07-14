//! Governed action-routing persistence and runtime-publication contracts.

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
use aether_domain::{ChannelCommandAddress, ChannelId, InstanceId, PointId, PointKind};
use aether_model::product_lib::ProductLibrary;
use aether_ports::{
    ActionRoute, ActionRouteKey, ActionRoutingMutation, AuditSink, AutomationActionRoutingMutator,
    AutomationMeasurementRoutingMutator, CommandDispatcher, DeviceCommandSink,
    LogicalRoutingRevision, MeasurementRoute, MeasurementRouteKey, MeasurementRoutingMutation,
    PortErrorKind, RevisionedActionRoutingMutation,
};
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
        common::test_utils::schema::initialize_configuration_revisions(&pool)
            .await
            .expect("logical-routing revision");
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
        let manager = Arc::new(InstanceManager::new(
            pool.clone(),
            Arc::new(ProductLoader::with_library(pool.clone(), Arc::new(library))),
        ));
        let mutator = SqliteActionRoutingMutator::new(Arc::clone(&manager));
        Self {
            _models: models,
            pool,
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
            Arc::clone(&audit_port),
            SafetyPolicy,
        ));
        let measurement_routing = Arc::new(MeasurementRoutingApplication::new(
            Arc::new(SqliteMeasurementRoutingMutator::new(Arc::clone(
                &self.manager,
            ))),
            Arc::clone(&audit_port),
            SafetyPolicy,
        ));
        let instance_configuration = Arc::new(
            aether_automation::instance_configuration::InstanceConfigurationApplication::new(
                Arc::clone(&self.manager),
                audit_port,
            ),
        );
        let authenticator =
            Arc::new(ControlAuthenticator::new(JWT_SECRET, None).expect("routing authenticator"));
        let state = Arc::new(AppState::new(
            Arc::new(aether_automation::config::AutomationConfig::default()),
            Arc::clone(&self.manager),
            control,
            action_routing,
            measurement_routing,
            instance_configuration,
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
    expected_revision: u64,
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
        "expected_revision": expected_revision,
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

const fn revision(value: u64) -> LogicalRoutingRevision {
    LogicalRoutingRevision::new(value)
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
async fn mixed_global_delete_is_rejected_before_either_plane_changes() {
    let fixture = RoutingFixture::complete().await;
    for statement in [
        "INSERT INTO action_routing \
         (instance_id, instance_name, action_id, channel_id, channel_type, channel_point_id, enabled) \
         VALUES (7, 'actuator_7', 1, 3, 'A', 5, 1)",
        "INSERT INTO measurement_routing \
         (instance_id, instance_name, measurement_id, channel_id, channel_type, channel_point_id, enabled) \
         VALUES (7, 'actuator_7', 1, 3, 'T', 5, 1)",
    ] {
        sqlx::query(statement)
            .execute(&fixture.pool)
            .await
            .expect("seed mixed routing");
    }
    let router = fixture.router().await;
    let response = router
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/api/routing?confirm=true&expected_revision=1")
                .header("authorization", format!("Bearer {}", access_token()))
                .body(Body::empty())
                .expect("mixed delete request"),
        )
        .await
        .expect("mixed delete response");

    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let (measurement_count, action_count, head): (i64, i64, i64) = (
        sqlx::query_scalar("SELECT COUNT(*) FROM measurement_routing")
            .fetch_one(&fixture.pool)
            .await
            .expect("measurement count"),
        sqlx::query_scalar("SELECT COUNT(*) FROM action_routing")
            .fetch_one(&fixture.pool)
            .await
            .expect("action count"),
        sqlx::query_scalar(
            "SELECT revision FROM configuration_revisions WHERE scope = 'logical_routing'",
        )
        .fetch_one(&fixture.pool)
        .await
        .expect("logical-routing head"),
    );
    assert_eq!((measurement_count, action_count, head), (1, 1, 1));
}

#[tokio::test]
async fn mutations_publish_the_committed_m2c_view_and_revoke_it_on_disable_or_delete() {
    let fixture = RoutingFixture::complete().await;
    let key = ActionRouteKey::new(InstanceId::new(7), PointId::new(1));

    let upsert = fixture
        .mutator
        .mutate_revisioned(RevisionedActionRoutingMutation::upsert(
            route(7, 1, 3, 5),
            revision(1),
        ))
        .await
        .expect("upsert action route");
    assert_eq!(upsert.affected_routes(), 1);
    assert_eq!(upsert.resulting_revision(), revision(2));
    let stored: (i64, String, i64, bool) = sqlx::query_as(
        "SELECT channel_id, channel_type, channel_point_id, enabled \
         FROM action_routing WHERE instance_id = 7 AND action_id = 1",
    )
    .fetch_one(&fixture.pool)
    .await
    .expect("stored action route");
    assert_eq!(stored, (3, "A".to_string(), 5, true));
    fixture
        .mutator
        .mutate_revisioned(RevisionedActionRoutingMutation::set_enabled(
            key,
            false,
            revision(2),
        ))
        .await
        .expect("disable route");
    let enabled: bool = sqlx::query_scalar(
        "SELECT enabled FROM action_routing WHERE instance_id = 7 AND action_id = 1",
    )
    .fetch_one(&fixture.pool)
    .await
    .expect("disabled route");
    assert!(!enabled);

    fixture
        .mutator
        .mutate_revisioned(RevisionedActionRoutingMutation::set_enabled(
            key,
            true,
            revision(3),
        ))
        .await
        .expect("enable route");
    let enabled: bool = sqlx::query_scalar(
        "SELECT enabled FROM action_routing WHERE instance_id = 7 AND action_id = 1",
    )
    .fetch_one(&fixture.pool)
    .await
    .expect("enabled route");
    assert!(enabled);

    fixture
        .mutator
        .mutate_revisioned(RevisionedActionRoutingMutation::delete(key, revision(4)))
        .await
        .expect("delete route");
    let remaining: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM action_routing")
        .fetch_one(&fixture.pool)
        .await
        .expect("route count");
    assert_eq!(remaining, 0);
    let head: i64 = sqlx::query_scalar(
        "SELECT revision FROM configuration_revisions WHERE scope = 'logical_routing'",
    )
    .fetch_one(&fixture.pool)
    .await
    .expect("logical-routing head");
    assert_eq!(head, 5);
}

#[tokio::test]
async fn legacy_rust_action_mutation_reads_the_current_head_and_uses_the_cas_path() {
    let fixture = RoutingFixture::complete().await;

    let receipt = fixture
        .mutator
        .mutate(ActionRoutingMutation::upsert(route(7, 1, 3, 5)))
        .await
        .expect("legacy Rust action mutation");

    assert_eq!(receipt.resulting_revision(), revision(2));
    assert!(receipt.runtime_status().is_published());
}

#[tokio::test]
async fn invalid_logical_or_physical_targets_are_rejected_without_persistence() {
    let fixture = RoutingFixture::complete().await;

    let logical_error = fixture
        .mutator
        .mutate_revisioned(RevisionedActionRoutingMutation::upsert(
            route(7, 99, 3, 5),
            revision(1),
        ))
        .await
        .expect_err("undeclared logical action must fail");
    assert_eq!(logical_error.kind(), PortErrorKind::InvalidData);

    let physical_error = fixture
        .mutator
        .mutate_revisioned(RevisionedActionRoutingMutation::upsert(
            route(7, 1, 3, 99),
            revision(1),
        ))
        .await
        .expect_err("missing physical action must fail");
    assert_eq!(physical_error.kind(), PortErrorKind::InvalidData);

    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM action_routing")
        .fetch_one(&fixture.pool)
        .await
        .expect("route count");
    assert_eq!(count, 0);
    let head: i64 = sqlx::query_scalar(
        "SELECT revision FROM configuration_revisions WHERE scope = 'logical_routing'",
    )
    .fetch_one(&fixture.pool)
    .await
    .expect("logical-routing head after rejected validation");
    assert_eq!(head, 1, "validation failure must roll back the CAS");
}

#[tokio::test]
async fn stale_revision_conflicts_without_route_or_head_changes() {
    let fixture = RoutingFixture::complete().await;
    let key = ActionRouteKey::new(InstanceId::new(7), PointId::new(1));
    fixture
        .mutator
        .mutate_revisioned(RevisionedActionRoutingMutation::upsert(
            route(7, 1, 3, 5),
            revision(1),
        ))
        .await
        .expect("initial CAS mutation");

    let error = fixture
        .mutator
        .mutate_revisioned(RevisionedActionRoutingMutation::set_enabled(
            key,
            false,
            revision(1),
        ))
        .await
        .expect_err("stale action mutation must conflict");
    assert_eq!(error.kind(), PortErrorKind::Conflict);
    let stored_enabled: bool = sqlx::query_scalar(
        "SELECT enabled FROM action_routing WHERE instance_id = 7 AND action_id = 1",
    )
    .fetch_one(&fixture.pool)
    .await
    .expect("stored action route");
    assert!(stored_enabled);
    let head: i64 = sqlx::query_scalar(
        "SELECT revision FROM configuration_revisions WHERE scope = 'logical_routing'",
    )
    .fetch_one(&fixture.pool)
    .await
    .expect("logical-routing head");
    assert_eq!(head, 2);
}

#[tokio::test]
async fn action_commit_invalidates_measurement_commands_fenced_by_the_same_head() {
    let fixture = RoutingFixture::complete().await;
    let action_receipt = fixture
        .mutator
        .mutate_revisioned(RevisionedActionRoutingMutation::upsert(
            route(7, 1, 3, 5),
            revision(1),
        ))
        .await
        .expect("action mutation advances the shared head");
    assert_eq!(action_receipt.resulting_revision(), revision(2));

    let destination = aether_domain::ChannelPointAddress::new(
        ChannelId::new(3),
        PointKind::Telemetry,
        PointId::new(5),
    )
    .expect("acquisition-owned destination");
    let measurement_mutator = SqliteMeasurementRoutingMutator::new(Arc::clone(&fixture.manager));
    let error = measurement_mutator
        .mutate(MeasurementRoutingMutation::upsert(
            MeasurementRoute::new(
                MeasurementRouteKey::new(InstanceId::new(7), PointId::new(1)),
                destination,
                true,
            ),
            revision(1),
        ))
        .await
        .expect_err("measurement command fenced by the old shared head must conflict");
    assert_eq!(error.kind(), PortErrorKind::Conflict);
}

#[tokio::test]
async fn publication_failure_revokes_the_previous_runtime_route() {
    let fixture = RoutingFixture::missing_measurement_table().await;
    assert!(fixture.manager.runtime_topology().is_none());

    let receipt = fixture
        .mutator
        .mutate_revisioned(RevisionedActionRoutingMutation::upsert(
            route(7, 1, 3, 5),
            revision(1),
        ))
        .await
        .expect("durably committed routing must return a degraded receipt");
    assert!(!receipt.runtime_status().is_published());
    assert!(receipt.runtime_status().reconciliation_required());
    assert_eq!(receipt.resulting_revision(), revision(2));
    assert_eq!(
        receipt
            .runtime_status()
            .failure()
            .expect("publication failure")
            .kind(),
        PortErrorKind::Unavailable
    );
    assert!(
        fixture.manager.runtime_topology().is_none(),
        "a failed publication must not create an alternate route owner"
    );
}

#[tokio::test]
async fn http_reports_committed_publication_degradation_as_non_retryable_acceptance() {
    let fixture = RoutingFixture::missing_measurement_table().await;
    let router = fixture.router().await;

    let (status, body) = action_route_request(&router, true, true, 1).await;

    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"]["runtime"]["status"], "commands_revoked");
    assert_eq!(body["data"]["runtime"]["reconciliation_required"], true);
    assert_eq!(body["data"]["retryable"], false);
    assert_eq!(body["data"]["resulting_revision"], 2);
    let stored: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM action_routing")
        .fetch_one(&fixture.pool)
        .await
        .expect("committed action route count");
    assert_eq!(stored, 1);
}

#[tokio::test]
async fn http_action_routing_requires_identity_and_confirmation_before_database_effects() {
    let fixture = RoutingFixture::complete().await;
    let router = fixture.router().await;

    let (denied, _) = action_route_request(&router, false, true, 1).await;
    assert_eq!(denied, StatusCode::FORBIDDEN);
    let (unconfirmed, _) = action_route_request(&router, true, false, 1).await;
    assert_eq!(unconfirmed, StatusCode::UNPROCESSABLE_ENTITY);
    let before: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM action_routing")
        .fetch_one(&fixture.pool)
        .await
        .expect("route count before acceptance");
    assert_eq!(before, 0);

    let (accepted, body) = action_route_request(&router, true, true, 1).await;
    assert_eq!(accepted, StatusCode::OK, "{body}");
    assert_eq!(
        body["data"]["request_id"],
        "018f0000-0000-7000-8000-000000000077"
    );
    assert_eq!(body["data"]["audit"]["status"], "recorded");
    assert_eq!(body["data"]["affected_routes"], 1);
    assert_eq!(body["data"]["resulting_revision"], 2);
    assert_eq!(body["data"]["runtime"]["status"], "published");
    assert_eq!(body["data"]["runtime"]["reconciliation_required"], false);
    let after: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM action_routing")
        .fetch_one(&fixture.pool)
        .await
        .expect("route count after acceptance");
    assert_eq!(after, 1);
}

#[tokio::test]
async fn http_action_routing_rejects_a_stale_shared_revision() {
    let fixture = RoutingFixture::complete().await;
    let router = fixture.router().await;

    let (accepted, _) = action_route_request(&router, true, true, 1).await;
    assert_eq!(accepted, StatusCode::OK);
    let (conflict, body) = action_route_request(&router, true, true, 1).await;
    assert_eq!(conflict, StatusCode::CONFLICT, "{body}");

    let head: i64 = sqlx::query_scalar(
        "SELECT revision FROM configuration_revisions WHERE scope = 'logical_routing'",
    )
    .fetch_one(&fixture.pool)
    .await
    .expect("logical-routing head");
    assert_eq!(head, 2);
}
