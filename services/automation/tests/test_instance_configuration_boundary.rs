//! Instance desired-state CAS, audit, projection, and subtree atomicity contracts.

#![allow(clippy::disallowed_methods)]

use std::collections::HashMap;
use std::sync::Arc;

use aether_application::{
    ActionRoutingApplication, Actor, ControlApplication, MeasurementRoutingApplication,
    RequestContext, SafetyPolicy,
};
use aether_automation::app_state::AppState;
use aether_automation::infra::action_routing::SqliteActionRoutingMutator;
use aether_automation::infra::application_control::{
    AutomationCommandDispatcher, ControlAuthenticator,
};
use aether_automation::infra::measurement_routing::SqliteMeasurementRoutingMutator;
use aether_automation::instance_configuration::{
    InstanceConfigurationApplication, InstanceConfigurationMutation, InstanceConfigurationPayload,
    InstanceConfigurationRevision, initialize_instance_configuration_revision,
};
use aether_automation::{AutomationError, InstanceManager, ProductLoader};
use aether_domain::TimestampMs;
use aether_model::product_lib::ProductLibrary;
use aether_ports::{
    AuditSink, CommandDispatcher, DeviceCommandSink, PortError, PortErrorKind, PortResult,
};
use aether_store_local::SqliteAuditSink;
use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::routing::{get, post};
use http_body_util::BodyExt;
use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};
use serde::Serialize;
use serde_json::json;
use sqlx::SqlitePool;
use tower::ServiceExt;

const JWT_SECRET: &str = "0123456789abcdef0123456789abcdef";

struct Fixture {
    _models: tempfile::TempDir,
    pool: SqlitePool,
    manager: Arc<InstanceManager>,
    audit: Arc<dyn AuditSink>,
    application: Arc<InstanceConfigurationApplication>,
}

impl Fixture {
    async fn new() -> Self {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("instance database");
        common::test_utils::schema::init_automation_schema(&pool)
            .await
            .expect("automation schema");
        common::test_utils::schema::init_io_schema(&pool)
            .await
            .expect("IO schema");
        common::test_utils::schema::install_logical_routing_integrity_triggers(&pool)
            .await
            .expect("routing integrity triggers");
        initialize_instance_configuration_revision(&pool)
            .await
            .expect("instances revision");

        let models = tempfile::tempdir().expect("model directory");
        std::fs::write(
            models.path().join("Fixture.json"),
            r#"{
                "name": "Fixture",
                "M": [{"id": 1, "name": "Reading", "unit": "", "type": "number"}],
                "A": [],
                "P": [
                    {"id": 1, "name": "serial", "unit": "", "type": "string"},
                    {"id": 2, "name": "site", "unit": "", "type": "string"}
                ]
            }"#,
        )
        .expect("model fixture");
        let library = ProductLibrary::load(Some(models.path())).expect("load model fixture");
        let manager = Arc::new(InstanceManager::new(
            pool.clone(),
            Arc::new(ProductLoader::with_library(pool.clone(), Arc::new(library))),
        ));
        manager
            .populate_name_cache()
            .await
            .expect("initial name cache");
        let audit: Arc<dyn AuditSink> = Arc::new(
            SqliteAuditSink::initialize(pool.clone())
                .await
                .expect("audit schema"),
        );
        let application = Arc::new(InstanceConfigurationApplication::new(
            Arc::clone(&manager),
            Arc::clone(&audit),
        ));
        Self {
            _models: models,
            pool,
            manager,
            audit,
            application,
        }
    }

    async fn create(
        &self,
        request_id: &str,
        id: u32,
        name: &str,
        parent_id: Option<u32>,
        expected_revision: u64,
    ) -> Result<u64, AutomationError> {
        let acceptance = self
            .application
            .mutate(
                &confirmed_context(request_id),
                InstanceConfigurationMutation::Create {
                    request: aether_automation::CreateInstanceRequest {
                        instance_id: Some(id),
                        instance_name: name.to_string(),
                        product_name: "Fixture".to_string(),
                        parent_id,
                        properties: HashMap::from([("serial".to_string(), json!(name))]),
                    },
                    expected_revision: InstanceConfigurationRevision::new(expected_revision),
                },
            )
            .await?;
        Ok(acceptance.resulting_revision().get())
    }

    fn router(&self) -> axum::Router {
        let physical_sink = Arc::new(aether_shm_bridge::ShmDeviceCommandSink::new());
        let dispatcher: Arc<dyn CommandDispatcher> = Arc::new(AutomationCommandDispatcher::new(
            Arc::clone(&self.manager),
            physical_sink.clone() as Arc<dyn DeviceCommandSink>,
        ));
        let state = Arc::new(AppState::new(
            Arc::new(aether_automation::config::AutomationConfig::default()),
            Arc::clone(&self.manager),
            Arc::new(ControlApplication::new(
                dispatcher,
                Arc::clone(&self.audit),
                SafetyPolicy,
            )),
            Arc::new(ActionRoutingApplication::new(
                Arc::new(SqliteActionRoutingMutator::new(Arc::clone(&self.manager))),
                Arc::clone(&self.audit),
                SafetyPolicy,
            )),
            Arc::new(MeasurementRoutingApplication::new(
                Arc::new(SqliteMeasurementRoutingMutator::new(Arc::clone(
                    &self.manager,
                ))),
                Arc::clone(&self.audit),
                SafetyPolicy,
            )),
            Arc::clone(&self.application),
            Arc::new(ControlAuthenticator::new(JWT_SECRET, None).expect("authenticator")),
            physical_sink,
        ));
        axum::Router::new()
            .route(
                "/api/instances/revision",
                get(
                    aether_automation::api::instance_management_handlers::get_instance_configuration_revision,
                ),
            )
            .route(
                "/api/instances",
                post(aether_automation::api::instance_management_handlers::create_instance),
            )
            .with_state(state)
    }
}

fn confirmed_context(request_id: &str) -> RequestContext {
    RequestContext::new(
        request_id,
        Actor::new("commissioner").with_permission("automation.instance.manage"),
        true,
        TimestampMs::new(1_720_000_000_000),
    )
}

#[tokio::test]
async fn create_and_atomic_rename_advance_only_instances_revision_and_publish_names() {
    let fixture = Fixture::new().await;
    assert_eq!(
        fixture
            .create("create", 7, "before", None, 1)
            .await
            .unwrap(),
        2
    );

    for statement in [
        "INSERT INTO channels (channel_id, name, protocol, enabled) VALUES (3, 'bus', 'virtual', 1)",
        "INSERT INTO telemetry_points (channel_id, point_id, signal_name) VALUES (3, 5, 'reading')",
        "INSERT INTO measurement_routing (instance_id, instance_name, channel_id, channel_type, channel_point_id, measurement_id, enabled) VALUES (7, 'before', 3, 'T', 5, 1, 1)",
    ] {
        sqlx::query(statement)
            .execute(&fixture.pool)
            .await
            .expect("routing fixture");
    }
    let logical_before: i64 = sqlx::query_scalar(
        "SELECT revision FROM configuration_revisions WHERE scope = 'logical_routing'",
    )
    .fetch_one(&fixture.pool)
    .await
    .unwrap();

    let acceptance = fixture
        .application
        .mutate(
            &confirmed_context("rename"),
            InstanceConfigurationMutation::Update {
                instance_id: 7,
                instance_name: Some("after".to_string()),
                properties: Some(HashMap::from([("serial".to_string(), json!("v2"))])),
                expected_revision: InstanceConfigurationRevision::new(2),
            },
        )
        .await
        .unwrap();
    assert_eq!(acceptance.resulting_revision().get(), 3);
    assert!(matches!(
        acceptance.payload(),
        InstanceConfigurationPayload::Updated { instance_name, .. } if instance_name == "after"
    ));
    assert_eq!(fixture.manager.get_instance_id("after").await.unwrap(), 7);
    assert!(fixture.manager.get_instance_id("before").await.is_err());

    let (instance_name, route_name, property): (String, String, String) = sqlx::query_as(
        "SELECT i.instance_name, r.instance_name, p.value_json FROM instances i \
         JOIN measurement_routing r USING(instance_id) \
         JOIN instance_properties p USING(instance_id) WHERE i.instance_id = 7",
    )
    .fetch_one(&fixture.pool)
    .await
    .unwrap();
    assert_eq!(instance_name, "after");
    assert_eq!(route_name, "after");
    assert_eq!(property, "\"v2\"");
    let logical_after: i64 = sqlx::query_scalar(
        "SELECT revision FROM configuration_revisions WHERE scope = 'logical_routing'",
    )
    .fetch_one(&fixture.pool)
    .await
    .unwrap();
    assert_eq!(logical_after, logical_before);
}

#[tokio::test]
async fn stale_cas_and_invalid_update_roll_back_data_and_revision_with_terminal_audit() {
    let fixture = Fixture::new().await;
    fixture
        .create("create", 7, "stable", None, 1)
        .await
        .unwrap();

    let stale = fixture
        .create("stale", 8, "stale", None, 1)
        .await
        .expect_err("stale CAS must fail");
    assert!(matches!(stale, AutomationError::ConfigurationConflict(_)));

    let invalid = fixture
        .application
        .mutate(
            &confirmed_context("invalid"),
            InstanceConfigurationMutation::Update {
                instance_id: 7,
                instance_name: Some("must_rollback".to_string()),
                properties: Some(HashMap::from([("unknown".to_string(), json!(1))])),
                expected_revision: InstanceConfigurationRevision::new(2),
            },
        )
        .await
        .expect_err("invalid property must roll back rename and CAS");
    assert!(matches!(invalid, AutomationError::InvalidData(_)));

    let (name, revision, count): (String, i64, i64) = sqlx::query_as(
        "SELECT i.instance_name, c.revision, (SELECT COUNT(*) FROM instances) \
         FROM instances i JOIN configuration_revisions c ON c.scope = 'instances' \
         WHERE i.instance_id = 7",
    )
    .fetch_one(&fixture.pool)
    .await
    .unwrap();
    assert_eq!(name, "stable");
    assert_eq!(revision, 2);
    assert_eq!(count, 1);

    let outcomes: Vec<String> = sqlx::query_scalar(
        "SELECT outcome FROM command_audit_events WHERE request_id IN ('stale', 'invalid') ORDER BY id",
    )
    .fetch_all(&fixture.pool)
    .await
    .unwrap();
    assert_eq!(outcomes, ["attempted", "failed", "attempted", "failed"]);
}

#[tokio::test]
async fn single_property_commands_share_instances_revision_and_preserve_siblings() {
    let fixture = Fixture::new().await;
    fixture
        .application
        .mutate(
            &confirmed_context("property-create"),
            InstanceConfigurationMutation::Create {
                request: aether_automation::CreateInstanceRequest {
                    instance_id: Some(7),
                    instance_name: "property-target".to_string(),
                    product_name: "Fixture".to_string(),
                    parent_id: None,
                    properties: HashMap::from([
                        ("serial".to_string(), json!("v1")),
                        ("site".to_string(), json!("east")),
                    ]),
                },
                expected_revision: InstanceConfigurationRevision::new(1),
            },
        )
        .await
        .unwrap();

    let upsert = fixture
        .application
        .mutate(
            &confirmed_context("property-upsert"),
            InstanceConfigurationMutation::UpsertProperty {
                instance_id: 7,
                property_id: 1,
                value: json!("v2"),
                expected_revision: InstanceConfigurationRevision::new(2),
            },
        )
        .await
        .unwrap();
    assert_eq!(upsert.resulting_revision().get(), 3);
    let instance = fixture.manager.get_instance(7).await.unwrap();
    assert_eq!(instance.core.properties["serial"], "v2");
    assert_eq!(instance.core.properties["site"], "east");

    let deleted = fixture
        .application
        .mutate(
            &confirmed_context("property-delete"),
            InstanceConfigurationMutation::DeleteProperty {
                instance_id: 7,
                property_id: 1,
                expected_revision: InstanceConfigurationRevision::new(3),
            },
        )
        .await
        .unwrap();
    assert_eq!(deleted.resulting_revision().get(), 4);
    let instance = fixture.manager.get_instance(7).await.unwrap();
    assert!(!instance.core.properties.contains_key("serial"));
    assert_eq!(instance.core.properties["site"], "east");
}

#[tokio::test]
async fn committed_revision_is_returned_when_cache_publication_requires_reconciliation() {
    let fixture = Fixture::new().await;
    sqlx::query("DROP TABLE action_routing")
        .execute(&fixture.pool)
        .await
        .unwrap();

    let acceptance = fixture
        .application
        .mutate(
            &confirmed_context("degraded-publication"),
            InstanceConfigurationMutation::Create {
                request: aether_automation::CreateInstanceRequest {
                    instance_id: Some(7),
                    instance_name: "committed-degraded".to_string(),
                    product_name: "Fixture".to_string(),
                    parent_id: None,
                    properties: HashMap::new(),
                },
                expected_revision: InstanceConfigurationRevision::new(1),
            },
        )
        .await
        .expect("post-commit projection failure is an acceptance");
    assert_eq!(acceptance.resulting_revision().get(), 2);
    assert!(acceptance.runtime_status().reconciliation_required());
    assert_eq!(acceptance.runtime_status().as_str(), "degraded");
    assert!(!acceptance.is_retryable());
    assert_eq!(instance_count(&fixture.pool).await, 1);
    assert_eq!(instances_revision(&fixture.pool).await, 2);
    assert_eq!(
        fixture
            .manager
            .get_instance_id("committed-degraded")
            .await
            .unwrap(),
        7
    );
}

#[tokio::test]
async fn subtree_delete_prechecks_routes_and_any_delete_failure_rolls_back_every_member() {
    let fixture = Fixture::new().await;
    fixture.create("root", 1, "root", None, 1).await.unwrap();
    fixture
        .create("child", 2, "child", Some(1), 2)
        .await
        .unwrap();
    fixture.create("leaf", 3, "leaf", Some(2), 3).await.unwrap();

    sqlx::query(
        "CREATE TRIGGER fail_child_delete BEFORE DELETE ON instances \
         WHEN OLD.instance_id = 2 BEGIN SELECT RAISE(ABORT, 'injected delete failure'); END",
    )
    .execute(&fixture.pool)
    .await
    .unwrap();
    let failed = fixture
        .application
        .mutate(
            &confirmed_context("delete-fails"),
            InstanceConfigurationMutation::DeleteSubtree {
                instance_id: 1,
                expected_revision: InstanceConfigurationRevision::new(4),
            },
        )
        .await
        .expect_err("one row failure must abort the complete subtree transaction");
    assert!(matches!(failed, AutomationError::DatabaseError(_)));
    assert_eq!(instance_count(&fixture.pool).await, 3);
    assert_eq!(instances_revision(&fixture.pool).await, 4);
    sqlx::query("DROP TRIGGER fail_child_delete")
        .execute(&fixture.pool)
        .await
        .unwrap();

    for statement in [
        "INSERT INTO channels (channel_id, name, protocol, enabled) VALUES (9, 'delete-bus', 'virtual', 1)",
        "INSERT INTO telemetry_points (channel_id, point_id, signal_name) VALUES (9, 1, 'child-reading')",
        "INSERT INTO measurement_routing (instance_id, instance_name, measurement_id, channel_id, channel_type, channel_point_id, enabled) VALUES (2, 'child', 1, 9, 'T', 1, 1)",
    ] {
        sqlx::query(statement).execute(&fixture.pool).await.unwrap();
    }
    let routed = fixture
        .application
        .mutate(
            &confirmed_context("delete-routed"),
            InstanceConfigurationMutation::DeleteSubtree {
                instance_id: 1,
                expected_revision: InstanceConfigurationRevision::new(4),
            },
        )
        .await
        .expect_err("a routed descendant must reject the whole subtree");
    assert!(matches!(routed, AutomationError::ConfigurationConflict(_)));
    assert_eq!(instance_count(&fixture.pool).await, 3);
    assert_eq!(instances_revision(&fixture.pool).await, 4);

    sqlx::query("DELETE FROM measurement_routing WHERE instance_id = 2")
        .execute(&fixture.pool)
        .await
        .unwrap();
    let accepted = fixture
        .application
        .mutate(
            &confirmed_context("delete-ok"),
            InstanceConfigurationMutation::DeleteSubtree {
                instance_id: 1,
                expected_revision: InstanceConfigurationRevision::new(4),
            },
        )
        .await
        .unwrap();
    assert_eq!(accepted.resulting_revision().get(), 5);
    assert_eq!(instance_count(&fixture.pool).await, 0);
}

#[tokio::test]
async fn authentication_confirmation_and_attempt_audit_fail_closed_before_mutation() {
    let fixture = Fixture::new().await;
    let mutation = || InstanceConfigurationMutation::Create {
        request: aether_automation::CreateInstanceRequest {
            instance_id: Some(7),
            instance_name: "blocked".to_string(),
            product_name: "Fixture".to_string(),
            parent_id: None,
            properties: HashMap::new(),
        },
        expected_revision: InstanceConfigurationRevision::new(1),
    };
    let unauthenticated = RequestContext::new(
        "unauthenticated",
        Actor::new("unauthenticated"),
        true,
        TimestampMs::new(1),
    );
    assert!(matches!(
        fixture
            .application
            .mutate(&unauthenticated, mutation())
            .await,
        Err(AutomationError::AuthorizationDenied(_))
    ));
    let unconfirmed = RequestContext::new(
        "unconfirmed",
        Actor::new("commissioner").with_permission("automation.instance.manage"),
        false,
        TimestampMs::new(2),
    );
    assert!(matches!(
        fixture.application.mutate(&unconfirmed, mutation()).await,
        Err(AutomationError::InvalidData(_))
    ));
    assert_eq!(instance_count(&fixture.pool).await, 0);
    assert_eq!(instances_revision(&fixture.pool).await, 1);

    let outcomes: Vec<String> =
        sqlx::query_scalar("SELECT outcome FROM command_audit_events ORDER BY id")
            .fetch_all(&fixture.pool)
            .await
            .unwrap();
    assert_eq!(outcomes, ["rejected", "rejected"]);

    let failing =
        InstanceConfigurationApplication::new(Arc::clone(&fixture.manager), Arc::new(FailingAudit));
    assert!(matches!(
        failing
            .mutate(&confirmed_context("audit-down"), mutation())
            .await,
        Err(AutomationError::AuditUnavailable(_))
    ));
    assert_eq!(instance_count(&fixture.pool).await, 0);
    assert_eq!(instances_revision(&fixture.pool).await, 1);
}

#[tokio::test]
async fn http_create_requires_identity_confirmation_and_explicit_revision() {
    let fixture = Fixture::new().await;
    let router = fixture.router();
    let revision = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/instances/revision")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(revision.status(), StatusCode::OK);
    let revision: serde_json::Value =
        serde_json::from_slice(&revision.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(revision["data"]["scope"], "instances");
    assert_eq!(revision["data"]["revision"], 1);
    let body = |confirmed: bool, expected_revision: Option<u64>| {
        let mut body = json!({
            "instance_id": 7,
            "instance_name": "http-created",
            "product_name": "Fixture",
            "properties": {"serial": "http"},
            "confirmed": confirmed
        });
        if let Some(revision) = expected_revision {
            body["expected_revision"] = json!(revision);
        }
        body
    };

    let (missing_revision, _) = http_create(&router, body(true, None), Some(&access_token())).await;
    assert_eq!(missing_revision, StatusCode::UNPROCESSABLE_ENTITY);
    let (unauthenticated, _) = http_create(&router, body(true, Some(1)), None).await;
    assert_eq!(unauthenticated, StatusCode::FORBIDDEN);
    let (unconfirmed, _) = http_create(&router, body(false, Some(1)), Some(&access_token())).await;
    assert_eq!(unconfirmed, StatusCode::UNPROCESSABLE_ENTITY);

    let (created, response) =
        http_create(&router, body(true, Some(1)), Some(&access_token())).await;
    assert_eq!(created, StatusCode::OK);
    assert_eq!(response["data"]["governance"]["resulting_revision"], 2);
    assert_eq!(
        response["data"]["governance"]["audit"]["status"],
        "recorded"
    );
    assert_eq!(
        response["data"]["governance"]["runtime"]["status"],
        "refreshed"
    );

    let (stale, _) = http_create(
        &router,
        json!({
            "instance_id": 8,
            "instance_name": "stale-http",
            "product_name": "Fixture",
            "expected_revision": 1,
            "confirmed": true
        }),
        Some(&access_token()),
    )
    .await;
    assert_eq!(stale, StatusCode::CONFLICT);
    assert_eq!(instance_count(&fixture.pool).await, 1);
}

async fn http_create(
    router: &axum::Router,
    body: serde_json::Value,
    token: Option<&str>,
) -> (StatusCode, serde_json::Value) {
    let mut request = Request::builder()
        .method("POST")
        .uri("/api/instances")
        .header("content-type", "application/json");
    if let Some(token) = token {
        request = request.header("authorization", format!("Bearer {token}"));
    }
    let response = router
        .clone()
        .oneshot(
            request
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let body = serde_json::from_slice(&bytes).unwrap_or_else(|_| json!({}));
    (status, body)
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
    encode(
        &Header::new(Algorithm::HS256),
        &AccessClaims {
            user_id: 7,
            role: "Admin",
            exp: 4_102_444_800,
            iat: 1,
            token_type: "access",
        },
        &EncodingKey::from_secret(JWT_SECRET.as_bytes()),
    )
    .unwrap()
}

struct FailingAudit;

#[async_trait]
impl AuditSink for FailingAudit {
    async fn record(&self, _record: aether_ports::AuditRecord) -> PortResult<()> {
        Err(PortError::new(
            PortErrorKind::Unavailable,
            "injected audit failure",
        ))
    }
}

async fn instance_count(pool: &SqlitePool) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM instances")
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn instances_revision(pool: &SqlitePool) -> i64 {
    sqlx::query_scalar("SELECT revision FROM configuration_revisions WHERE scope = 'instances'")
        .fetch_one(pool)
        .await
        .unwrap()
}
