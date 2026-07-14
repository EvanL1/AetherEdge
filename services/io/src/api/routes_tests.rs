// NOTE: API tests use a real temporary mmap so the test topology matches the
// production SHM-only data plane.
#![allow(clippy::disallowed_methods)] // Test code - unwrap is acceptable

use super::*;
use crate::dto::{AdjustmentRequest, ControlRequest};
use axum::{
    Extension,
    body::Body,
    http::{Request, Response, StatusCode},
};
use serde_json::json;
use sqlx::SqlitePool;
use std::collections::{BTreeMap, HashMap};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};

#[cfg(feature = "modbus")]
use crate::api::handlers::provision_handlers::{
    SunSpecDiscoveryBoundary, SunSpecDiscoveryPort, provision_channel_handler,
};
#[cfg(feature = "modbus")]
use crate::protocols::adapters::modbus_config::ModbusChannelParamsConfig;

use aether_model::PointType;
use aether_ports::{
    AuditOutcome, AuditRecord, AuditSink, ChannelDesiredStateObservation, ChannelMutation,
    ChannelMutationKind, ChannelMutationReceipt, ChannelMutator, ChannelReconciler,
    ChannelReconciliationItem, ChannelReconciliationReceipt, ChannelReconciliationScope,
    ChannelRevision, ChannelRuntimeProjection, PortError, PortErrorKind, PortResult,
};
use aether_shm_bridge::ShmWriterHandle;
use tower::util::ServiceExt; // for `oneshot` and `ready`

const TEST_JWT_SECRET: &str = "0123456789abcdef0123456789abcdef";
const ADMIN_ACCESS_TOKEN: &str = "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJ1c2VyX2lkIjo3LCJyb2xlIjoiQWRtaW4iLCJ0eXBlIjoiYWNjZXNzIiwiaWF0IjoxNzAwMDAwMDAwLCJleHAiOjQxMDI0NDQ4MDB9.JtjQvDBo7j0bLOxwed6yC9-M9qFCloc4H2Dt0LjzF9E";
const TEST_REQUEST_ID: &str = "018f0000-0000-7000-8000-000000000041";

#[cfg(feature = "modbus")]
#[derive(Default)]
struct RecordingSunSpecDiscovery {
    connect_calls: AtomicUsize,
    read_calls: AtomicUsize,
}

#[cfg(feature = "modbus")]
impl RecordingSunSpecDiscovery {
    fn connect_calls(&self) -> usize {
        self.connect_calls.load(Ordering::SeqCst)
    }

    fn read_calls(&self) -> usize {
        self.read_calls.load(Ordering::SeqCst)
    }
}

#[cfg(feature = "modbus")]
#[async_trait::async_trait]
impl SunSpecDiscoveryPort for RecordingSunSpecDiscovery {
    async fn connect_and_discover(
        &self,
        _params: &ModbusChannelParamsConfig,
        _protocol: &str,
        _slave_id: u8,
        _function_code: u8,
        _base_address: Option<u16>,
    ) -> Result<(u16, Vec<aether_model::sunspec::DiscoveredModel>), String> {
        self.connect_calls.fetch_add(1, Ordering::SeqCst);
        self.read_calls.fetch_add(1, Ordering::SeqCst);
        Ok((
            40_000,
            vec![aether_model::sunspec::DiscoveredModel {
                model_id: 103,
                length: 50,
                start_register: 40_002,
            }],
        ))
    }
}

/// Helper: Create in-memory SQLite pool for testing
async fn create_test_sqlite_pool() -> sqlx::SqlitePool {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();

    // Use standard io schema from common test utils
    common::test_utils::schema::init_io_schema(&pool)
        .await
        .unwrap();

    pool
}

/// Helper: Create in-memory SQLite pool with point tables (including protocol_mappings)
async fn create_test_sqlite_pool_with_points() -> sqlx::SqlitePool {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();

    // Use standard io schema from common test utils
    common::test_utils::schema::init_io_schema(&pool)
        .await
        .unwrap();

    pool
}

/// Helper: Create API routes over authoritative SHM for testing.
async fn create_test_api_routes(channel_manager: Arc<ChannelManager>) -> Router {
    let sqlite_pool = create_test_sqlite_pool().await;
    create_test_api_with_pool(channel_manager, sqlite_pool).await
}

/// Helper: Build a Router using a provided in-memory SQLite pool
async fn create_test_api_with_pool(
    channel_manager: Arc<ChannelManager>,
    sqlite_pool: SqlitePool,
) -> Router {
    // Channel deletion owns cross-service routing rows in the unified
    // edge database. Mirror the complete production topology so HTTP tests do
    // not exercise the governed adapter against a partial schema.
    common::test_utils::schema::init_automation_schema(&sqlite_pool)
        .await
        .unwrap();
    let command_tx_cache = Arc::new(crate::api::command_cache::CommandTxCache::new());
    let adapter = Arc::new(crate::SqliteChannelMutator::new(
        sqlite_pool.clone(),
        Arc::clone(&channel_manager),
    ));
    let mutator: Arc<dyn ChannelMutator> = adapter.clone();
    let reconciler: Arc<dyn ChannelReconciler> = adapter;
    let audit: Arc<dyn AuditSink> = Arc::new(aether_store_local::MemoryAuditSink::new());
    let application = Arc::new(aether_application::ChannelManagementApplication::new(
        mutator,
        Arc::clone(&audit),
        aether_application::SafetyPolicy,
    ));
    let reconciliation = Arc::new(aether_application::ChannelReconciliationApplication::new(
        reconciler,
        Arc::clone(&audit),
        aether_application::SafetyPolicy,
    ));
    let point_topology = Arc::new(crate::point_topology::PointTopologyApplication::new(
        sqlite_pool.clone(),
        audit,
    ));
    let authenticator = Arc::new(
        aether_auth_jwt::AccessTokenAuthenticator::new(TEST_JWT_SECRET)
            .expect("valid test access-token secret"),
    );
    create_api_routes_with_boundary(
        channel_manager,
        sqlite_pool,
        command_tx_cache,
        false,
        Some(Arc::clone(&reconciliation)),
        ChannelManagementHttpBoundary::governed_with_reconciliation(
            application,
            reconciliation,
            Arc::clone(&authenticator),
        ),
        PointTopologyHttpBoundary::governed(point_topology, authenticator),
    )
}

#[cfg(feature = "modbus")]
async fn create_provision_test_api(
    sqlite_pool: SqlitePool,
    discovery: Arc<RecordingSunSpecDiscovery>,
) -> Router {
    let channel_manager = Arc::new(
        ChannelManager::new(
            crate::test_utils::create_test_shm_handle(),
            crate::test_utils::create_test_routing_cache(),
        )
        .expect("provision test channel manager"),
    );
    let audit: Arc<dyn AuditSink> = Arc::new(aether_store_local::MemoryAuditSink::new());
    let point_topology = Arc::new(crate::point_topology::PointTopologyApplication::new(
        sqlite_pool.clone(),
        audit,
    ));
    let authenticator = Arc::new(
        aether_auth_jwt::AccessTokenAuthenticator::new(TEST_JWT_SECRET)
            .expect("valid test access-token secret"),
    );
    let state = AppState::new(
        channel_manager,
        sqlite_pool,
        Arc::new(crate::api::command_cache::CommandTxCache::new()),
        false,
        None,
    );
    Router::new()
        .route(
            "/api/channels/{channel_id}/provision",
            axum::routing::post(provision_channel_handler),
        )
        .layer(Extension(PointTopologyHttpBoundary::governed(
            point_topology,
            authenticator,
        )))
        .layer(Extension(SunSpecDiscoveryBoundary::from_port(discovery)))
        .with_state(state)
}

#[cfg(feature = "modbus")]
fn provision_request(
    authorization: bool,
    confirmed: bool,
    expected_revision: Option<&str>,
) -> Request<Body> {
    let mut request = Request::builder()
        .method("POST")
        .uri("/api/channels/711/provision")
        .header("content-type", "application/json")
        .header("x-request-id", TEST_REQUEST_ID);
    if authorization {
        request = request.header("authorization", format!("Bearer {ADMIN_ACCESS_TOKEN}"));
    }
    if confirmed {
        request = request.header("x-aether-confirmed", "true");
    }
    if let Some(expected_revision) = expected_revision {
        request = request.header("x-aether-expected-revision", expected_revision);
    }
    request
        .body(Body::from(
            json!({
                "strategy": "sunspec",
                "slave_id": 1,
                "function_code": 3,
                "replace_existing": true
            })
            .to_string(),
        ))
        .expect("provision request")
}

#[cfg(feature = "modbus")]
#[tokio::test]
async fn provision_authorization_precedes_device_io_and_stale_cas_fails_closed() {
    let pool = create_test_sqlite_pool_with_points().await;
    sqlx::query(
        "INSERT INTO channels (channel_id, name, protocol, enabled, config) \
         VALUES (711, 'SunSpec device', 'sunspec_tcp', 0, \
                 '{\"parameters\":{\"host\":\"127.0.0.1\",\"port\":502}}')",
    )
    .execute(&pool)
    .await
    .expect("seed SunSpec channel");
    let discovery = Arc::new(RecordingSunSpecDiscovery::default());
    let app = create_provision_test_api(pool, Arc::clone(&discovery)).await;

    let rejected = [
        (
            provision_request(false, true, Some("1")),
            StatusCode::FORBIDDEN,
        ),
        (
            provision_request(true, false, Some("1")),
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
        (provision_request(true, true, None), StatusCode::BAD_REQUEST),
        (
            provision_request(true, true, Some("not-a-revision")),
            StatusCode::BAD_REQUEST,
        ),
        (
            provision_request(true, true, Some("0")),
            StatusCode::BAD_REQUEST,
        ),
    ];
    for (request, expected_status) in rejected {
        let response = app.clone().oneshot(request).await.expect("HTTP response");
        assert_eq!(response.status(), expected_status);
    }
    assert_eq!(discovery.connect_calls(), 0);
    assert_eq!(discovery.read_calls(), 0);

    let response = app
        .oneshot(provision_request(true, true, Some("2")))
        .await
        .expect("stale provision response");
    let status = response.status();
    let body = http_body_util::BodyExt::collect(response.into_body())
        .await
        .expect("stale provision body")
        .to_bytes();
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "unexpected response: {}",
        String::from_utf8_lossy(&body)
    );
    assert_eq!(discovery.connect_calls(), 1);
    assert_eq!(discovery.read_calls(), 1);
}

fn channel_mutation_request(
    method: &str,
    uri: &str,
    body: Option<serde_json::Value>,
) -> Request<Body> {
    let builder = Request::builder()
        .uri(uri)
        .method(method)
        .header("authorization", format!("Bearer {ADMIN_ACCESS_TOKEN}"))
        .header("x-request-id", TEST_REQUEST_ID)
        .header("x-aether-confirmed", "true");
    match body {
        Some(body) => builder
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .expect("channel mutation request"),
        None => builder
            .body(Body::empty())
            .expect("channel mutation request"),
    }
}

struct RecordingChannelMutator {
    mutations: Mutex<Vec<ChannelMutation>>,
    error: Option<PortError>,
    projection: Option<ChannelRuntimeProjection>,
}

impl RecordingChannelMutator {
    fn successful(projection: Option<ChannelRuntimeProjection>) -> Arc<Self> {
        Arc::new(Self {
            mutations: Mutex::new(Vec::new()),
            error: None,
            projection,
        })
    }

    fn failing(kind: PortErrorKind) -> Arc<Self> {
        Arc::new(Self {
            mutations: Mutex::new(Vec::new()),
            error: Some(PortError::new(kind, format!("{kind:?} test failure"))),
            projection: None,
        })
    }

    fn mutation_count(&self) -> usize {
        self.mutations.lock().unwrap().len()
    }

    fn mutations(&self) -> Vec<ChannelMutation> {
        self.mutations.lock().unwrap().clone()
    }
}

#[async_trait::async_trait]
impl ChannelMutator for RecordingChannelMutator {
    async fn mutate(&self, mutation: ChannelMutation) -> PortResult<ChannelMutationReceipt> {
        self.mutations.lock().unwrap().push(mutation.clone());
        if let Some(error) = &self.error {
            return Err(error.clone());
        }

        let channel_id = mutation
            .channel_id()
            .unwrap_or(aether_domain::ChannelId::new(41));
        let resulting_revision = mutation
            .expected_revision()
            .and_then(ChannelRevision::checked_next)
            .unwrap_or(ChannelRevision::new(1));
        let desired_enabled = match &mutation {
            ChannelMutation::Create { definition } => definition.enabled(),
            ChannelMutation::SetEnabled { enabled, .. } => *enabled,
            ChannelMutation::Update { .. } => false,
            ChannelMutation::Delete { .. } => false,
        };
        let projection = self.projection.unwrap_or(match mutation.kind() {
            ChannelMutationKind::Delete => ChannelRuntimeProjection::Removed,
            ChannelMutationKind::Enable => ChannelRuntimeProjection::Active,
            ChannelMutationKind::Create
            | ChannelMutationKind::Update
            | ChannelMutationKind::Disable => ChannelRuntimeProjection::Stopped,
        });
        Ok(ChannelMutationReceipt::new(
            channel_id,
            mutation.kind(),
            resulting_revision,
            desired_enabled,
            projection,
        ))
    }
}

struct RecordingChannelReconciler {
    scopes: Mutex<Vec<ChannelReconciliationScope>>,
    items: Vec<ChannelReconciliationItem>,
    error: Option<PortError>,
}

impl RecordingChannelReconciler {
    fn successful(items: Vec<ChannelReconciliationItem>) -> Arc<Self> {
        Arc::new(Self {
            scopes: Mutex::new(Vec::new()),
            items,
            error: None,
        })
    }

    fn failing(kind: PortErrorKind) -> Arc<Self> {
        Arc::new(Self {
            scopes: Mutex::new(Vec::new()),
            items: Vec::new(),
            error: Some(PortError::new(
                kind,
                "sensitive protocol credential must not cross HTTP",
            )),
        })
    }

    fn scopes(&self) -> Vec<ChannelReconciliationScope> {
        self.scopes.lock().unwrap().clone()
    }
}

#[async_trait::async_trait]
impl ChannelReconciler for RecordingChannelReconciler {
    async fn reconcile(
        &self,
        scope: ChannelReconciliationScope,
    ) -> PortResult<ChannelReconciliationReceipt> {
        self.scopes.lock().unwrap().push(scope);
        if let Some(error) = &self.error {
            return Err(error.clone());
        }
        let items = match scope {
            ChannelReconciliationScope::All => self.items.clone(),
            ChannelReconciliationScope::One(channel_id) => self
                .items
                .iter()
                .copied()
                .filter(|item| item.channel_id() == channel_id)
                .collect(),
        };
        Ok(ChannelReconciliationReceipt::new(scope, items))
    }
}

struct TerminalAuditFailure;

#[async_trait::async_trait]
impl AuditSink for TerminalAuditFailure {
    async fn record(&self, record: AuditRecord) -> PortResult<()> {
        if record.outcome() == AuditOutcome::Succeeded {
            Err(PortError::new(
                PortErrorKind::Unavailable,
                "terminal audit unavailable",
            ))
        } else {
            Ok(())
        }
    }
}

struct UnavailableAuditSink;

#[async_trait::async_trait]
impl AuditSink for UnavailableAuditSink {
    async fn record(&self, _record: AuditRecord) -> PortResult<()> {
        Err(PortError::new(
            PortErrorKind::Unavailable,
            "sensitive audit backend detail",
        ))
    }
}

async fn recording_channel_router(mutator: Arc<RecordingChannelMutator>) -> Router {
    recording_channel_router_with_audit(
        mutator,
        Arc::new(aether_store_local::MemoryAuditSink::new()),
    )
    .await
}

async fn recording_channel_router_with_audit(
    mutator: Arc<RecordingChannelMutator>,
    audit: Arc<dyn AuditSink>,
) -> Router {
    let pool = create_test_sqlite_pool().await;
    let channel_manager = Arc::new(
        ChannelManager::new(
            crate::test_utils::create_test_shm_handle(),
            crate::test_utils::create_test_routing_cache(),
        )
        .unwrap(),
    );
    let application = Arc::new(aether_application::ChannelManagementApplication::new(
        mutator,
        audit,
        aether_application::SafetyPolicy,
    ));
    let authenticator = Arc::new(
        aether_auth_jwt::AccessTokenAuthenticator::new(TEST_JWT_SECRET)
            .expect("valid test access-token secret"),
    );
    create_api_routes_with_boundary(
        channel_manager,
        pool,
        Arc::new(crate::api::command_cache::CommandTxCache::new()),
        false,
        None,
        ChannelManagementHttpBoundary::governed(application, authenticator),
        PointTopologyHttpBoundary::unavailable(),
    )
}

async fn recording_reconciliation_router(
    reconciler: Arc<RecordingChannelReconciler>,
    audit: Arc<dyn AuditSink>,
) -> Router {
    recording_channel_applications_router(
        RecordingChannelMutator::successful(None),
        reconciler,
        audit,
    )
    .await
}

async fn recording_channel_applications_router(
    mutator: Arc<RecordingChannelMutator>,
    reconciler: Arc<RecordingChannelReconciler>,
    audit: Arc<dyn AuditSink>,
) -> Router {
    let pool = create_test_sqlite_pool().await;
    let channel_manager = Arc::new(
        ChannelManager::new(
            crate::test_utils::create_test_shm_handle(),
            crate::test_utils::create_test_routing_cache(),
        )
        .unwrap(),
    );
    let channel_management = Arc::new(aether_application::ChannelManagementApplication::new(
        mutator,
        Arc::clone(&audit),
        aether_application::SafetyPolicy,
    ));
    let channel_reconciliation =
        Arc::new(aether_application::ChannelReconciliationApplication::new(
            reconciler,
            audit,
            aether_application::SafetyPolicy,
        ));
    let authenticator = Arc::new(
        aether_auth_jwt::AccessTokenAuthenticator::new(TEST_JWT_SECRET)
            .expect("valid test access-token secret"),
    );
    create_api_routes_with_boundary(
        channel_manager,
        pool,
        Arc::new(crate::api::command_cache::CommandTxCache::new()),
        false,
        Some(Arc::clone(&channel_reconciliation)),
        ChannelManagementHttpBoundary::governed_with_reconciliation(
            channel_management,
            channel_reconciliation,
            authenticator,
        ),
        PointTopologyHttpBoundary::unavailable(),
    )
}

#[tokio::test]
async fn channel_mutations_require_real_bearer_auth_and_confirmation_before_side_effects() {
    let mutator = RecordingChannelMutator::successful(None);
    let app = recording_channel_router(Arc::clone(&mutator)).await;
    let body = json!({
        "channel_id": 41,
        "name": "governed channel",
        "protocol": "virtual",
        "parameters": {}
    });

    let unauthenticated = Request::builder()
        .uri("/api/channels")
        .method("POST")
        .header("content-type", "application/json")
        .header("x-aether-confirmed", "true")
        .body(Body::from(body.to_string()))
        .unwrap();
    let response = app.clone().oneshot(unauthenticated).await.unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert_eq!(mutator.mutation_count(), 0);

    let unconfirmed = Request::builder()
        .uri("/api/channels")
        .method("POST")
        .header("content-type", "application/json")
        .header("authorization", format!("Bearer {ADMIN_ACCESS_TOKEN}"))
        .body(Body::from(body.to_string()))
        .unwrap();
    let response = app.oneshot(unconfirmed).await.unwrap();
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(mutator.mutation_count(), 0);
}

#[tokio::test]
async fn channel_create_defaults_disabled_and_returns_the_typed_receipt() {
    let mutator = RecordingChannelMutator::successful(None);
    let app = recording_channel_router(Arc::clone(&mutator)).await;
    let request = channel_mutation_request(
        "POST",
        "/api/channels",
        Some(json!({
            "channel_id": 41,
            "name": "safe commissioning",
            "description": "disabled until explicitly enabled",
            "protocol": "virtual",
            "parameters": {}
        })),
    );

    let response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let payload = extract_json(response).await;
    assert_eq!(payload["data"]["id"], 41);
    assert_eq!(payload["data"]["channel_id"], 41);
    assert_eq!(payload["data"]["name"], "safe commissioning");
    assert_eq!(payload["data"]["protocol"], "virtual");
    assert_eq!(payload["data"]["operation"], "create");
    assert_eq!(payload["data"]["resulting_revision"], 1);
    assert_eq!(payload["data"]["desired_enabled"], false);
    assert_eq!(payload["data"]["runtime_projection"], "stopped");
    assert_eq!(payload["data"]["runtime_status"], "stopped");
    assert_eq!(payload["data"]["reconciliation_required"], false);
    assert_eq!(payload["data"]["completion_audit"]["status"], "recorded");
    assert_eq!(payload["data"]["retryable"], false);
    assert_eq!(payload["data"]["request_id"], TEST_REQUEST_ID);

    let mutations = mutator.mutations();
    let ChannelMutation::Create { definition } = &mutations[0] else {
        panic!("expected create mutation");
    };
    assert!(!definition.enabled());
}

#[tokio::test]
async fn channel_revision_header_is_forwarded_as_compare_and_set() {
    let mutator = RecordingChannelMutator::successful(None);
    let app = recording_channel_router(Arc::clone(&mutator)).await;
    let mut request = channel_mutation_request(
        "PUT",
        "/api/channels/41",
        Some(json!({"name": "revision guarded"})),
    );
    request.headers_mut().insert(
        "x-aether-expected-revision",
        axum::http::HeaderValue::from_static("7"),
    );

    let response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let payload = extract_json(response).await;
    assert_eq!(payload["data"]["resulting_revision"], 8);
    assert_eq!(payload["data"]["request_id"], TEST_REQUEST_ID);
    assert_eq!(
        mutator.mutations()[0].expected_revision(),
        Some(ChannelRevision::new(7))
    );
}

#[tokio::test]
async fn ordinary_update_rejects_channel_id_migration_without_mutating() {
    let mutator = RecordingChannelMutator::successful(None);
    let app = recording_channel_router(Arc::clone(&mutator)).await;
    let request = channel_mutation_request(
        "PUT",
        "/api/channels/41",
        Some(json!({"channel_id": 42, "name": "must not migrate"})),
    );

    let response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(mutator.mutation_count(), 0);
}

#[tokio::test]
async fn channel_port_error_kinds_have_stable_http_mappings() {
    for (kind, status) in [
        (PortErrorKind::InvalidData, StatusCode::BAD_REQUEST),
        (PortErrorKind::NotFound, StatusCode::NOT_FOUND),
        (PortErrorKind::Rejected, StatusCode::CONFLICT),
        (PortErrorKind::Conflict, StatusCode::CONFLICT),
        (PortErrorKind::Unavailable, StatusCode::SERVICE_UNAVAILABLE),
        (PortErrorKind::Timeout, StatusCode::GATEWAY_TIMEOUT),
        (PortErrorKind::Permanent, StatusCode::INTERNAL_SERVER_ERROR),
    ] {
        let mutator = RecordingChannelMutator::failing(kind);
        let app = recording_channel_router(Arc::clone(&mutator)).await;
        let request = channel_mutation_request(
            "PUT",
            "/api/channels/41",
            Some(json!({"name": "typed error mapping"})),
        );

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), status, "unexpected mapping for {kind:?}");
        assert_eq!(mutator.mutation_count(), 1);
    }
}

#[tokio::test]
async fn delete_conflicts_when_an_action_route_still_references_the_channel() {
    let mutator = Arc::new(RecordingChannelMutator {
        mutations: Mutex::new(Vec::new()),
        error: Some(PortError::new(
            PortErrorKind::Conflict,
            "action route still references channel 41",
        )),
        projection: None,
    });
    let app = recording_channel_router(Arc::clone(&mutator)).await;
    let request = channel_mutation_request("DELETE", "/api/channels/41", None);

    let response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::CONFLICT);
    let payload = extract_json(response).await;
    assert_eq!(
        payload["error"]["message"],
        "Channel mutation conflicts with current desired state"
    );
}

#[tokio::test]
async fn degraded_runtime_projection_is_an_accepted_non_retryable_outcome() {
    let mutator = RecordingChannelMutator::successful(Some(ChannelRuntimeProjection::Degraded));
    let app = recording_channel_router(mutator).await;
    let request = channel_mutation_request(
        "POST",
        "/api/channels",
        Some(json!({
            "channel_id": 41,
            "name": "degraded projection",
            "protocol": "virtual",
            "enabled": true,
            "parameters": {}
        })),
    );

    let response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let payload = extract_json(response).await;
    assert_eq!(payload["data"]["runtime_projection"], "degraded");
    assert_eq!(payload["data"]["runtime_status"], "degraded");
    assert_eq!(payload["data"]["reconciliation_required"], true);
    assert_eq!(payload["data"]["retryable"], false);
}

#[tokio::test]
async fn terminal_audit_failure_stays_accepted_and_is_never_retryable() {
    let mutator = RecordingChannelMutator::successful(None);
    let app =
        recording_channel_router_with_audit(Arc::clone(&mutator), Arc::new(TerminalAuditFailure))
            .await;
    let request = channel_mutation_request(
        "POST",
        "/api/channels",
        Some(json!({
            "channel_id": 41,
            "name": "audit reconciliation",
            "protocol": "virtual",
            "parameters": {}
        })),
    );

    let response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let payload = extract_json(response).await;
    assert_eq!(payload["data"]["completion_audit"]["status"], "incomplete");
    assert_eq!(payload["data"]["completion_audit"]["retryable"], false);
    assert_eq!(payload["data"]["retryable"], false);
    assert_eq!(mutator.mutation_count(), 1);
}

// The write-environment helper is inlined into
// `setup_write_test_env` so the latter can register a stub command
// sender on `command_tx_cache` before constructing the router. There
// are no other callers, so the helper was removed.

// ========================================================================
// Closed-loop Testing Utilities
// ========================================================================

/// Extract JSON response body from axum Response
async fn extract_json(resp: axum::response::Response) -> serde_json::Value {
    use http_body_util::BodyExt;
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).expect("Response body should be valid JSON")
}

/// Assert that a JSON field at the given JSON pointer path equals the expected value
///
/// # Arguments
/// * `json` - The JSON value to inspect
/// * `path` - JSON pointer path (e.g., "/data/channel_id", "/data/name")
/// * `expected` - The expected value at that path
///
/// # Panics
/// Panics if the field doesn't exist or doesn't match the expected value
fn assert_json_field(json: &serde_json::Value, path: &str, expected: serde_json::Value) {
    let actual = json
        .pointer(path)
        .unwrap_or_else(|| panic!("Field '{}' not found in JSON: {:?}", path, json));
    assert_eq!(
        actual, &expected,
        "Field '{}' mismatch: expected {:?}, got {:?}",
        path, expected, actual
    );
}

// ========================================================================
// Phase 1: Service Status Endpoint Tests
// ========================================================================

#[tokio::test]
async fn test_get_service_status_returns_200() {
    let channel_manager = Arc::new(
        ChannelManager::new(
            crate::test_utils::create_test_shm_handle(),
            crate::test_utils::create_test_routing_cache(),
        )
        .unwrap(),
    );
    let app = create_test_api_routes(channel_manager).await;

    let request = Request::builder()
        .uri("/api/status")
        .body(Body::empty())
        .unwrap();

    let response = app.oneshot(request).await.unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let payload = extract_json(response).await;
    assert_json_field(
        &payload,
        "/data/name",
        serde_json::Value::String("Aether I/O Service".to_string()),
    );
}

#[tokio::test]
async fn test_health_check_returns_200_with_initialized_shm() {
    let channel_manager = Arc::new(
        ChannelManager::new(
            crate::test_utils::create_test_shm_handle(),
            crate::test_utils::create_test_routing_cache(),
        )
        .unwrap(),
    );
    let app = create_test_api_routes(channel_manager).await;

    let request = Request::builder()
        .uri("/health")
        .body(Body::empty())
        .unwrap();

    let response = app.oneshot(request).await.unwrap();

    assert_eq!(response.status(), StatusCode::OK);
}

// ========================================================================
// Phase 2: Channel Query Endpoint Tests
// ========================================================================

#[tokio::test]
async fn test_get_all_channels_returns_200() {
    let channel_manager = Arc::new(
        ChannelManager::new(
            crate::test_utils::create_test_shm_handle(),
            crate::test_utils::create_test_routing_cache(),
        )
        .unwrap(),
    );
    let app = create_test_api_routes(channel_manager).await;

    let request = Request::builder()
        .uri("/api/channels")
        .body(Body::empty())
        .unwrap();

    let response = app.oneshot(request).await.unwrap();

    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn test_get_all_channels_with_filters() {
    // Seed channels table with two channels of different protocols
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();

    // Use standard io schema from common test utils
    common::test_utils::schema::init_io_schema(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO channels (channel_id, name, protocol, enabled, config) VALUES (100, 'Ch100', 'virtual', 1, '{}')")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO channels (channel_id, name, protocol, enabled, config) VALUES (101, 'Ch101', 'modbus_tcp', 0, '{}')")
        .execute(&pool)
        .await
        .unwrap();

    // Build the protocol factory without external infrastructure.
    let channel_manager = Arc::new(
        ChannelManager::new(
            crate::test_utils::create_test_shm_handle(),
            crate::test_utils::create_test_routing_cache(),
        )
        .unwrap(),
    );
    let app = create_test_api_with_pool(channel_manager, pool).await;

    // Protocol filter
    let req1 = Request::builder()
        .uri("/api/channels?protocol=virtual")
        .body(Body::empty())
        .unwrap();
    let resp1 = app.clone().oneshot(req1).await.unwrap();
    assert_eq!(resp1.status(), StatusCode::OK);
    let payload = extract_json(resp1).await;
    assert_eq!(payload["data"]["list"][0]["revision"], 1);

    // Enabled filter
    let req2 = Request::builder()
        .uri("/api/channels?enabled=false")
        .body(Body::empty())
        .unwrap();
    let resp2 = app.clone().oneshot(req2).await.unwrap();
    assert_eq!(resp2.status(), StatusCode::OK);

    // Pagination
    let req3 = Request::builder()
        .uri("/api/channels?page=1&page_size=1")
        .body(Body::empty())
        .unwrap();
    let resp3 = app.oneshot(req3).await.unwrap();
    assert_eq!(resp3.status(), StatusCode::OK);
}

#[tokio::test]
async fn test_get_channel_status_invalid_id_returns_400() {
    let channel_manager = Arc::new(
        ChannelManager::new(
            crate::test_utils::create_test_shm_handle(),
            crate::test_utils::create_test_routing_cache(),
        )
        .unwrap(),
    );
    let app = create_test_api_routes(channel_manager).await;

    let request = Request::builder()
        .uri("/api/channels/invalid/status")
        .body(Body::empty())
        .unwrap();

    let response = app.oneshot(request).await.unwrap();

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn test_get_channel_status_not_found_returns_404() {
    let channel_manager = Arc::new(
        ChannelManager::new(
            crate::test_utils::create_test_shm_handle(),
            crate::test_utils::create_test_routing_cache(),
        )
        .unwrap(),
    );
    let app = create_test_api_routes(channel_manager).await;

    let request = Request::builder()
        .uri("/api/channels/9999/status")
        .body(Body::empty())
        .unwrap();

    let response = app.oneshot(request).await.unwrap();

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn test_get_point_info_handler_returns_200() {
    let channel_manager = Arc::new(
        ChannelManager::new(
            crate::test_utils::create_test_shm_handle(),
            crate::test_utils::create_test_routing_cache(),
        )
        .unwrap(),
    );
    let app = create_test_api_routes(channel_manager).await;

    let request = Request::builder()
        .uri("/api/channels/1/T/1")
        .body(Body::empty())
        .unwrap();

    let response = app.oneshot(request).await.unwrap();

    assert_eq!(response.status(), StatusCode::OK);
}

// ========================================================================
// Phase X: CRUD regression tests (description propagation)
// ========================================================================

#[tokio::test]
async fn test_create_channel_returns_description() {
    let channel_manager = Arc::new(
        ChannelManager::new(
            crate::test_utils::create_test_shm_handle(),
            crate::test_utils::create_test_routing_cache(),
        )
        .unwrap(),
    );

    // Use simple in-memory DB (channels table only)
    let sqlite_pool = create_test_sqlite_pool().await;
    let app = create_test_api_with_pool(channel_manager, sqlite_pool).await;

    let body = serde_json::json!({
        "name": "Virtual Channel A",
        "description": "desc-A",
        "protocol": "virtual",
        "enabled": true,
        "parameters": {}
    });

    let req = channel_mutation_request("POST", "/api/channels", Some(body));

    use http_body_util::BodyExt as _;
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(v["success"], true);
    assert_eq!(v["data"]["operation"], "create");
    assert_eq!(v["data"]["desired_enabled"], true);
    assert_eq!(v["data"]["retryable"], false);
    assert_eq!(v["data"]["name"], "Virtual Channel A");
    assert_eq!(v["data"]["description"], "desc-A");
    assert_eq!(v["data"]["protocol"], "virtual");
}

#[tokio::test]
async fn test_update_channel_returns_description() {
    let channel_manager = Arc::new(
        ChannelManager::new(
            crate::test_utils::create_test_shm_handle(),
            crate::test_utils::create_test_routing_cache(),
        )
        .unwrap(),
    );
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();

    // Use standard io schema from common test utils
    common::test_utils::schema::init_io_schema(&pool)
        .await
        .unwrap();

    let config = serde_json::json!({"description": "old-desc", "host": "127.0.0.1"}).to_string();
    sqlx::query("INSERT INTO channels (channel_id, name, protocol, enabled, config) VALUES (42, 'Ch42', 'virtual', 1, ?)")
        .bind(&config)
        .execute(&pool)
        .await
        .unwrap();

    let app = create_test_api_with_pool(channel_manager, pool).await;

    // Update description
    let body = serde_json::json!({
        "description": "new-desc"
    });
    let req = channel_mutation_request("PUT", "/api/channels/42", Some(body));

    use http_body_util::BodyExt as _;
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(v["data"]["operation"], "update");
    assert_eq!(v["data"]["description"], "new-desc");

    // Update without description: should keep last description
    let body2 = serde_json::json!({ "parameters": {"x": 1} });
    let req2 = channel_mutation_request("PUT", "/api/channels/42", Some(body2));
    let resp2 = app.oneshot(req2).await.unwrap();
    assert_eq!(resp2.status(), StatusCode::OK);
    let bytes2 = resp2.into_body().collect().await.unwrap().to_bytes();
    let v2: serde_json::Value = serde_json::from_slice(&bytes2).unwrap();
    assert_eq!(v2["data"]["operation"], "update");
    assert!(v2["data"].get("description").is_none());
}

#[tokio::test]
async fn test_enable_disable_preserves_description() {
    let channel_manager = Arc::new(
        ChannelManager::new(
            crate::test_utils::create_test_shm_handle(),
            crate::test_utils::create_test_routing_cache(),
        )
        .unwrap(),
    );
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();

    // Use standard io schema from common test utils
    common::test_utils::schema::init_io_schema(&pool)
        .await
        .unwrap();
    let config = serde_json::json!({"description": "keep-me"}).to_string();
    sqlx::query("INSERT INTO channels (channel_id, name, protocol, enabled, config) VALUES (77, 'Ch77', 'virtual', 0, ?)")
        .bind(&config)
        .execute(&pool)
        .await
        .unwrap();

    let app = create_test_api_with_pool(channel_manager, pool).await;

    // Enable
    let body = serde_json::json!({"enabled": true});
    let req = channel_mutation_request("PUT", "/api/channels/77/enabled", Some(body));
    use http_body_util::BodyExt as _;
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(v["data"]["desired_enabled"], true);

    // Disable
    let body2 = serde_json::json!({"enabled": false});
    let req2 = channel_mutation_request("PUT", "/api/channels/77/enabled", Some(body2));
    let resp2 = app.oneshot(req2).await.unwrap();
    assert_eq!(resp2.status(), StatusCode::OK);
    let bytes2 = resp2.into_body().collect().await.unwrap().to_bytes();
    let v2: serde_json::Value = serde_json::from_slice(&bytes2).unwrap();
    assert_eq!(v2["data"]["desired_enabled"], false);
}

#[tokio::test]
async fn test_grouped_points_unfiltered_and_filtered() {
    let channel_manager = Arc::new(
        ChannelManager::new(
            crate::test_utils::create_test_shm_handle(),
            crate::test_utils::create_test_routing_cache(),
        )
        .unwrap(),
    );
    let pool = create_test_sqlite_pool_with_points().await;

    // Seed a channel and some points
    sqlx::query("INSERT INTO channels (channel_id, name, protocol, enabled, config) VALUES (9001, 'Ch9001', 'virtual', 1, '{}')")
        .execute(&pool)
        .await
        .unwrap();

    // telemetry: 2 points
    sqlx::query("INSERT INTO telemetry_points (channel_id, point_id, signal_name, scale, offset, unit, reverse, data_type, description, protocol_mappings) VALUES (9001, 1, 'T1', 1.0, 0.0, 'V', 0, 'float32', '', ?)")
        .bind(r#"{"slave_id":1}"#)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO telemetry_points (channel_id, point_id, signal_name, scale, offset, unit, reverse, data_type, description, protocol_mappings) VALUES (9001, 2, 'T2', 1.0, 0.0, 'A', 0, 'float32', '', null)")
        .execute(&pool)
        .await
        .unwrap();

    // signal: 1 point
    sqlx::query("INSERT INTO signal_points (channel_id, point_id, signal_name, unit, reverse, data_type, description, normal_state, protocol_mappings) VALUES (9001, 10, 'S1', '', 0, 'uint16', '', 0, ?)")
        .bind(r#"{"slave_id":1}"#)
        .execute(&pool)
        .await
        .unwrap();

    // control: 1 point
    sqlx::query("INSERT INTO control_points (channel_id, point_id, signal_name, unit, data_type, description, protocol_mappings) VALUES (9001, 20, 'C1', '', 'uint16', '', ?)")
        .bind(r#"{"slave_id":1}"#)
        .execute(&pool)
        .await
        .unwrap();

    // adjustment: 1 point
    sqlx::query("INSERT INTO adjustment_points (channel_id, point_id, signal_name, scale, offset, unit, reverse, data_type, description, protocol_mappings) VALUES (9001, 30, 'A1', 1.0, 0.0, '', 0, 'float32', '', ?)")
        .bind(r#"{"slave_id":1}"#)
        .execute(&pool)
        .await
        .unwrap();

    let app = create_test_api_with_pool(channel_manager, pool).await;

    // Unfiltered
    let req = Request::builder()
        .uri("/api/channels/9001/points")
        .body(Body::empty())
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    use http_body_util::BodyExt as _;
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(v["data"]["telemetry"].as_array().unwrap().len(), 2);
    assert_eq!(v["data"]["signal"].as_array().unwrap().len(), 1);
    assert_eq!(v["data"]["control"].as_array().unwrap().len(), 1);
    assert_eq!(v["data"]["adjustment"].as_array().unwrap().len(), 1);

    // Filter type=S
    let req2 = Request::builder()
        .uri("/api/channels/9001/points?type=S")
        .body(Body::empty())
        .unwrap();
    let resp2 = app.clone().oneshot(req2).await.unwrap();
    assert_eq!(resp2.status(), StatusCode::OK);
    let bytes2 = resp2.into_body().collect().await.unwrap().to_bytes();
    let v2: serde_json::Value = serde_json::from_slice(&bytes2).unwrap();
    assert_eq!(v2["data"]["telemetry"].as_array().unwrap().len(), 0);
    assert_eq!(v2["data"]["signal"].as_array().unwrap().len(), 1);
    assert_eq!(v2["data"]["control"].as_array().unwrap().len(), 0);
    assert_eq!(v2["data"]["adjustment"].as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn test_grouped_mappings_unfiltered() {
    let channel_manager = Arc::new(
        ChannelManager::new(
            crate::test_utils::create_test_shm_handle(),
            crate::test_utils::create_test_routing_cache(),
        )
        .unwrap(),
    );
    let pool = create_test_sqlite_pool_with_points().await;

    // Seed channel and points with protocol_mappings
    sqlx::query("INSERT INTO channels (channel_id, name, protocol, enabled, config) VALUES (9002, 'Ch9002', 'virtual', 1, '{}')")
        .execute(&pool)
        .await
        .unwrap();

    sqlx::query("INSERT INTO telemetry_points (channel_id, point_id, signal_name, scale, offset, unit, reverse, data_type, description, protocol_mappings) VALUES (9002, 1, 'T1', 1.0, 0.0, 'V', 0, 'float32', '', ?)")
        .bind(r#"{"fc":3}"#)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO signal_points (channel_id, point_id, signal_name, unit, reverse, data_type, description, normal_state, protocol_mappings) VALUES (9002, 10, 'S1', '', 0, 'uint16', '', 0, ?)")
        .bind(r#"{"fc":2}"#)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO control_points (channel_id, point_id, signal_name, unit, data_type, description, protocol_mappings) VALUES (9002, 20, 'C1', '', 'uint16', '', ?)")
        .bind(r#"{"fc":5}"#)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO adjustment_points (channel_id, point_id, signal_name, scale, offset, unit, reverse, data_type, description, protocol_mappings) VALUES (9002, 30, 'A1', 1.0, 0.0, '', 0, 'float32', '', ?)")
        .bind(r#"{"fc":16}"#)
        .execute(&pool)
        .await
        .unwrap();

    let app = create_test_api_with_pool(channel_manager, pool).await;
    let req = Request::builder()
        .uri("/api/channels/9002/mappings")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    use http_body_util::BodyExt as _;
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(v["data"]["telemetry"].as_array().unwrap().len(), 1);
    assert_eq!(v["data"]["signal"].as_array().unwrap().len(), 1);
    assert_eq!(v["data"]["control"].as_array().unwrap().len(), 1);
    assert_eq!(v["data"]["adjustment"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn test_channel_detail_returns_description() {
    let channel_manager = Arc::new(
        ChannelManager::new(
            crate::test_utils::create_test_shm_handle(),
            crate::test_utils::create_test_routing_cache(),
        )
        .unwrap(),
    );
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();

    // Use standard io schema from common test utils
    common::test_utils::schema::init_io_schema(&pool)
        .await
        .unwrap();

    let config = serde_json::json!({"description": "detail-desc", "host": "127.0.0.1"}).to_string();
    sqlx::query("INSERT INTO channels (channel_id, name, protocol, enabled, config) VALUES (500, 'Ch500', 'modbus_tcp', 1, ?)")
        .bind(&config)
        .execute(&pool)
        .await
        .unwrap();

    let app = create_test_api_with_pool(channel_manager, pool).await;
    let req = Request::builder()
        .uri("/api/channels/500")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    use http_body_util::BodyExt as _;
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(v["data"]["description"], "detail-desc");
    assert_eq!(v["data"]["revision"], 1);
}

#[tokio::test]
async fn test_delete_channel_ok() {
    let channel_manager = Arc::new(
        ChannelManager::new(
            crate::test_utils::create_test_shm_handle(),
            crate::test_utils::create_test_routing_cache(),
        )
        .unwrap(),
    );
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();

    // Use standard io schema from common test utils
    common::test_utils::schema::init_io_schema(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO channels (channel_id, name, protocol, enabled, config) VALUES (600, 'Ch600', 'virtual', 0, '{}')")
        .execute(&pool)
        .await
        .unwrap();
    let app = create_test_api_with_pool(channel_manager, pool).await;
    let req = channel_mutation_request("DELETE", "/api/channels/600", None);
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

// ========================================================================
// Phase X: Control/Adjustment endpoints (single & batch)
// ========================================================================

// ========================================================================
// Phase X: Mapping update endpoint
// ========================================================================

#[tokio::test]
async fn test_update_mappings_validate_only() {
    let channel_manager = Arc::new(
        ChannelManager::new(
            crate::test_utils::create_test_shm_handle(),
            crate::test_utils::create_test_routing_cache(),
        )
        .unwrap(),
    );
    let pool = create_test_sqlite_pool_with_points().await;
    // seed channel and telemetry points
    sqlx::query("INSERT INTO channels (channel_id, name, protocol, enabled, config) VALUES (8001, 'Ch8001', 'modbus_tcp', 1, '{}')")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO telemetry_points (channel_id, point_id, signal_name, scale, offset, unit, reverse, data_type, description, protocol_mappings) VALUES (8001, 101, 'T1', 1.0, 0.0, '', 0, 'float32', '', null)")
        .execute(&pool)
        .await
        .unwrap();

    let app = create_test_api_with_pool(channel_manager, pool).await;
    let body = serde_json::json!({
        "mappings": [
            {"point_id": 101, "four_remote": "T", "protocol_data": {"slave_id":1, "function_code":3, "register_address":100}}
        ],
        "validate_only": true,
        "reload_channel": false,
        "mode": "replace"
    });
    let req = Request::builder()
        .uri("/api/channels/8001/mappings")
        .method("PUT")
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn test_update_mappings_replace_persists() {
    let channel_manager = Arc::new(
        ChannelManager::new(
            crate::test_utils::create_test_shm_handle(),
            crate::test_utils::create_test_routing_cache(),
        )
        .unwrap(),
    );
    let pool = create_test_sqlite_pool_with_points().await;
    sqlx::query("INSERT INTO channels (channel_id, name, protocol, enabled, config) VALUES (8002, 'Ch8002', 'modbus_tcp', 1, '{}')")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO telemetry_points (channel_id, point_id, signal_name, scale, offset, unit, reverse, data_type, description, protocol_mappings) VALUES (8002, 101, 'T1', 1.0, 0.0, '', 0, 'float32', '', null)")
        .execute(&pool)
        .await
        .unwrap();

    let app = create_test_api_with_pool(channel_manager.clone(), pool.clone()).await;
    let body = serde_json::json!({
        "mappings": [
            {"point_id": 101, "four_remote": "T", "protocol_data": {"slave_id":1, "function_code":3, "register_address":100}}
        ],
        "validate_only": false,
        "reload_channel": false,
        "mode": "replace"
    });
    let req = Request::builder()
        .uri("/api/channels/8002/mappings")
        .method("PUT")
        .header("content-type", "application/json")
        .header("authorization", format!("Bearer {ADMIN_ACCESS_TOKEN}"))
        .header("x-request-id", TEST_REQUEST_ID)
        .header("x-aether-confirmed", "true")
        .header("x-aether-expected-revision", "1")
        .body(Body::from(body.to_string()))
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // Verify DB updated
    let row: Option<(Option<String>,)> = sqlx::query_as(
        "SELECT protocol_mappings FROM telemetry_points WHERE channel_id = 8002 AND point_id = 101",
    )
    .fetch_optional(&pool)
    .await
    .unwrap();
    let val = row.unwrap().0.unwrap();
    assert!(val.contains("\"function_code\":3"));
}

#[tokio::test]
async fn test_update_mappings_merge_persists() {
    let channel_manager = Arc::new(
        ChannelManager::new(
            crate::test_utils::create_test_shm_handle(),
            crate::test_utils::create_test_routing_cache(),
        )
        .unwrap(),
    );
    let pool = create_test_sqlite_pool_with_points().await;
    // seed channel and telemetry point with existing mapping
    sqlx::query("INSERT INTO channels (channel_id, name, protocol, enabled, config) VALUES (8010, 'Ch8010', 'modbus_tcp', 1, '{}')")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO telemetry_points (channel_id, point_id, signal_name, scale, offset, unit, reverse, data_type, description, protocol_mappings) VALUES (8010, 101, 'T1', 1.0, 0.0, '', 0, 'float32', '', '{\"slave_id\":1,\"function_code\":3}')")
        .execute(&pool)
        .await
        .unwrap();

    let app = create_test_api_with_pool(channel_manager.clone(), pool.clone()).await;
    // merge to add register_address
    let body = serde_json::json!({
        "mappings": [
            {"point_id": 101, "four_remote": "T", "protocol_data": {"register_address": 100}}
        ],
        "validate_only": false,
        "reload_channel": false,
        "mode": "merge"
    });
    let req = Request::builder()
        .uri("/api/channels/8010/mappings")
        .method("PUT")
        .header("content-type", "application/json")
        .header("authorization", format!("Bearer {ADMIN_ACCESS_TOKEN}"))
        .header("x-request-id", TEST_REQUEST_ID)
        .header("x-aether-confirmed", "true")
        .header("x-aether-expected-revision", "1")
        .body(Body::from(body.to_string()))
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // Verify DB merged
    let row: Option<(Option<String>,)> = sqlx::query_as(
        "SELECT protocol_mappings FROM telemetry_points WHERE channel_id = 8010 AND point_id = 101",
    )
    .fetch_optional(&pool)
    .await
    .unwrap();
    let val = row.unwrap().0.unwrap();
    assert!(val.contains("\"function_code\":3"));
    assert!(val.contains("\"register_address\":100"));
}

#[tokio::test]
async fn test_update_mappings_invalid_four_remote_400() {
    let channel_manager = Arc::new(
        ChannelManager::new(
            crate::test_utils::create_test_shm_handle(),
            crate::test_utils::create_test_routing_cache(),
        )
        .unwrap(),
    );
    let pool = create_test_sqlite_pool_with_points().await;
    sqlx::query("INSERT INTO channels (channel_id, name, protocol, enabled, config) VALUES (8011, 'Ch8011', 'modbus_tcp', 1, '{}')")
        .execute(&pool)
        .await
        .unwrap();
    // No need to insert point, we are testing invalid four_remote
    let app = create_test_api_with_pool(channel_manager, pool).await;
    let body = serde_json::json!({
        "mappings": [
            {"point_id": 1, "four_remote": "X", "protocol_data": {"slave_id":1}}
        ],
        "validate_only": false,
        "reload_channel": false,
        "mode": "replace"
    });
    let req = Request::builder()
        .uri("/api/channels/8011/mappings")
        .method("PUT")
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn test_update_mappings_point_not_found_400() {
    let channel_manager = Arc::new(
        ChannelManager::new(
            crate::test_utils::create_test_shm_handle(),
            crate::test_utils::create_test_routing_cache(),
        )
        .unwrap(),
    );
    let pool = create_test_sqlite_pool_with_points().await;
    sqlx::query("INSERT INTO channels (channel_id, name, protocol, enabled, config) VALUES (8012, 'Ch8012', 'modbus_tcp', 1, '{}')")
        .execute(&pool)
        .await
        .unwrap();
    // Tables exist but no matching point 999
    let app = create_test_api_with_pool(channel_manager, pool).await;
    let body = serde_json::json!({
        "mappings": [
            {"point_id": 999, "four_remote": "T", "protocol_data": {"slave_id":1}}
        ],
        "validate_only": false,
        "reload_channel": false,
        "mode": "replace"
    });
    let req = Request::builder()
        .uri("/api/channels/8012/mappings")
        .method("PUT")
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn test_update_mappings_invalid_function_code_for_t_400() {
    let channel_manager = Arc::new(
        ChannelManager::new(
            crate::test_utils::create_test_shm_handle(),
            crate::test_utils::create_test_routing_cache(),
        )
        .unwrap(),
    );
    let pool = create_test_sqlite_pool_with_points().await;
    sqlx::query("INSERT INTO channels (channel_id, name, protocol, enabled, config) VALUES (8013, 'Ch8013', 'modbus_tcp', 1, '{}')")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO telemetry_points (channel_id, point_id, signal_name, scale, offset, unit, reverse, data_type, description, protocol_mappings) VALUES (8013, 101, 'T1', 1.0, 0.0, '', 0, 'float32', '', null)")
        .execute(&pool)
        .await
        .unwrap();

    let app = create_test_api_with_pool(channel_manager, pool).await;
    // For T points, function_code 5 is invalid (should be 1/2/3/4)
    let body = serde_json::json!({
        "mappings": [
            {"point_id": 101, "four_remote": "T", "protocol_data": {"slave_id":1, "function_code":5}}
        ],
        "validate_only": false,
        "reload_channel": false,
        "mode": "replace"
    });
    let req = Request::builder()
        .uri("/api/channels/8013/mappings")
        .method("PUT")
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn test_reload_compatibility_reconciles_disabled_channel_without_runtime() {
    // Build sqlite with channels table only and a disabled channel
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();

    // Use standard io schema from common test utils
    common::test_utils::schema::init_io_schema(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO channels (channel_id, name, protocol, enabled, config) VALUES (9009, 'Ch9009', 'virtual', 0, '{\"description\": \"d\"}')")
        .execute(&pool)
        .await
        .unwrap();

    // Factory with pools to avoid filesystem DB
    let channel_manager = Arc::new(
        ChannelManager::new(
            crate::test_utils::create_test_shm_handle(),
            crate::test_utils::create_test_routing_cache(),
        )
        .unwrap(),
    );
    let app = create_test_api_with_pool(channel_manager, pool).await;

    let req = governed_reconciliation_request("/api/channels/reload");
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let payload = extract_json(resp).await;
    assert_eq!(payload["data"]["scope"], "all");
    assert_eq!(payload["data"]["items"][0]["channel_id"], 9009);
    assert_eq!(payload["data"]["items"][0]["runtime_projection"], "stopped");
}

#[tokio::test]
async fn test_get_point_info_invalid_type_400() {
    let channel_manager = Arc::new(
        ChannelManager::new(
            crate::test_utils::create_test_shm_handle(),
            crate::test_utils::create_test_routing_cache(),
        )
        .unwrap(),
    );
    let app = create_test_api_routes(channel_manager).await;
    let req = Request::builder()
        .uri("/api/channels/1/X/1")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn test_grouped_points_filter_c_and_a() {
    let channel_manager = Arc::new(
        ChannelManager::new(
            crate::test_utils::create_test_shm_handle(),
            crate::test_utils::create_test_routing_cache(),
        )
        .unwrap(),
    );
    let pool = create_test_sqlite_pool_with_points().await;
    // Seed channel and minimal points
    sqlx::query("INSERT INTO channels (channel_id, name, protocol, enabled, config) VALUES (9101, 'Ch9101', 'virtual', 1, '{}')")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO control_points (channel_id, point_id, signal_name, unit, data_type, description, protocol_mappings) VALUES (9101, 1, 'C1', '', 'uint16', '', '{}')")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO adjustment_points (channel_id, point_id, signal_name, scale, offset, unit, reverse, data_type, description, protocol_mappings) VALUES (9101, 2, 'A1', 1.0, 0.0, '', 0, 'float32', '', '{}')")
        .execute(&pool)
        .await
        .unwrap();
    let app = create_test_api_with_pool(channel_manager, pool).await;

    // Filter C
    let req_c = Request::builder()
        .uri("/api/channels/9101/points?type=C")
        .body(Body::empty())
        .unwrap();
    let resp_c = app.clone().oneshot(req_c).await.unwrap();
    assert_eq!(resp_c.status(), StatusCode::OK);
    use http_body_util::BodyExt as _;
    let bytes_c = resp_c.into_body().collect().await.unwrap().to_bytes();
    let v_c: serde_json::Value = serde_json::from_slice(&bytes_c).unwrap();
    assert_eq!(v_c["data"]["control"].as_array().unwrap().len(), 1);
    assert_eq!(v_c["data"]["telemetry"].as_array().unwrap().len(), 0);

    // Filter A
    let req_a = Request::builder()
        .uri("/api/channels/9101/points?type=A")
        .body(Body::empty())
        .unwrap();
    let resp_a = app.oneshot(req_a).await.unwrap();
    assert_eq!(resp_a.status(), StatusCode::OK);
    let bytes_a = resp_a.into_body().collect().await.unwrap().to_bytes();
    let v_a: serde_json::Value = serde_json::from_slice(&bytes_a).unwrap();
    assert_eq!(v_a["data"]["adjustment"].as_array().unwrap().len(), 1);
    assert_eq!(v_a["data"]["signal"].as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn test_get_channel_status_valid_id() {
    let channel_manager = Arc::new(
        ChannelManager::new(
            crate::test_utils::create_test_shm_handle(),
            crate::test_utils::create_test_routing_cache(),
        )
        .unwrap(),
    );
    let app = create_test_api_routes(channel_manager).await;

    let request = Request::builder()
        .uri("/api/channels/1001/status")
        .body(Body::empty())
        .unwrap();

    let response = app.oneshot(request).await.unwrap();

    // Should return 404 since channel doesn't exist, but ID format is valid
    assert!(response.status() == StatusCode::NOT_FOUND || response.status() == StatusCode::OK);
}

// ========================================================================
// Phase 3: Channel Control Endpoint Tests
// ========================================================================

fn governed_channel_control_request(
    operation: &str,
    authenticated: bool,
    confirmed: bool,
    request_id: Option<&str>,
) -> Request<Body> {
    let mut request = Request::builder()
        .uri("/api/channels/1001/control")
        .method("POST")
        .header("content-type", "application/json")
        .header("x-aether-confirmed", confirmed.to_string());
    if authenticated {
        request = request.header("authorization", format!("Bearer {ADMIN_ACCESS_TOKEN}"));
    }
    if let Some(request_id) = request_id {
        request = request.header("x-request-id", request_id);
    }
    request
        .body(Body::from(
            serde_json::json!({"operation": operation}).to_string(),
        ))
        .unwrap()
}

#[tokio::test]
async fn channel_control_forwards_start_stop_and_restart_to_governed_applications() {
    let mutator = RecordingChannelMutator::successful(None);
    let reconciler = RecordingChannelReconciler::successful(vec![ChannelReconciliationItem::new(
        aether_domain::ChannelId::new(1001),
        ChannelDesiredStateObservation::present(ChannelRevision::new(9), true),
        ChannelRuntimeProjection::Active,
    )]);
    let app = recording_channel_applications_router(
        Arc::clone(&mutator),
        Arc::clone(&reconciler),
        Arc::new(aether_store_local::MemoryAuditSink::new()),
    )
    .await;

    for (operation, enabled, revision, projection) in [
        ("start", Some(true), Some(1), "active"),
        ("stop", Some(false), Some(1), "stopped"),
        ("restart", Some(true), Some(9), "active"),
    ] {
        let response = app
            .clone()
            .oneshot(governed_channel_control_request(
                operation,
                true,
                true,
                Some(TEST_REQUEST_ID),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "{operation}");
        let payload = extract_json(response).await;
        assert_eq!(payload["success"], true);
        assert_eq!(payload["data"]["channel_id"], 1001);
        assert_eq!(payload["data"]["request_id"], TEST_REQUEST_ID);
        assert_eq!(payload["data"]["operation"], operation);
        assert_eq!(
            payload["data"]["desired_revision"],
            revision.map_or(serde_json::Value::Null, serde_json::Value::from)
        );
        assert_eq!(
            payload["data"]["desired_enabled"],
            enabled.map_or(serde_json::Value::Null, serde_json::Value::from)
        );
        assert_eq!(payload["data"]["runtime_projection"], projection);
        assert_eq!(payload["data"]["reconciliation_required"], false);
        assert_eq!(payload["data"]["completion_audit"]["status"], "recorded");
        assert_eq!(payload["data"]["retryable"], false);
    }

    let mutations = mutator.mutations();
    assert_eq!(mutations.len(), 2);
    assert_eq!(mutations[0].kind(), ChannelMutationKind::Enable);
    assert_eq!(
        mutations[0].channel_id(),
        Some(aether_domain::ChannelId::new(1001))
    );
    assert!(matches!(
        &mutations[0],
        ChannelMutation::SetEnabled { enabled: true, .. }
    ));
    assert_eq!(mutations[1].kind(), ChannelMutationKind::Disable);
    assert!(matches!(
        &mutations[1],
        ChannelMutation::SetEnabled { enabled: false, .. }
    ));
    assert_eq!(
        reconciler.scopes(),
        vec![ChannelReconciliationScope::One(
            aether_domain::ChannelId::new(1001)
        )]
    );
}

#[tokio::test]
async fn channel_control_requires_auth_confirmation_and_uuid_before_side_effects() {
    let mutator = RecordingChannelMutator::successful(None);
    let reconciler = RecordingChannelReconciler::successful(reconciliation_items());
    let app = recording_channel_applications_router(
        Arc::clone(&mutator),
        Arc::clone(&reconciler),
        Arc::new(aether_store_local::MemoryAuditSink::new()),
    )
    .await;

    for (request, expected) in [
        (
            governed_channel_control_request("start", false, true, Some(TEST_REQUEST_ID)),
            StatusCode::FORBIDDEN,
        ),
        (
            governed_channel_control_request("stop", true, false, Some(TEST_REQUEST_ID)),
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
        (
            governed_channel_control_request("restart", true, true, None),
            StatusCode::BAD_REQUEST,
        ),
        (
            governed_channel_control_request("start", true, true, Some("not-a-uuid")),
            StatusCode::BAD_REQUEST,
        ),
        (
            governed_channel_control_request("invalid", true, true, Some(TEST_REQUEST_ID)),
            StatusCode::BAD_REQUEST,
        ),
    ] {
        let response = app.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), expected);
    }

    assert!(mutator.mutations().is_empty());
    assert!(reconciler.scopes().is_empty());
}

#[tokio::test]
async fn channel_control_terminal_audit_failure_is_accepted_and_sanitized() {
    let mutator = RecordingChannelMutator::successful(None);
    let reconciler = RecordingChannelReconciler::successful(reconciliation_items());
    let app = recording_channel_applications_router(
        Arc::clone(&mutator),
        reconciler,
        Arc::new(TerminalAuditFailure),
    )
    .await;

    let response = app
        .oneshot(governed_channel_control_request(
            "start",
            true,
            true,
            Some(TEST_REQUEST_ID),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let payload = extract_json(response).await;
    assert_eq!(payload["data"]["completion_audit"]["status"], "incomplete");
    assert_eq!(payload["data"]["retryable"], false);
    assert!(!payload.to_string().contains("terminal audit unavailable"));
    assert_eq!(mutator.mutation_count(), 1);
}

#[tokio::test]
async fn legacy_router_fails_closed_for_channel_control() {
    let channel_manager = Arc::new(
        ChannelManager::new(
            crate::test_utils::create_test_shm_handle(),
            crate::test_utils::create_test_routing_cache(),
        )
        .unwrap(),
    );
    let app = create_api_routes(
        channel_manager,
        create_test_sqlite_pool().await,
        Arc::new(crate::api::command_cache::CommandTxCache::new()),
    );

    for operation in ["start", "restart"] {
        let response = app
            .clone()
            .oneshot(governed_channel_control_request(
                operation,
                true,
                true,
                Some(TEST_REQUEST_ID),
            ))
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::SERVICE_UNAVAILABLE,
            "{operation}"
        );
    }
}

// ========================================================================
// Phase 4: Command Send Endpoint Tests
// ========================================================================

#[test]
fn test_control_command_structure() {
    let cmd = ControlRequest {
        point_id: 1,
        value: 1, // u8: 0 or 1
    };

    assert_eq!(cmd.point_id, 1);
    assert_eq!(cmd.value, 1);
}

#[test]
fn test_adjustment_command_structure() {
    let cmd = AdjustmentRequest {
        point_id: 2,
        value: 50.0, // f64
    };

    assert_eq!(cmd.point_id, 2);
    assert_eq!(cmd.value, 50.0);
}

// ========================================================================
// Phase 5: Legacy Tests
// ========================================================================

#[tokio::test]
async fn test_api_routes_with_shm() {
    let channel_manager = Arc::new(
        ChannelManager::new(
            crate::test_utils::create_test_shm_handle(),
            crate::test_utils::create_test_routing_cache(),
        )
        .unwrap(),
    );
    let _app = create_test_api_routes(channel_manager).await;
    // Basic test to ensure the SHM-only route graph compiles.
    // Test passes if code compiles
}

#[test]
fn test_api_routes_compile() {
    // Verify the public route factory exposes only SHM-backed runtime state
    // plus SQLite configuration and the command dispatch cache.
    use super::*;
    use crate::api::command_cache::CommandTxCache;
    let _ = create_api_routes
        as fn(Arc<ChannelManager>, sqlx::SqlitePool, Arc<CommandTxCache>) -> Router;
}

// ========================================================================
// Phase 6: Channel CRUD Operations Tests
// ========================================================================

#[tokio::test]
async fn create_channel_without_enabled_stays_disabled_and_has_no_runtime() {
    let pool = create_test_sqlite_pool().await;
    let channel_manager = Arc::new(
        ChannelManager::with_shared_memory(
            crate::test_utils::create_test_routing_cache(),
            pool.clone(),
            crate::test_utils::create_test_shm_handle(),
            None,
            None,
        )
        .unwrap(),
    );
    let app = create_test_api_with_pool(Arc::clone(&channel_manager), pool.clone()).await;
    let request = channel_mutation_request(
        "POST",
        "/api/channels",
        Some(json!({
            "channel_id": 2101,
            "name": "Safe Default Channel",
            "protocol": "virtual",
            "parameters": {}
        })),
    );

    let response = app.oneshot(request).await.unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let payload = extract_json(response).await;
    assert_json_field(&payload, "/data/enabled", json!(false));
    assert_json_field(&payload, "/data/runtime_status", json!("stopped"));
    let persisted_enabled: bool =
        sqlx::query_scalar("SELECT enabled FROM channels WHERE channel_id = ?")
            .bind(2101_i64)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(!persisted_enabled);
    assert!(channel_manager.get_channel(2101).is_none());
    assert_eq!(channel_manager.channel_count(), 0);
}

#[tokio::test]
async fn create_channel_with_explicit_true_still_creates_runtime() {
    let pool = create_test_sqlite_pool().await;
    let channel_manager = Arc::new(
        ChannelManager::with_shared_memory(
            crate::test_utils::create_test_routing_cache(),
            pool.clone(),
            crate::test_utils::create_test_shm_handle(),
            None,
            None,
        )
        .unwrap(),
    );
    let app = create_test_api_with_pool(Arc::clone(&channel_manager), pool.clone()).await;
    let request = channel_mutation_request(
        "POST",
        "/api/channels",
        Some(json!({
            "channel_id": 2102,
            "name": "Explicitly Enabled Channel",
            "protocol": "virtual",
            "enabled": true,
            "parameters": {}
        })),
    );

    let response = app.oneshot(request).await.unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let payload = extract_json(response).await;
    assert_json_field(&payload, "/data/enabled", json!(true));
    assert!(matches!(
        payload
            .pointer("/data/runtime_status")
            .and_then(|value| value.as_str()),
        Some("connecting" | "running")
    ));
    let persisted_enabled: bool =
        sqlx::query_scalar("SELECT enabled FROM channels WHERE channel_id = ?")
            .bind(2102_i64)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(persisted_enabled);
    assert!(channel_manager.get_channel(2102).is_some());
    assert_eq!(channel_manager.channel_count(), 1);

    channel_manager.remove_channel(2102).await.unwrap();
}

#[tokio::test]
async fn test_create_channel_handler_returns_response() {
    let channel_manager = Arc::new(
        ChannelManager::new(
            crate::test_utils::create_test_shm_handle(),
            crate::test_utils::create_test_routing_cache(),
        )
        .unwrap(),
    );
    let app = create_test_api_routes(channel_manager).await;

    let mut params = HashMap::new();
    params.insert("host".to_string(), serde_json::json!("127.0.0.1"));
    params.insert("port".to_string(), serde_json::json!(502));

    let request_body = crate::dto::ChannelCreateRequest {
        channel_id: Some(2001),
        name: "Test Channel".to_string(),
        description: Some("Test Description".to_string()),
        protocol: "virtual".to_string(),
        enabled: Some(true),
        parameters: params,
        logging: None,
    };

    let request = channel_mutation_request(
        "POST",
        "/api/channels",
        Some(serde_json::to_value(request_body).unwrap()),
    );

    let response = app.oneshot(request).await.unwrap();

    // Should return 200 or appropriate status code
    assert!(
        response.status() == StatusCode::OK
            || response.status() == StatusCode::CREATED
            || response.status() == StatusCode::INTERNAL_SERVER_ERROR
    );
}

#[tokio::test]
async fn test_get_channel_detail_handler() {
    let channel_manager = Arc::new(
        ChannelManager::new(
            crate::test_utils::create_test_shm_handle(),
            crate::test_utils::create_test_routing_cache(),
        )
        .unwrap(),
    );
    let app = create_test_api_routes(channel_manager).await;

    let request = Request::builder()
        .uri("/api/channels/1001")
        .body(Body::empty())
        .unwrap();

    let response = app.oneshot(request).await.unwrap();

    // Should return 404 (not found) or 200 (if channel exists)
    assert!(response.status() == StatusCode::NOT_FOUND || response.status() == StatusCode::OK);
}

#[tokio::test]
async fn test_update_channel_handler() {
    let channel_manager = Arc::new(
        ChannelManager::new(
            crate::test_utils::create_test_shm_handle(),
            crate::test_utils::create_test_routing_cache(),
        )
        .unwrap(),
    );
    let app = create_test_api_routes(channel_manager).await;

    let mut params = HashMap::new();
    params.insert("timeout".to_string(), serde_json::json!(5000));

    let request_body = crate::dto::ChannelConfigUpdateRequest {
        channel_id: None, // No ID migration
        name: Some("Updated Channel".to_string()),
        description: Some("Updated Description".to_string()),
        protocol: None,
        parameters: Some(params),
        logging: None,
    };

    let request = channel_mutation_request(
        "PUT",
        "/api/channels/1001",
        Some(serde_json::to_value(request_body).unwrap()),
    );

    let response = app.oneshot(request).await.unwrap();

    // Should return 404 (not found) or 200 (success) or 500 (error)
    assert!(
        response.status() == StatusCode::NOT_FOUND
            || response.status() == StatusCode::OK
            || response.status() == StatusCode::INTERNAL_SERVER_ERROR
    );
}

#[tokio::test]
async fn test_delete_channel_handler() {
    let channel_manager = Arc::new(
        ChannelManager::new(
            crate::test_utils::create_test_shm_handle(),
            crate::test_utils::create_test_routing_cache(),
        )
        .unwrap(),
    );
    let app = create_test_api_routes(channel_manager).await;

    let request = channel_mutation_request("DELETE", "/api/channels/1001", None);

    let response = app.oneshot(request).await.unwrap();

    // Should return 404 (not found) or 200 (success) or 500 (error)
    assert!(
        response.status() == StatusCode::NOT_FOUND
            || response.status() == StatusCode::OK
            || response.status() == StatusCode::NO_CONTENT
            || response.status() == StatusCode::INTERNAL_SERVER_ERROR
    );
}

// ========================================================================
// Phase 7: Point and Mapping Management Tests
// ========================================================================

#[tokio::test]
async fn test_get_channel_points_handler() {
    let channel_manager = Arc::new(
        ChannelManager::new(
            crate::test_utils::create_test_shm_handle(),
            crate::test_utils::create_test_routing_cache(),
        )
        .unwrap(),
    );
    let app = create_test_api_routes(channel_manager).await;

    let request = Request::builder()
        .uri("/api/channels/1001/points")
        .body(Body::empty())
        .unwrap();

    let response = app.oneshot(request).await.unwrap();

    // Should return 200 (success) or 404 (not found)
    assert!(response.status() == StatusCode::OK || response.status() == StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn test_get_channel_points_with_type_filter() {
    let channel_manager = Arc::new(
        ChannelManager::new(
            crate::test_utils::create_test_shm_handle(),
            crate::test_utils::create_test_routing_cache(),
        )
        .unwrap(),
    );
    let app = create_test_api_routes(channel_manager).await;

    let request = Request::builder()
        .uri("/api/channels/1001/points?type=T")
        .body(Body::empty())
        .unwrap();

    let response = app.oneshot(request).await.unwrap();

    // Should return 200 (success) or 404 (not found)
    assert!(response.status() == StatusCode::OK || response.status() == StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn test_get_channel_mappings_handler() {
    let channel_manager = Arc::new(
        ChannelManager::new(
            crate::test_utils::create_test_shm_handle(),
            crate::test_utils::create_test_routing_cache(),
        )
        .unwrap(),
    );
    let app = create_test_api_routes(channel_manager).await;

    let request = Request::builder()
        .uri("/api/channels/1001/mappings")
        .body(Body::empty())
        .unwrap();

    let response = app.oneshot(request).await.unwrap();

    // Should return 200 (success) or 404 (not found) or 500 (error)
    assert!(
        response.status() == StatusCode::OK
            || response.status() == StatusCode::NOT_FOUND
            || response.status() == StatusCode::INTERNAL_SERVER_ERROR
    );
}

// ========================================================================
// Phase 8: Control Command Endpoints Tests
// ========================================================================

// ========================================================================
// Phase 9: Configuration Management Tests
// ========================================================================

#[tokio::test]
async fn test_set_channel_enabled_handler() {
    let channel_manager = Arc::new(
        ChannelManager::new(
            crate::test_utils::create_test_shm_handle(),
            crate::test_utils::create_test_routing_cache(),
        )
        .unwrap(),
    );
    let app = create_test_api_routes(channel_manager).await;

    let request_body = crate::dto::ChannelEnabledRequest { enabled: true };

    let request = channel_mutation_request(
        "PUT",
        "/api/channels/1001/enabled",
        Some(serde_json::to_value(request_body).unwrap()),
    );

    let response = app.oneshot(request).await.unwrap();

    // Should return appropriate status code
    assert!(
        response.status() == StatusCode::OK
            || response.status() == StatusCode::NOT_FOUND
            || response.status() == StatusCode::INTERNAL_SERVER_ERROR
    );
}

#[tokio::test]
async fn test_set_channel_disabled() {
    let channel_manager = Arc::new(
        ChannelManager::new(
            crate::test_utils::create_test_shm_handle(),
            crate::test_utils::create_test_routing_cache(),
        )
        .unwrap(),
    );
    let app = create_test_api_routes(channel_manager).await;

    let request_body = crate::dto::ChannelEnabledRequest { enabled: false };

    let request = channel_mutation_request(
        "PUT",
        "/api/channels/1001/enabled",
        Some(serde_json::to_value(request_body).unwrap()),
    );

    let response = app.oneshot(request).await.unwrap();

    // Should return appropriate status code
    assert!(
        response.status() == StatusCode::OK
            || response.status() == StatusCode::NOT_FOUND
            || response.status() == StatusCode::INTERNAL_SERVER_ERROR
    );
}

#[tokio::test]
async fn legacy_router_fails_closed_for_channel_reconciliation() {
    let channel_manager = Arc::new(
        ChannelManager::new(
            crate::test_utils::create_test_shm_handle(),
            crate::test_utils::create_test_routing_cache(),
        )
        .unwrap(),
    );
    let app = create_api_routes(
        channel_manager,
        create_test_sqlite_pool().await,
        Arc::new(crate::api::command_cache::CommandTxCache::new()),
    );

    for path in ["/api/channels/reconcile", "/api/channels/reload"] {
        let response = app
            .clone()
            .oneshot(governed_reconciliation_request(path))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE, "{path}");
    }
}

// ========================================================================
// Phase 10: Pagination Tests
// ========================================================================

#[tokio::test]
async fn test_get_all_channels_with_pagination() {
    let channel_manager = Arc::new(
        ChannelManager::new(
            crate::test_utils::create_test_shm_handle(),
            crate::test_utils::create_test_routing_cache(),
        )
        .unwrap(),
    );
    let app = create_test_api_routes(channel_manager).await;

    let request = Request::builder()
        .uri("/api/channels?page=1&page_size=10")
        .body(Body::empty())
        .unwrap();

    let response = app.oneshot(request).await.unwrap();

    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn test_get_all_channels_with_filter() {
    let channel_manager = Arc::new(
        ChannelManager::new(
            crate::test_utils::create_test_shm_handle(),
            crate::test_utils::create_test_routing_cache(),
        )
        .unwrap(),
    );
    let app = create_test_api_routes(channel_manager).await;

    let request = Request::builder()
        .uri("/api/channels?protocol=modbus_tcp&enabled=true")
        .body(Body::empty())
        .unwrap();

    let response = app.oneshot(request).await.unwrap();

    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn test_get_all_channels_large_page_size() {
    let channel_manager = Arc::new(
        ChannelManager::new(
            crate::test_utils::create_test_shm_handle(),
            crate::test_utils::create_test_routing_cache(),
        )
        .unwrap(),
    );
    let app = create_test_api_routes(channel_manager).await;

    // Test page_size exceeding maximum (should be clamped to 100)
    let request = Request::builder()
        .uri("/api/channels?page=1&page_size=500")
        .body(Body::empty())
        .unwrap();

    let response = app.oneshot(request).await.unwrap();

    assert_eq!(response.status(), StatusCode::OK);
}

// ========================================================================
// Phase 2: Closed-Loop Integration Tests (P0 Priority)
// ========================================================================

/// Closed-loop test: Create channel → GET channel → Verify all fields match
///
/// Tests complete data flow from POST to persistence to retrieval
#[tokio::test]
async fn test_create_channel_full_closed_loop() {
    let channel_manager = Arc::new(
        ChannelManager::new(
            crate::test_utils::create_test_shm_handle(),
            crate::test_utils::create_test_routing_cache(),
        )
        .unwrap(),
    );
    let app = create_test_api_routes(channel_manager).await;

    // Step 1: POST - Create channel with full configuration
    let create_body = serde_json::json!({
        "channel_id": 2001,
        "name": "test_virtual_channel",
        "protocol": "virtual",
        "enabled": true,
        "parameters": {
            "interval_ms": 1000,
            "initial_value": 100
        },
        "description": "Full closed-loop test channel"
    });

    let create_req = channel_mutation_request("POST", "/api/channels", Some(create_body));

    let create_resp = app.clone().oneshot(create_req).await.unwrap();
    assert_eq!(
        create_resp.status(),
        StatusCode::OK,
        "Channel creation should succeed"
    );

    // Step 2: GET - Read back channel details
    let get_req = Request::builder()
        .uri("/api/channels/2001")
        .body(Body::empty())
        .unwrap();

    let get_resp = app.oneshot(get_req).await.unwrap();
    assert_eq!(
        get_resp.status(),
        StatusCode::OK,
        "Channel retrieval should succeed"
    );

    // Step 3: Verify - All fields match what was posted
    let json = extract_json(get_resp).await;
    assert_json_field(&json, "/data/id", serde_json::json!(2001));
    assert_json_field(
        &json,
        "/data/name",
        serde_json::json!("test_virtual_channel"),
    );
    assert_json_field(&json, "/data/protocol", serde_json::json!("virtual"));
    assert_json_field(&json, "/data/enabled", serde_json::json!(true));
    assert_json_field(
        &json,
        "/data/description",
        serde_json::json!("Full closed-loop test channel"),
    );

    // Note: parameters verification depends on how they're stored/retrieved
    // Some services may store parameters as JSON string in config field
}

/// Closed-loop test: Create channel → UPDATE channel → GET → Verify changes
///
/// Tests that updates are properly persisted and retrievable
#[tokio::test]
async fn test_update_channel_full_closed_loop() {
    let channel_manager = Arc::new(
        ChannelManager::new(
            crate::test_utils::create_test_shm_handle(),
            crate::test_utils::create_test_routing_cache(),
        )
        .unwrap(),
    );
    let app = create_test_api_routes(channel_manager).await;

    // Step 1: Create initial channel
    let create_body = serde_json::json!({
        "channel_id": 2002,
        "name": "initial_name",
        "protocol": "virtual",
        "enabled": true,
        "parameters": {
            "interval_ms": 1000,
            "initial_value": 100
        },
        "description": "Initial description"
    });

    let create_req = channel_mutation_request("POST", "/api/channels", Some(create_body));

    let _ = app.clone().oneshot(create_req).await.unwrap();

    // Step 2: Update channel with new values
    // Note: enabled field is managed via /control endpoint, not PUT
    let update_body = serde_json::json!({
        "name": "updated_name",
        "protocol": "virtual",
        "parameters": {"interval_ms": 2000},
        "description": "Updated description"
    });

    let update_req = channel_mutation_request("PUT", "/api/channels/2002", Some(update_body));

    let update_resp = app.clone().oneshot(update_req).await.unwrap();
    assert_eq!(
        update_resp.status(),
        StatusCode::OK,
        "Channel update should succeed"
    );

    // Step 3: GET updated channel and verify changes
    let get_req = Request::builder()
        .uri("/api/channels/2002")
        .body(Body::empty())
        .unwrap();

    let get_resp = app.oneshot(get_req).await.unwrap();
    assert_eq!(get_resp.status(), StatusCode::OK);

    let json = extract_json(get_resp).await;
    assert_json_field(&json, "/data/id", serde_json::json!(2002));
    assert_json_field(&json, "/data/name", serde_json::json!("updated_name"));
    assert_json_field(&json, "/data/protocol", serde_json::json!("virtual"));
    // Note: enabled field remains true (initial value) - use /control endpoint to change it
    assert_json_field(&json, "/data/enabled", serde_json::json!(true));
    assert_json_field(
        &json,
        "/data/description",
        serde_json::json!("Updated description"),
    );
}

// ========================================================================
// Phase 3: P1 Priority Tests (Delete & Batch Operations)
// ========================================================================

/// Test 1: Delete Channel Closed-loop
/// Verifies that deleted channels are no longer accessible
#[tokio::test]
async fn test_delete_channel_closed_loop() {
    let channel_manager = Arc::new(
        ChannelManager::new(
            crate::test_utils::create_test_shm_handle(),
            crate::test_utils::create_test_routing_cache(),
        )
        .unwrap(),
    );
    let app = create_test_api_routes(channel_manager).await;

    // Step 1: POST - Create channel
    let create_body = serde_json::json!({
        "channel_id": 3001,
        "name": "channel_to_delete",
        "protocol": "virtual",
        "enabled": true,
        "parameters": {
            "interval_ms": 1000,
            "initial_value": 50
        },
        "description": "This channel will be deleted"
    });

    let create_req = channel_mutation_request("POST", "/api/channels", Some(create_body));

    let create_resp = app.clone().oneshot(create_req).await.unwrap();
    assert_eq!(
        create_resp.status(),
        StatusCode::OK,
        "Channel creation should succeed"
    );

    // Step 2: GET - Verify channel exists
    let get_req1 = Request::builder()
        .uri("/api/channels/3001")
        .body(Body::empty())
        .unwrap();

    let get_resp1 = app.clone().oneshot(get_req1).await.unwrap();
    assert_eq!(
        get_resp1.status(),
        StatusCode::OK,
        "Channel should exist before deletion"
    );

    // Step 3: DELETE - Remove channel
    let delete_req = channel_mutation_request("DELETE", "/api/channels/3001", None);

    let delete_resp = app.clone().oneshot(delete_req).await.unwrap();
    assert_eq!(
        delete_resp.status(),
        StatusCode::OK,
        "Channel deletion should succeed"
    );

    // Step 4: GET - Verify channel no longer exists (404)
    let get_req2 = Request::builder()
        .uri("/api/channels/3001")
        .body(Body::empty())
        .unwrap();

    let get_resp2 = app.oneshot(get_req2).await.unwrap();
    assert_eq!(
        get_resp2.status(),
        StatusCode::NOT_FOUND,
        "Deleted channel should return 404"
    );
}

// ========================================================================
// Point Mapping with Type Tests (New API)
// ========================================================================

#[tokio::test]
async fn test_get_point_mapping_with_type_telemetry_success() {
    let channel_manager = Arc::new(
        ChannelManager::new(
            crate::test_utils::create_test_shm_handle(),
            crate::test_utils::create_test_routing_cache(),
        )
        .unwrap(),
    );
    let pool = create_test_sqlite_pool_with_points().await;

    // Insert channel
    sqlx::query("INSERT INTO channels (channel_id, name, protocol, enabled, config) VALUES (1000, 'TestChannel', 'modbus_tcp', 1, '{}')")
        .execute(&pool)
        .await
        .unwrap();

    // Insert telemetry point with full protocol_mappings
    sqlx::query("INSERT INTO telemetry_points (channel_id, point_id, signal_name, scale, offset, unit, reverse, data_type, description, protocol_mappings) VALUES (1000, 1, 'Total_Power', 1.0, 0.0, 'kW', 0, 'float32', 'test', ?)")
        .bind(r#"{"slave_id":"1","function_code":"3","register_address":"100","data_type":"float32","byte_order":"ABCD"}"#)
        .execute(&pool)
        .await
        .unwrap();

    let app = create_test_api_with_pool(channel_manager, pool).await;

    // Request mapping for telemetry point
    let req = Request::builder()
        .uri("/api/channels/1000/T/points/1/mapping")
        .body(Body::empty())
        .unwrap();

    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // Parse response body
    let body_bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let response: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();

    assert_eq!(response["success"], true);
    assert_eq!(response["data"]["point_id"], 1);
    assert_eq!(response["data"]["signal_name"], "Total_Power");
    assert_eq!(response["data"]["protocol_data"]["slave_id"], "1");
    assert_eq!(response["data"]["protocol_data"]["function_code"], "3");
    assert_eq!(response["data"]["protocol_data"]["register_address"], "100");
    assert_eq!(response["data"]["protocol_data"]["byte_order"], "ABCD");
}

#[tokio::test]
async fn test_get_point_mapping_with_type_signal_success() {
    let channel_manager = Arc::new(
        ChannelManager::new(
            crate::test_utils::create_test_shm_handle(),
            crate::test_utils::create_test_routing_cache(),
        )
        .unwrap(),
    );
    let pool = create_test_sqlite_pool_with_points().await;

    sqlx::query("INSERT INTO channels (channel_id, name, protocol, enabled, config) VALUES (1001, 'TestChannel', 'modbus_tcp', 1, '{}')")
        .execute(&pool)
        .await
        .unwrap();

    // Insert signal point
    sqlx::query("INSERT INTO signal_points (channel_id, point_id, signal_name, unit, reverse, data_type, description, normal_state, protocol_mappings) VALUES (1001, 1, 'Operation_Status', '', 0, 'bool', 'test', 1, ?)")
        .bind(r#"{"slave_id":"1","function_code":"1","register_address":"200","bit_position":"0"}"#)
        .execute(&pool)
        .await
        .unwrap();

    let app = create_test_api_with_pool(channel_manager, pool).await;

    let req = Request::builder()
        .uri("/api/channels/1001/S/points/1/mapping")
        .body(Body::empty())
        .unwrap();

    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let body_bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let response: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();

    assert_eq!(response["success"], true);
    assert_eq!(response["data"]["point_id"], 1);
    assert_eq!(response["data"]["signal_name"], "Operation_Status");
    assert_eq!(response["data"]["protocol_data"]["register_address"], "200");
    assert_eq!(response["data"]["protocol_data"]["bit_position"], "0");
}

#[tokio::test]
async fn test_get_point_mapping_with_type_control_success() {
    let channel_manager = Arc::new(
        ChannelManager::new(
            crate::test_utils::create_test_shm_handle(),
            crate::test_utils::create_test_routing_cache(),
        )
        .unwrap(),
    );
    let pool = create_test_sqlite_pool_with_points().await;

    sqlx::query("INSERT INTO channels (channel_id, name, protocol, enabled, config) VALUES (1002, 'TestChannel', 'modbus_tcp', 1, '{}')")
        .execute(&pool)
        .await
        .unwrap();

    // Insert control point
    sqlx::query("INSERT INTO control_points (channel_id, point_id, signal_name, unit, data_type, description, protocol_mappings) VALUES (1002, 1, 'Start_Stop', '', 'bool', 'test', ?)")
        .bind(r#"{"slave_id":"1","function_code":"5","register_address":"0","data_type":"bool"}"#)
        .execute(&pool)
        .await
        .unwrap();

    let app = create_test_api_with_pool(channel_manager, pool).await;

    let req = Request::builder()
        .uri("/api/channels/1002/C/points/1/mapping")
        .body(Body::empty())
        .unwrap();

    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let body_bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let response: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();

    assert_eq!(response["success"], true);
    assert_eq!(response["data"]["point_id"], 1);
    assert_eq!(response["data"]["signal_name"], "Start_Stop");
    assert_eq!(response["data"]["protocol_data"]["function_code"], "5");
    assert_eq!(response["data"]["protocol_data"]["register_address"], "0");
}

#[tokio::test]
async fn test_get_point_mapping_with_type_adjustment_success() {
    let channel_manager = Arc::new(
        ChannelManager::new(
            crate::test_utils::create_test_shm_handle(),
            crate::test_utils::create_test_routing_cache(),
        )
        .unwrap(),
    );
    let pool = create_test_sqlite_pool_with_points().await;

    sqlx::query("INSERT INTO channels (channel_id, name, protocol, enabled, config) VALUES (1003, 'TestChannel', 'modbus_tcp', 1, '{}')")
        .execute(&pool)
        .await
        .unwrap();

    // Insert adjustment point
    sqlx::query("INSERT INTO adjustment_points (channel_id, point_id, signal_name, scale, offset, unit, reverse, data_type, description, protocol_mappings) VALUES (1003, 1, 'Power_Setpoint', 1.0, 0.0, 'kW', 0, 'float32', 'test', ?)")
        .bind(r#"{"slave_id":"1","function_code":"6","register_address":"100","data_type":"float32"}"#)
        .execute(&pool)
        .await
        .unwrap();

    let app = create_test_api_with_pool(channel_manager, pool).await;

    let req = Request::builder()
        .uri("/api/channels/1003/A/points/1/mapping")
        .body(Body::empty())
        .unwrap();

    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let body_bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let response: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();

    assert_eq!(response["success"], true);
    assert_eq!(response["data"]["point_id"], 1);
    assert_eq!(response["data"]["signal_name"], "Power_Setpoint");
    assert_eq!(response["data"]["protocol_data"]["function_code"], "6");
    assert_eq!(response["data"]["protocol_data"]["register_address"], "100");
}

#[tokio::test]
async fn test_get_point_mapping_with_invalid_type_returns_400() {
    let channel_manager = Arc::new(
        ChannelManager::new(
            crate::test_utils::create_test_shm_handle(),
            crate::test_utils::create_test_routing_cache(),
        )
        .unwrap(),
    );
    let pool = create_test_sqlite_pool_with_points().await;

    sqlx::query("INSERT INTO channels (channel_id, name, protocol, enabled, config) VALUES (1004, 'TestChannel', 'modbus_tcp', 1, '{}')")
        .execute(&pool)
        .await
        .unwrap();

    let app = create_test_api_with_pool(channel_manager, pool).await;

    // Use invalid four-remote type 'X'
    let req = Request::builder()
        .uri("/api/channels/1004/X/points/1/mapping")
        .body(Body::empty())
        .unwrap();

    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    let body_bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let response: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();

    assert_eq!(response["success"], false);
    assert!(
        response["error"]["message"]
            .as_str()
            .unwrap()
            .contains("Invalid point type 'X'")
    );
}

#[tokio::test]
async fn test_get_point_mapping_channel_not_found_returns_404() {
    let channel_manager = Arc::new(
        ChannelManager::new(
            crate::test_utils::create_test_shm_handle(),
            crate::test_utils::create_test_routing_cache(),
        )
        .unwrap(),
    );
    let pool = create_test_sqlite_pool_with_points().await;

    let app = create_test_api_with_pool(channel_manager, pool).await;

    // Request non-existent channel 9999
    let req = Request::builder()
        .uri("/api/channels/9999/T/points/1/mapping")
        .body(Body::empty())
        .unwrap();

    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);

    let body_bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let response: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();

    assert_eq!(response["success"], false);
    assert!(
        response["error"]["message"]
            .as_str()
            .unwrap()
            .contains("Channel 9999 not found")
    );
}

#[tokio::test]
async fn test_get_point_mapping_point_not_found_returns_404() {
    let channel_manager = Arc::new(
        ChannelManager::new(
            crate::test_utils::create_test_shm_handle(),
            crate::test_utils::create_test_routing_cache(),
        )
        .unwrap(),
    );
    let pool = create_test_sqlite_pool_with_points().await;

    sqlx::query("INSERT INTO channels (channel_id, name, protocol, enabled, config) VALUES (1005, 'TestChannel', 'modbus_tcp', 1, '{}')")
        .execute(&pool)
        .await
        .unwrap();

    // Channel exists but point 999 does not
    let app = create_test_api_with_pool(channel_manager, pool).await;

    let req = Request::builder()
        .uri("/api/channels/1005/T/points/999/mapping")
        .body(Body::empty())
        .unwrap();

    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);

    let body_bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let response: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();

    assert_eq!(response["success"], false);
    assert!(
        response["error"]["message"]
            .as_str()
            .unwrap()
            .contains("Point 999 (type T) not found")
    );
}

/// Critical test: Write-Read closed loop validation
/// Tests that database changes are immediately reflected in API responses
#[tokio::test]
async fn test_get_point_mapping_reflects_database_changes() {
    let channel_manager = Arc::new(
        ChannelManager::new(
            crate::test_utils::create_test_shm_handle(),
            crate::test_utils::create_test_routing_cache(),
        )
        .unwrap(),
    );
    let pool = create_test_sqlite_pool_with_points().await;

    // Step 1: Initialize - Create channel and point
    sqlx::query("INSERT INTO channels (channel_id, name, protocol, enabled, config) VALUES (2000, 'ClosedLoopTest', 'modbus_tcp', 1, '{}')")
        .execute(&pool)
        .await
        .unwrap();

    sqlx::query("INSERT INTO telemetry_points (channel_id, point_id, signal_name, scale, offset, unit, reverse, data_type, description, protocol_mappings) VALUES (2000, 1, 'Test_Point', 1.0, 0.0, 'kW', 0, 'float32', 'test', ?)")
        .bind(r#"{"slave_id":"1","function_code":"3","register_address":"100"}"#)
        .execute(&pool)
        .await
        .unwrap();

    let app = create_test_api_with_pool(channel_manager, pool.clone()).await;

    // Step 2: First read - Baseline
    let req1 = Request::builder()
        .uri("/api/channels/2000/T/points/1/mapping")
        .body(Body::empty())
        .unwrap();

    let resp1 = app.clone().oneshot(req1).await.unwrap();
    assert_eq!(resp1.status(), StatusCode::OK);

    let body_bytes1 = axum::body::to_bytes(resp1.into_body(), usize::MAX)
        .await
        .unwrap();
    let response1: serde_json::Value = serde_json::from_slice(&body_bytes1).unwrap();

    // Verify baseline value
    assert_eq!(
        response1["data"]["protocol_data"]["register_address"], "100",
        "Baseline: register_address should be 100"
    );

    // Step 3: Modify database - Change register_address from 100 to 999
    sqlx::query("UPDATE telemetry_points SET protocol_mappings = json_set(protocol_mappings, '$.register_address', '999') WHERE channel_id = 2000 AND point_id = 1")
        .execute(&pool)
        .await
        .unwrap();

    // Step 4: Second read - Verify modification
    let req2 = Request::builder()
        .uri("/api/channels/2000/T/points/1/mapping")
        .body(Body::empty())
        .unwrap();

    let resp2 = app.clone().oneshot(req2).await.unwrap();
    assert_eq!(resp2.status(), StatusCode::OK);

    let body_bytes2 = axum::body::to_bytes(resp2.into_body(), usize::MAX)
        .await
        .unwrap();
    let response2: serde_json::Value = serde_json::from_slice(&body_bytes2).unwrap();

    // ✅ Critical assertion: Modified value is reflected
    assert_eq!(
        response2["data"]["protocol_data"]["register_address"], "999",
        "After modification: register_address should be 999"
    );

    // Step 5: Restore original value
    sqlx::query("UPDATE telemetry_points SET protocol_mappings = json_set(protocol_mappings, '$.register_address', '100') WHERE channel_id = 2000 AND point_id = 1")
        .execute(&pool)
        .await
        .unwrap();

    // Step 6: Third read - Verify restoration
    let req3 = Request::builder()
        .uri("/api/channels/2000/T/points/1/mapping")
        .body(Body::empty())
        .unwrap();

    let resp3 = app.oneshot(req3).await.unwrap();
    assert_eq!(resp3.status(), StatusCode::OK);

    let body_bytes3 = axum::body::to_bytes(resp3.into_body(), usize::MAX)
        .await
        .unwrap();
    let response3: serde_json::Value = serde_json::from_slice(&body_bytes3).unwrap();

    // ✅ Closed loop complete: Value restored to original
    assert_eq!(
        response3["data"]["protocol_data"]["register_address"], "100",
        "After restoration: register_address should be back to 100"
    );
}

#[tokio::test]
async fn test_get_point_mapping_null_mappings_returns_empty_object() {
    let channel_manager = Arc::new(
        ChannelManager::new(
            crate::test_utils::create_test_shm_handle(),
            crate::test_utils::create_test_routing_cache(),
        )
        .unwrap(),
    );
    let pool = create_test_sqlite_pool_with_points().await;

    sqlx::query("INSERT INTO channels (channel_id, name, protocol, enabled, config) VALUES (3000, 'TestChannel', 'modbus_tcp', 1, '{}')")
        .execute(&pool)
        .await
        .unwrap();

    // Insert point with NULL protocol_mappings
    sqlx::query("INSERT INTO telemetry_points (channel_id, point_id, signal_name, scale, offset, unit, reverse, data_type, description, protocol_mappings) VALUES (3000, 1, 'No_Mapping_Point', 1.0, 0.0, 'kW', 0, 'float32', 'test', NULL)")
        .execute(&pool)
        .await
        .unwrap();

    let app = create_test_api_with_pool(channel_manager, pool).await;

    let req = Request::builder()
        .uri("/api/channels/3000/T/points/1/mapping")
        .body(Body::empty())
        .unwrap();

    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let body_bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let response: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();

    assert_eq!(response["success"], true);
    assert_eq!(response["data"]["point_id"], 1);
    assert_eq!(response["data"]["signal_name"], "No_Mapping_Point");

    // When protocol_mappings is NULL, protocol_data should be empty object
    assert_eq!(response["data"]["protocol_data"], serde_json::json!({}));
}

#[tokio::test]
async fn test_get_point_mapping_type_case_insensitive() {
    let channel_manager = Arc::new(
        ChannelManager::new(
            crate::test_utils::create_test_shm_handle(),
            crate::test_utils::create_test_routing_cache(),
        )
        .unwrap(),
    );
    let pool = create_test_sqlite_pool_with_points().await;

    sqlx::query("INSERT INTO channels (channel_id, name, protocol, enabled, config) VALUES (3001, 'TestChannel', 'modbus_tcp', 1, '{}')")
        .execute(&pool)
        .await
        .unwrap();

    sqlx::query("INSERT INTO telemetry_points (channel_id, point_id, signal_name, scale, offset, unit, reverse, data_type, description, protocol_mappings) VALUES (3001, 1, 'Test_Point', 1.0, 0.0, 'kW', 0, 'float32', 'test', ?)")
        .bind(r#"{"register_address":"50"}"#)
        .execute(&pool)
        .await
        .unwrap();

    let app = create_test_api_with_pool(channel_manager, pool).await;

    // Test lowercase 't'
    let req_lower = Request::builder()
        .uri("/api/channels/3001/t/points/1/mapping")
        .body(Body::empty())
        .unwrap();

    let resp_lower = app.clone().oneshot(req_lower).await.unwrap();
    assert_eq!(resp_lower.status(), StatusCode::OK);

    let body_bytes_lower = axum::body::to_bytes(resp_lower.into_body(), usize::MAX)
        .await
        .unwrap();
    let response_lower: serde_json::Value = serde_json::from_slice(&body_bytes_lower).unwrap();

    // Test uppercase 'T'
    let req_upper = Request::builder()
        .uri("/api/channels/3001/T/points/1/mapping")
        .body(Body::empty())
        .unwrap();

    let resp_upper = app.oneshot(req_upper).await.unwrap();
    assert_eq!(resp_upper.status(), StatusCode::OK);

    let body_bytes_upper = axum::body::to_bytes(resp_upper.into_body(), usize::MAX)
        .await
        .unwrap();
    let response_upper: serde_json::Value = serde_json::from_slice(&body_bytes_upper).unwrap();

    // Both should return the same data
    assert_eq!(
        response_lower["data"]["point_id"],
        response_upper["data"]["point_id"]
    );
    assert_eq!(
        response_lower["data"]["signal_name"],
        response_upper["data"]["signal_name"]
    );
    assert_eq!(
        response_lower["data"]["protocol_data"],
        response_upper["data"]["protocol_data"]
    );
}

/// Test type normalization in closed-loop PUT → GET
///
/// Verifies that protocol_data numeric fields are normalized to JSON numbers (not strings)
/// when writing and remain numbers when reading back.
///
/// This test validates the complete round-trip: PUT with string-typed numbers →
/// normalization → storage → GET with properly typed JSON numbers.
#[tokio::test]
async fn test_protocol_data_type_normalization_closed_loop() {
    let channel_manager = Arc::new(
        ChannelManager::new(
            crate::test_utils::create_test_shm_handle(),
            crate::test_utils::create_test_routing_cache(),
        )
        .unwrap(),
    );
    let pool = create_test_sqlite_pool_with_points().await;

    // Create test channel
    sqlx::query(
        "INSERT INTO channels (channel_id, name, protocol, enabled)
         VALUES (4001, 'test_type_normalization', 'modbus_tcp', 1)",
    )
    .execute(&pool)
    .await
    .unwrap();

    // Insert test points
    sqlx::query(
        "INSERT INTO telemetry_points
         (channel_id, point_id, signal_name, scale, offset, unit, reverse, data_type, description)
         VALUES (4001, 1, 'Test_Telemetry', 1.0, 0.0, 'kW', 0, 'float32', 'test')",
    )
    .execute(&pool)
    .await
    .unwrap();

    sqlx::query(
        "INSERT INTO control_points
         (channel_id, point_id, signal_name, scale, offset, unit, reverse, data_type, description)
         VALUES (4001, 2, 'Test_Control', 1.0, 0.0, '', 0, 'uint16', 'test')",
    )
    .execute(&pool)
    .await
    .unwrap();

    let app = create_test_api_with_pool(channel_manager, pool).await;

    // Test 1: PUT with STRING types (simulate CSV import or user input)
    let put_body = json!({
        "mappings": [
            {
                "point_id": 1,
                "four_remote": "T",
                "protocol_data": {
                    "slave_id": "1",           // ← String
                    "function_code": "3",      // ← String
                    "register_address": "100", // ← String
                    "data_type": "float32",
                    "byte_order": "ABCD"
                }
            },
            {
                "point_id": 2,
                "four_remote": "C",
                "protocol_data": {
                    "slave_id": "2",           // ← String
                    "function_code": "5",      // ← String
                    "register_address": "200", // ← String
                    "data_type": "uint16",
                    "byte_order": "AB"
                }
            }
        ],
        "validate_only": false,
        "mode": "replace"
    });

    let put_req = Request::builder()
        .uri("/api/channels/4001/mappings")
        .method("PUT")
        .header("content-type", "application/json")
        .header("authorization", format!("Bearer {ADMIN_ACCESS_TOKEN}"))
        .header("x-request-id", TEST_REQUEST_ID)
        .header("x-aether-confirmed", "true")
        .header("x-aether-expected-revision", "1")
        .body(Body::from(serde_json::to_string(&put_body).unwrap()))
        .unwrap();

    let put_resp = app.clone().oneshot(put_req).await.unwrap();
    assert_eq!(put_resp.status(), StatusCode::OK);

    // Test 2: GET and verify types are NUMBERS
    let get_req = Request::builder()
        .uri("/api/channels/4001/mappings")
        .body(Body::empty())
        .unwrap();

    let get_resp = app.oneshot(get_req).await.unwrap();
    assert_eq!(get_resp.status(), StatusCode::OK);

    let body_bytes = axum::body::to_bytes(get_resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let response: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();

    // Verify telemetry point (T)
    let telemetry = &response["data"]["telemetry"][0]["protocol_data"];
    assert!(
        telemetry["slave_id"].is_number(),
        "slave_id should be number, got: {:?}",
        telemetry["slave_id"]
    );
    assert_eq!(telemetry["slave_id"], 1); // Verify value
    assert!(
        telemetry["function_code"].is_number(),
        "function_code should be number, got: {:?}",
        telemetry["function_code"]
    );
    assert_eq!(telemetry["function_code"], 3);
    assert!(
        telemetry["register_address"].is_number(),
        "register_address should be number, got: {:?}",
        telemetry["register_address"]
    );
    assert_eq!(telemetry["register_address"], 100);

    // Verify control point (C)
    let control = &response["data"]["control"][0]["protocol_data"];
    assert!(
        control["slave_id"].is_number(),
        "slave_id should be number, got: {:?}",
        control["slave_id"]
    );
    assert_eq!(control["slave_id"], 2);
    assert!(
        control["function_code"].is_number(),
        "function_code should be number, got: {:?}",
        control["function_code"]
    );
    assert_eq!(control["function_code"], 5);
    assert!(
        control["register_address"].is_number(),
        "register_address should be number, got: {:?}",
        control["register_address"]
    );
    assert_eq!(control["register_address"], 200);

    // String fields should remain strings
    assert!(telemetry["data_type"].is_string());
    assert!(telemetry["byte_order"].is_string());
}

// ========================================================================
// Write API Tests (Unified Endpoint) - P0/P1/P2 Priority
// ========================================================================

/// Helper: Setup test environment with authoritative SHM and a stub
/// command sender registered for channel 1005.
///
/// The fail-closed C/A write path in `write_channel_point` requires a
/// registered mpsc sender via `CommandTxCache::register` before any
/// Control/Adjustment write is accepted (otherwise it returns 503
/// "Channel offline; command not dispatched"). Every test that writes
/// to channel 1005 needs this stub.
///
/// The returned tuple's third element is a background drainer task that
/// silently consumes commands sent to channel 1005. Tests should bind
/// it as `_drainer` so it stays alive for the test's duration; dropping
/// the JoinHandle does not abort the task, but holding it keeps the
/// intent visible.
async fn setup_write_test_env() -> (Router, Arc<ShmWriterHandle>, tokio::task::JoinHandle<()>) {
    use crate::core::channels::types::ChannelCommand;

    let shm_handle = crate::test_utils::create_test_shm_handle_with_points(BTreeMap::from([(
        1005,
        [103, 103, 13, 203],
    )]));
    let channel_manager = Arc::new(
        ChannelManager::new(
            Arc::clone(&shm_handle),
            crate::test_utils::create_test_routing_cache(),
        )
        .unwrap(),
    );
    let pool = create_test_sqlite_pool().await;
    sqlx::query(
        "INSERT INTO channels (channel_id, name, protocol, enabled)
         VALUES (1005, 'write-test', 'virtual', 1)",
    )
    .execute(&pool)
    .await
    .unwrap();
    for point_id in [10_i64, 11, 12] {
        sqlx::query(
            "INSERT INTO control_points (channel_id, point_id, signal_name)
             VALUES (1005, ?, ?)",
        )
        .bind(point_id)
        .bind(format!("control-{point_id}"))
        .execute(&pool)
        .await
        .unwrap();
    }
    for point_id in [10_i64, 200, 201, 202] {
        sqlx::query(
            "INSERT INTO adjustment_points
             (channel_id, point_id, signal_name, min_value, max_value, step)
             VALUES (1005, ?, ?, 0.0, 5000.0, 1.0)",
        )
        .bind(point_id)
        .bind(format!("adjustment-{point_id}"))
        .execute(&pool)
        .await
        .unwrap();
    }

    // Build the command tx cache up front so we can register a stub
    // sender BEFORE the router is constructed (and therefore before any
    // test fires a write request).
    let command_tx_cache = Arc::new(crate::api::command_cache::CommandTxCache::new());
    let (tx, mut rx) = tokio::sync::mpsc::channel::<ChannelCommand>(64);
    command_tx_cache.register(1005, tx);
    let drainer = tokio::spawn(async move {
        while rx.recv().await.is_some() {
            // discard
        }
    });

    let router =
        create_api_routes_with_simulation_writes(channel_manager, pool, command_tx_cache, true);
    (router, shm_handle, drainer)
}

/// Helper: Extract JSON from response body
async fn extract_write_response_json(resp: Response<Body>) -> serde_json::Value {
    use http_body_util::BodyExt;
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap()
}

/// Helper: Send write request to unified endpoint
async fn send_write_request(
    app: Router,
    channel_id: u32,
    body: serde_json::Value,
) -> Response<Body> {
    let req = Request::builder()
        .uri(format!("/api/channels/{}/write", channel_id))
        .method("POST")
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();

    app.oneshot(req).await.unwrap()
}

// Device commands must enter through automation's application boundary.

#[tokio::test]
async fn test_simulation_writes_are_disabled_by_default() {
    let shm_handle = crate::test_utils::create_test_shm_handle_with_points(BTreeMap::from([(
        1005,
        [103, 103, 13, 203],
    )]));
    let channel_manager = Arc::new(
        ChannelManager::new(
            Arc::clone(&shm_handle),
            crate::test_utils::create_test_routing_cache(),
        )
        .unwrap(),
    );
    let app = create_api_routes(
        channel_manager,
        create_test_sqlite_pool().await,
        Arc::new(crate::api::command_cache::CommandTxCache::new()),
    );

    let response = send_write_request(
        app,
        1005,
        serde_json::json!({"type": "T", "id": "1", "value": 42.0}),
    )
    .await;

    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn test_write_rejects_direct_control_and_adjustment_points() {
    let (app, _shm, _drainer) = setup_write_test_env().await;

    for body in [
        serde_json::json!({"type": "C", "id": "10", "value": 1.0}),
        serde_json::json!({"type": "A", "id": "200", "value": 4500.0}),
    ] {
        let response = send_write_request(app.clone(), 1005, body).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    for body in [
        serde_json::json!({"type": "C", "points": [{"id": "10", "value": 1.0}]}),
        serde_json::json!({"type": "A", "points": [{"id": "200", "value": 4500.0}]}),
    ] {
        let response = send_write_request(app.clone(), 1005, body).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }
}

// ===== P1: New Feature Tests (5 tests) =====

#[tokio::test]
async fn test_write_single_telemetry_point() {
    let (app, shm, _drainer) = setup_write_test_env().await;

    let request_body = serde_json::json!({
        "type": "T",
        "id": "1",
        "value": 123.45
    });

    let resp = send_write_request(app, 1005, request_body).await;
    assert_eq!(resp.status(), StatusCode::OK);

    let json = extract_write_response_json(resp).await;
    assert_eq!(json["data"]["point_type"], "T");
    assert_eq!(json["data"]["value"], 123.45);

    crate::test_utils::assert_channel_value(&shm, 1005, PointType::Telemetry, 1, 123.45);
}

#[tokio::test]
async fn test_write_single_signal_point() {
    let (app, shm, _drainer) = setup_write_test_env().await;

    let request_body = serde_json::json!({
        "type": "S",
        "id": "100",
        "value": 1.0
    });

    let resp = send_write_request(app, 1005, request_body).await;
    assert_eq!(resp.status(), StatusCode::OK);

    let json = extract_write_response_json(resp).await;
    assert_eq!(json["data"]["point_type"], "S");

    crate::test_utils::assert_channel_value(&shm, 1005, PointType::Signal, 100, 1.0);
}

#[tokio::test]
async fn test_write_batch_telemetry_points() {
    let (app, shm, _drainer) = setup_write_test_env().await;

    let request_body = serde_json::json!({
        "type": "T",
        "points": [
            {"id": "1", "value": 100.0},
            {"id": "2", "value": 200.0}
        ]
    });

    let resp = send_write_request(app, 1005, request_body).await;
    assert_eq!(resp.status(), StatusCode::OK);

    let json = extract_write_response_json(resp).await;
    assert_eq!(json["data"]["total"], 2);
    assert_eq!(json["data"]["succeeded"], 2);

    crate::test_utils::assert_channel_value(&shm, 1005, PointType::Telemetry, 1, 100.0);
    crate::test_utils::assert_channel_value(&shm, 1005, PointType::Telemetry, 2, 200.0);
}

#[tokio::test]
async fn test_point_type_normalization_short_names() {
    let (app, _shm, _drainer) = setup_write_test_env().await;

    for point_type in &["T", "S"] {
        let request_body = serde_json::json!({
            "type": point_type,
            "id": "10",
            "value": 1.0
        });

        let resp = send_write_request(app.clone(), 1005, request_body).await;
        assert_eq!(
            resp.status(),
            StatusCode::OK,
            "Type {} should be accepted",
            point_type
        );

        let json = extract_write_response_json(resp).await;
        assert!(json["success"].as_bool().unwrap());
    }

    for point_type in &["C", "A"] {
        let response = send_write_request(
            app.clone(),
            1005,
            serde_json::json!({"type": point_type, "id": "10", "value": 1.0}),
        )
        .await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }
}

#[tokio::test]
async fn test_point_type_normalization_full_names() {
    let (app, _shm, _drainer) = setup_write_test_env().await;

    // Test full names and case variations
    let test_types = vec![
        ("Telemetry", "T"),
        ("telemetry", "T"),
        ("TELEMETRY", "T"),
        ("Signal", "S"),
        ("signal", "S"),
        ("SIGNAL", "S"),
    ];

    for (input_type, expected_short) in test_types {
        let request_body = serde_json::json!({
            "type": input_type,
            "id": "10",
            "value": 1.0
        });

        let resp = send_write_request(app.clone(), 1005, request_body).await;
        assert_eq!(
            resp.status(),
            StatusCode::OK,
            "Type {} should be accepted",
            input_type
        );

        let json = extract_write_response_json(resp).await;
        assert_eq!(
            json["data"]["point_type"], expected_short,
            "Type {} should normalize to {}",
            input_type, expected_short
        );
    }

    for input_type in [
        "Control",
        "control",
        "CONTROL",
        "Adjustment",
        "adjustment",
        "ADJUSTMENT",
    ] {
        let response = send_write_request(
            app.clone(),
            1005,
            serde_json::json!({"type": input_type, "id": "10", "value": 1.0}),
        )
        .await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }
}

// ===== P2: Error Handling & Boundary Conditions (4 tests) =====

#[tokio::test]
async fn test_write_invalid_point_type() {
    let (app, _shm, _drainer) = setup_write_test_env().await;

    let request_body = serde_json::json!({
        "type": "X",
        "id": "10",
        "value": 1.0
    });

    let resp = send_write_request(app, 1005, request_body).await;

    // Should return error (400 or 500)
    assert!(
        resp.status().is_client_error() || resp.status().is_server_error(),
        "Invalid type should return error status"
    );

    let json = extract_write_response_json(resp).await;
    assert!(!json["success"].as_bool().unwrap_or(false));
}

#[tokio::test]
async fn test_write_empty_batch_commands() {
    let (app, _shm, _drainer) = setup_write_test_env().await;

    let request_body = serde_json::json!({
        "type": "T",
        "points": []
    });

    let resp = send_write_request(app, 1005, request_body).await;

    // Should handle gracefully (200 with 0 succeeded or 400 error)
    assert!(
        resp.status().is_success() || resp.status().is_client_error(),
        "Empty batch should be handled gracefully"
    );
}

#[tokio::test]
async fn test_write_response_format_single() {
    let (app, _shm, _drainer) = setup_write_test_env().await;

    let request_body = serde_json::json!({
        "type": "T",
        "id": "10",
        "value": 1.0
    });

    let resp = send_write_request(app, 1005, request_body).await;
    let json = extract_write_response_json(resp).await;

    // Verify single response format
    assert!(json["success"].is_boolean());
    assert!(json["data"].is_object());
    assert!(json["data"]["channel_id"].is_number());
    assert!(json["data"]["point_type"].is_string());
    assert!(json["data"]["point_id"].is_number());
    assert!(json["data"]["value"].is_number());
    assert!(json["data"]["timestamp_ms"].is_number());
}

#[tokio::test]
async fn test_write_response_format_batch() {
    let (app, _shm, _drainer) = setup_write_test_env().await;

    let request_body = serde_json::json!({
        "type": "T",
        "points": [
            {"id": "10", "value": 1.0},
            {"id": "11", "value": 0.0}
        ]
    });

    let resp = send_write_request(app, 1005, request_body).await;
    let json = extract_write_response_json(resp).await;

    // Verify batch response format
    assert!(json["success"].is_boolean());
    assert!(json["data"].is_object());
    assert!(json["data"]["total"].is_number());
    assert!(json["data"]["succeeded"].is_number());
    assert!(json["data"]["failed"].is_number());
    assert!(json["data"]["errors"].is_array());
}

// ========================================================================
// Template API Tests
// ========================================================================

/// Helper: Create a test app with a shared SQLite pool for template tests
async fn create_template_test_app() -> (Router, SqlitePool) {
    let channel_manager = Arc::new(
        ChannelManager::new(
            crate::test_utils::create_test_shm_handle(),
            crate::test_utils::create_test_routing_cache(),
        )
        .unwrap(),
    );
    let pool = create_test_sqlite_pool_with_points().await;
    let app = create_test_api_with_pool(channel_manager, pool.clone()).await;
    (app, pool)
}

/// Helper: Rebuild the router from the same pool (since oneshot consumes the router)
async fn rebuild_template_app(pool: SqlitePool) -> Router {
    let channel_manager = Arc::new(
        ChannelManager::new(
            crate::test_utils::create_test_shm_handle(),
            crate::test_utils::create_test_routing_cache(),
        )
        .unwrap(),
    );
    create_test_api_with_pool(channel_manager, pool).await
}

/// Helper: Send a JSON POST request and return the response
async fn send_json_request(
    app: Router,
    method: &str,
    uri: &str,
    body: Option<serde_json::Value>,
) -> Response<Body> {
    let mut builder = Request::builder()
        .uri(uri)
        .header("content-type", "application/json");
    builder = match method {
        "POST" => builder.method("POST"),
        "PUT" => builder.method("PUT"),
        "DELETE" => builder.method("DELETE"),
        _ => builder.method("GET"),
    };
    if uri.contains("/apply/") {
        builder = builder
            .header("authorization", format!("Bearer {ADMIN_ACCESS_TOKEN}"))
            .header("x-request-id", TEST_REQUEST_ID)
            .header("x-aether-confirmed", "true")
            .header("x-aether-expected-revision", "1");
    }

    let body = match body {
        Some(json) => Body::from(serde_json::to_string(&json).unwrap()),
        None => Body::empty(),
    };

    app.oneshot(builder.body(body).unwrap()).await.unwrap()
}

#[tokio::test]
async fn test_list_templates_empty() {
    let (app, _pool) = create_template_test_app().await;

    let resp = send_json_request(app, "GET", "/api/templates", None).await;
    assert_eq!(resp.status(), StatusCode::OK);

    let json = extract_json(resp).await;
    assert_eq!(json["success"], true);
    assert_eq!(json["data"].as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn test_create_template_manually() {
    let (app, pool) = create_template_test_app().await;

    let body = json!({
        "name": "Test Template",
        "description": "Unit test template",
        "protocol": "modbus_tcp",
        "points_snapshot": {
            "telemetry": [{"point_id": 1, "signal_name": "voltage", "scale": 1.0, "offset": 0.0, "unit": "V", "data_type": "float32", "reverse": false, "description": ""}]
        },
        "mappings_snapshot": {
            "telemetry": [{"point_id": 1, "signal_name": "voltage", "protocol_data": {"register": 0, "slave_id": 1}}]
        }
    });

    let resp = send_json_request(app, "POST", "/api/templates", Some(body)).await;
    assert_eq!(resp.status(), StatusCode::OK);

    let json = extract_json(resp).await;
    assert_eq!(json["success"], true);
    assert_eq!(json["data"]["name"], "Test Template");
    assert_eq!(json["data"]["protocol"], "modbus_tcp");
    assert!(json["data"]["template_id"].as_i64().unwrap() > 0);

    // Verify it shows up in list
    let app2 = rebuild_template_app(pool).await;
    let resp2 = send_json_request(app2, "GET", "/api/templates", None).await;
    let json2 = extract_json(resp2).await;
    assert_eq!(json2["data"].as_array().unwrap().len(), 1);
    assert_eq!(json2["data"][0]["name"], "Test Template");
}

#[tokio::test]
async fn test_create_template_duplicate_name_returns_409() {
    let (app, pool) = create_template_test_app().await;

    let body = json!({
        "name": "Duplicate",
        "protocol": "modbus_tcp",
        "points_snapshot": {},
        "mappings_snapshot": {}
    });

    let resp = send_json_request(app, "POST", "/api/templates", Some(body.clone())).await;
    assert_eq!(resp.status(), StatusCode::OK);

    // Second create with same name
    let app2 = rebuild_template_app(pool).await;
    let resp2 = send_json_request(app2, "POST", "/api/templates", Some(body)).await;
    assert_eq!(resp2.status(), StatusCode::CONFLICT);
}

#[tokio::test]
async fn test_get_template_not_found() {
    let (app, _pool) = create_template_test_app().await;

    let resp = send_json_request(app, "GET", "/api/templates/9999", None).await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn test_get_template_detail() {
    let (app, pool) = create_template_test_app().await;

    let body = json!({
        "name": "Detail Test",
        "protocol": "modbus_tcp",
        "points_snapshot": {"telemetry": [{"point_id": 1, "signal_name": "v", "scale": 1.0, "offset": 0.0, "unit": "V", "data_type": "float32", "reverse": false, "description": ""}]},
        "mappings_snapshot": {"telemetry": [{"point_id": 1, "signal_name": "v", "protocol_data": {}}]}
    });

    let resp = send_json_request(app, "POST", "/api/templates", Some(body)).await;
    let created = extract_json(resp).await;
    let template_id = created["data"]["template_id"].as_i64().unwrap();

    let app2 = rebuild_template_app(pool).await;
    let resp2 = send_json_request(
        app2,
        "GET",
        &format!("/api/templates/{}", template_id),
        None,
    )
    .await;
    assert_eq!(resp2.status(), StatusCode::OK);

    let json = extract_json(resp2).await;
    assert_eq!(json["data"]["name"], "Detail Test");
    assert!(json["data"]["points_snapshot"]["telemetry"].is_array());
    assert!(json["data"]["mappings_snapshot"]["telemetry"].is_array());
}

#[tokio::test]
async fn test_update_template() {
    let (app, pool) = create_template_test_app().await;

    let body = json!({
        "name": "Before Update",
        "protocol": "modbus_tcp",
        "points_snapshot": {},
        "mappings_snapshot": {}
    });

    let resp = send_json_request(app, "POST", "/api/templates", Some(body)).await;
    let created = extract_json(resp).await;
    let template_id = created["data"]["template_id"].as_i64().unwrap();

    // Update name
    let app2 = rebuild_template_app(pool.clone()).await;
    let update_body = json!({ "name": "After Update" });
    let resp2 = send_json_request(
        app2,
        "PUT",
        &format!("/api/templates/{}", template_id),
        Some(update_body),
    )
    .await;
    assert_eq!(resp2.status(), StatusCode::OK);

    // Verify updated
    let app3 = rebuild_template_app(pool).await;
    let resp3 = send_json_request(
        app3,
        "GET",
        &format!("/api/templates/{}", template_id),
        None,
    )
    .await;
    let json = extract_json(resp3).await;
    assert_eq!(json["data"]["name"], "After Update");
}

#[tokio::test]
async fn test_update_template_not_found() {
    let (app, _pool) = create_template_test_app().await;

    let body = json!({ "name": "No Such Template" });
    let resp = send_json_request(app, "PUT", "/api/templates/9999", Some(body)).await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn test_delete_template() {
    let (app, pool) = create_template_test_app().await;

    let body = json!({
        "name": "To Delete",
        "protocol": "modbus_tcp",
        "points_snapshot": {},
        "mappings_snapshot": {}
    });

    let resp = send_json_request(app, "POST", "/api/templates", Some(body)).await;
    let created = extract_json(resp).await;
    let template_id = created["data"]["template_id"].as_i64().unwrap();

    // Delete
    let app2 = rebuild_template_app(pool.clone()).await;
    let resp2 = send_json_request(
        app2,
        "DELETE",
        &format!("/api/templates/{}", template_id),
        None,
    )
    .await;
    assert_eq!(resp2.status(), StatusCode::OK);

    // Verify gone
    let app3 = rebuild_template_app(pool).await;
    let resp3 = send_json_request(
        app3,
        "GET",
        &format!("/api/templates/{}", template_id),
        None,
    )
    .await;
    assert_eq!(resp3.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn test_delete_template_not_found() {
    let (app, _pool) = create_template_test_app().await;

    let resp = send_json_request(app, "DELETE", "/api/templates/9999", None).await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn test_list_templates_filter_by_protocol() {
    let (app, pool) = create_template_test_app().await;

    // Create modbus template
    let body1 = json!({
        "name": "Modbus Template",
        "protocol": "modbus_tcp",
        "points_snapshot": {},
        "mappings_snapshot": {}
    });
    send_json_request(app, "POST", "/api/templates", Some(body1)).await;

    // Create another protocol template
    let app2 = rebuild_template_app(pool.clone()).await;
    let body2 = json!({
        "name": "GPIO Template",
        "protocol": "gpio",
        "points_snapshot": {},
        "mappings_snapshot": {}
    });
    send_json_request(app2, "POST", "/api/templates", Some(body2)).await;

    // Filter by modbus_tcp
    let app3 = rebuild_template_app(pool.clone()).await;
    let resp = send_json_request(app3, "GET", "/api/templates?protocol=modbus_tcp", None).await;
    let json = extract_json(resp).await;
    assert_eq!(json["data"].as_array().unwrap().len(), 1);
    assert_eq!(json["data"][0]["protocol"], "modbus_tcp");

    // Filter by gpio
    let app4 = rebuild_template_app(pool).await;
    let resp2 = send_json_request(app4, "GET", "/api/templates?protocol=gpio", None).await;
    let json2 = extract_json(resp2).await;
    assert_eq!(json2["data"].as_array().unwrap().len(), 1);
    assert_eq!(json2["data"][0]["protocol"], "gpio");
}

#[tokio::test]
async fn test_create_template_empty_name_returns_400() {
    let (app, _pool) = create_template_test_app().await;

    let body = json!({
        "name": "   ",
        "protocol": "modbus_tcp",
        "points_snapshot": {},
        "mappings_snapshot": {}
    });

    let resp = send_json_request(app, "POST", "/api/templates", Some(body)).await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn test_create_template_from_channel() {
    let (_app, pool) = create_template_test_app().await;

    // Insert a test channel
    sqlx::query(
        "INSERT INTO channels (channel_id, name, protocol, enabled, config) VALUES (?, ?, ?, ?, ?)",
    )
    .bind(1001_i64)
    .bind("PCS#1")
    .bind("modbus_tcp")
    .bind(true)
    .bind("{}")
    .execute(&pool)
    .await
    .unwrap();

    // Insert a test telemetry point
    sqlx::query("INSERT INTO telemetry_points (channel_id, point_id, signal_name, scale, offset, unit, data_type, reverse, description) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)")
        .bind(1001_i64)
        .bind(1_i64)
        .bind("voltage")
        .bind(1.0)
        .bind(0.0)
        .bind("V")
        .bind("float32")
        .bind(false)
        .bind("Phase A voltage")
        .execute(&pool)
        .await
        .unwrap();

    sqlx::query(
        "INSERT INTO adjustment_points \
         (channel_id, point_id, signal_name, scale, offset, unit, data_type, reverse, description, min_value, max_value, step) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(1001_i64)
    .bind(2_i64)
    .bind("power_setpoint")
    .bind(0.1)
    .bind(0.0)
    .bind("kW")
    .bind("float32")
    .bind(false)
    .bind("Active power command")
    .bind(-500.0)
    .bind(500.0)
    .bind(0.5)
    .execute(&pool)
    .await
    .unwrap();

    let app = rebuild_template_app(pool.clone()).await;
    let body = json!({
        "name": "From Channel Template",
        "description": "Snapshot from PCS#1"
    });

    let resp = send_json_request(app, "POST", "/api/templates/from-channel/1001", Some(body)).await;
    assert_eq!(resp.status(), StatusCode::OK);

    let json = extract_json(resp).await;
    assert_eq!(json["data"]["name"], "From Channel Template");
    assert_eq!(json["data"]["protocol"], "modbus_tcp");
    assert_eq!(json["data"]["source_channel_id"], 1001);

    // Verify telemetry points were captured
    let points = &json["data"]["points_snapshot"]["telemetry"];
    assert_eq!(points.as_array().unwrap().len(), 1);
    assert_eq!(points[0]["signal_name"], "voltage");
    let adjustment = &json["data"]["points_snapshot"]["adjustment"][0];
    assert_eq!(adjustment["signal_name"], "power_setpoint");
    assert_eq!(adjustment["min_value"], -500.0);
    assert_eq!(adjustment["max_value"], 500.0);
    assert_eq!(adjustment["step"], 0.5);
}

#[tokio::test]
async fn test_create_template_from_nonexistent_channel() {
    let (app, _pool) = create_template_test_app().await;

    let body = json!({ "name": "From Nowhere" });
    let resp = send_json_request(app, "POST", "/api/templates/from-channel/9999", Some(body)).await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn test_apply_template_to_channel() {
    let (_app, pool) = create_template_test_app().await;

    // Insert target channel
    sqlx::query(
        "INSERT INTO channels (channel_id, name, protocol, enabled, config) VALUES (?, ?, ?, ?, ?)",
    )
    .bind(2001_i64)
    .bind("Target#1")
    .bind("modbus_tcp")
    .bind(true)
    .bind("{}")
    .execute(&pool)
    .await
    .unwrap();

    // Create a template first
    let app = rebuild_template_app(pool.clone()).await;
    let create_body = json!({
        "name": "Apply Test Template",
        "protocol": "modbus_tcp",
        "points_snapshot": {
            "telemetry": [{"point_id": 1, "signal_name": "v", "scale": 1.0, "offset": 0.0, "unit": "V", "data_type": "float32", "reverse": false, "description": "voltage"}],
            "signal": [{"point_id": 1, "signal_name": "alarm", "scale": 1.0, "offset": 0.0, "unit": "", "data_type": "bool", "reverse": false, "normal_state": 0, "description": "alarm"}]
        },
        "mappings_snapshot": {
            "telemetry": [{"point_id": 1, "signal_name": "v", "protocol_data": {"register_address": 0, "slave_id": 1, "function_code": 3}}],
            "signal": [{"point_id": 1, "signal_name": "alarm", "protocol_data": {}}]
        }
    });
    let resp = send_json_request(app, "POST", "/api/templates", Some(create_body)).await;
    let created = extract_json(resp).await;
    let template_id = created["data"]["template_id"].as_i64().unwrap();

    // Apply template to channel
    let app2 = rebuild_template_app(pool.clone()).await;
    let apply_body = json!({ "clear_existing": true });
    let resp2 = send_json_request(
        app2,
        "POST",
        &format!("/api/templates/{}/apply/2001", template_id),
        Some(apply_body),
    )
    .await;
    assert_eq!(resp2.status(), StatusCode::OK);

    let json = extract_json(resp2).await;
    assert_eq!(json["data"]["points_inserted"], 2);
    assert_eq!(json["data"]["channel_id"], 2001);

    // Verify points were inserted in the DB
    let count: (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM telemetry_points WHERE channel_id = ?")
            .bind(2001_i64)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(count.0, 1);

    let count_sig: (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM signal_points WHERE channel_id = ?")
            .bind(2001_i64)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(count_sig.0, 1);
}

#[tokio::test]
async fn test_apply_template_protocol_mismatch() {
    let (_app, pool) = create_template_test_app().await;

    // Insert channel with different protocol
    sqlx::query(
        "INSERT INTO channels (channel_id, name, protocol, enabled, config) VALUES (?, ?, ?, ?, ?)",
    )
    .bind(3001_i64)
    .bind("GPIO#1")
    .bind("gpio")
    .bind(true)
    .bind("{}")
    .execute(&pool)
    .await
    .unwrap();

    // Create modbus template
    let app = rebuild_template_app(pool.clone()).await;
    let body = json!({
        "name": "Modbus Only",
        "protocol": "modbus_tcp",
        "points_snapshot": {},
        "mappings_snapshot": {}
    });
    let resp = send_json_request(app, "POST", "/api/templates", Some(body)).await;
    let created = extract_json(resp).await;
    let template_id = created["data"]["template_id"].as_i64().unwrap();

    // Apply modbus template to gpio channel → should fail
    let app2 = rebuild_template_app(pool).await;
    let apply_body = json!({ "clear_existing": false });
    let resp2 = send_json_request(
        app2,
        "POST",
        &format!("/api/templates/{}/apply/3001", template_id),
        Some(apply_body),
    )
    .await;
    assert_eq!(resp2.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn test_apply_template_not_found() {
    let (_app, pool) = create_template_test_app().await;

    // Insert target channel
    sqlx::query(
        "INSERT INTO channels (channel_id, name, protocol, enabled, config) VALUES (?, ?, ?, ?, ?)",
    )
    .bind(4001_i64)
    .bind("Ch#4001")
    .bind("modbus_tcp")
    .bind(true)
    .bind("{}")
    .execute(&pool)
    .await
    .unwrap();

    let app = rebuild_template_app(pool).await;
    let apply_body = json!({ "clear_existing": false });
    let resp = send_json_request(
        app,
        "POST",
        "/api/templates/9999/apply/4001",
        Some(apply_body),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn test_apply_template_with_slave_id_override() {
    let (_app, pool) = create_template_test_app().await;

    // Insert target channel
    sqlx::query(
        "INSERT INTO channels (channel_id, name, protocol, enabled, config) VALUES (?, ?, ?, ?, ?)",
    )
    .bind(5001_i64)
    .bind("Override#1")
    .bind("modbus_tcp")
    .bind(true)
    .bind("{}")
    .execute(&pool)
    .await
    .unwrap();

    // Create template with slave_id in mapping
    let app = rebuild_template_app(pool.clone()).await;
    let body = json!({
        "name": "Override Template",
        "protocol": "modbus_tcp",
        "points_snapshot": {
            "telemetry": [{"point_id": 1, "signal_name": "v", "scale": 1.0, "offset": 0.0, "unit": "V", "data_type": "float32", "reverse": false, "description": ""}]
        },
        "mappings_snapshot": {
            "telemetry": [{"point_id": 1, "signal_name": "v", "protocol_data": {"register_address": 100, "slave_id": 1, "function_code": 3}}]
        }
    });
    let resp = send_json_request(app, "POST", "/api/templates", Some(body)).await;
    let created = extract_json(resp).await;
    let template_id = created["data"]["template_id"].as_i64().unwrap();

    // Apply with slave_id_override = 42
    let app2 = rebuild_template_app(pool.clone()).await;
    let apply_body = json!({ "clear_existing": true, "slave_id_override": 42 });
    let resp2 = send_json_request(
        app2,
        "POST",
        &format!("/api/templates/{}/apply/5001", template_id),
        Some(apply_body),
    )
    .await;
    assert_eq!(resp2.status(), StatusCode::OK);

    // Verify slave_id was overridden in DB
    let row: (Option<String>,) = sqlx::query_as(
        "SELECT protocol_mappings FROM telemetry_points WHERE channel_id = ? AND point_id = ?",
    )
    .bind(5001_i64)
    .bind(1_i64)
    .fetch_one(&pool)
    .await
    .unwrap();

    let mapping: serde_json::Value = serde_json::from_str(&row.0.unwrap()).unwrap();
    assert_eq!(mapping["slave_id"], 42);
    assert_eq!(mapping["register_address"], 100);
}

// ========================================================================
// Governed channel-management HTTP boundary tests
// ========================================================================

#[tokio::test]
async fn channel_management_logger_does_not_consume_large_chunked_json() {
    let mutator = RecordingChannelMutator::successful(None);
    let app = recording_channel_router(Arc::clone(&mutator)).await;
    let credential = "sensitive-device-credential".repeat(180);
    let body = json!({
        "channel_id": 7,
        "name": "large commissioning body",
        "protocol": "virtual",
        "parameters": {"credential": credential}
    });
    assert!(body.to_string().len() > 2_048);
    let request = Request::builder()
        .method("POST")
        .uri("/api/channels")
        .header("content-type", "application/json")
        .header("authorization", format!("Bearer {ADMIN_ACCESS_TOKEN}"))
        .header("x-aether-confirmed", "true")
        // Intentionally omit Content-Length to exercise chunked semantics.
        .body(Body::from(body.to_string()))
        .unwrap();

    let response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(mutator.mutation_count(), 1);
}

fn governed_channel_request(
    method: &str,
    uri: &str,
    body: Option<serde_json::Value>,
    authenticated: bool,
    confirmed: bool,
    expected_revision: Option<&str>,
) -> Request<Body> {
    let mut request = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json")
        .header("x-request-id", "018f4f04-0db8-7c6c-84ab-4b8457d8d385")
        .header("x-aether-confirmed", confirmed.to_string());
    if authenticated {
        request = request.header("authorization", format!("Bearer {ADMIN_ACCESS_TOKEN}"));
    }
    if let Some(revision) = expected_revision {
        request = request.header("x-aether-expected-revision", revision);
    }
    request
        .body(body.map_or_else(Body::empty, |body| Body::from(body.to_string())))
        .unwrap()
}

fn governed_reconciliation_request(uri: &str) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(uri)
        .header("authorization", format!("Bearer {ADMIN_ACCESS_TOKEN}"))
        .header("x-request-id", TEST_REQUEST_ID)
        .header("x-aether-confirmed", "true")
        .body(Body::empty())
        .unwrap()
}

fn reconciliation_items() -> Vec<ChannelReconciliationItem> {
    vec![
        ChannelReconciliationItem::new(
            aether_domain::ChannelId::new(8),
            ChannelDesiredStateObservation::absent(Some(ChannelRevision::new(4))),
            ChannelRuntimeProjection::Removed,
        ),
        ChannelReconciliationItem::new(
            aether_domain::ChannelId::new(7),
            ChannelDesiredStateObservation::present(ChannelRevision::new(3), true),
            ChannelRuntimeProjection::Active,
        ),
    ]
}

#[tokio::test]
async fn canonical_compatibility_and_single_channel_reconciliation_share_one_application() {
    let reconciler = RecordingChannelReconciler::successful(reconciliation_items());
    let app = recording_reconciliation_router(
        Arc::clone(&reconciler),
        Arc::new(aether_store_local::MemoryAuditSink::new()),
    )
    .await;

    for path in ["/api/channels/reconcile", "/api/channels/reload"] {
        let response = app
            .clone()
            .oneshot(governed_reconciliation_request(path))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "{path}");
        let payload = extract_json(response).await;
        assert_eq!(payload["success"], true);
        assert_eq!(payload["data"]["request_id"], TEST_REQUEST_ID);
        assert_eq!(payload["data"]["scope"], "all");
        assert_eq!(payload["data"]["channel_id"], serde_json::Value::Null);
        assert_eq!(payload["data"]["degraded_count"], 0);
        assert_eq!(payload["data"]["reconciliation_required"], false);
        assert_eq!(payload["data"]["completion_audit"]["status"], "recorded");
        assert_eq!(payload["data"]["retryable"], false);
        assert_eq!(payload["data"]["items"][0]["channel_id"], 7);
        assert_eq!(payload["data"]["items"][0]["desired"]["status"], "present");
        assert_eq!(payload["data"]["items"][0]["desired"]["revision"], 3);
        assert_eq!(payload["data"]["items"][0]["desired"]["enabled"], true);
        assert_eq!(payload["data"]["items"][0]["runtime_projection"], "active");
        assert_eq!(payload["data"]["items"][1]["channel_id"], 8);
        assert_eq!(payload["data"]["items"][1]["desired"]["status"], "absent");
        assert_eq!(payload["data"]["items"][1]["desired"]["last_revision"], 4);
        let serialized = payload.to_string().to_ascii_lowercase();
        for secret_bearing_field in ["parameters", "logging", "config", "credential"] {
            assert!(!serialized.contains(secret_bearing_field));
        }
    }

    let response = app
        .oneshot(governed_reconciliation_request("/api/channels/7/reconcile"))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let payload = extract_json(response).await;
    assert_eq!(payload["data"]["scope"], "one");
    assert_eq!(payload["data"]["channel_id"], 7);
    assert_eq!(payload["data"]["items"].as_array().unwrap().len(), 1);
    assert_eq!(payload["data"]["items"][0]["channel_id"], 7);

    assert_eq!(
        reconciler.scopes(),
        vec![
            ChannelReconciliationScope::All,
            ChannelReconciliationScope::All,
            ChannelReconciliationScope::One(aether_domain::ChannelId::new(7)),
        ]
    );
}

#[tokio::test]
async fn channel_reconciliation_requires_bearer_confirmation_and_explicit_request_id() {
    let reconciler = RecordingChannelReconciler::successful(reconciliation_items());
    let app = recording_reconciliation_router(
        Arc::clone(&reconciler),
        Arc::new(aether_store_local::MemoryAuditSink::new()),
    )
    .await;

    let missing_bearer = Request::builder()
        .method("POST")
        .uri("/api/channels/reconcile")
        .header("x-request-id", TEST_REQUEST_ID)
        .header("x-aether-confirmed", "true")
        .body(Body::empty())
        .unwrap();
    assert_eq!(
        app.clone().oneshot(missing_bearer).await.unwrap().status(),
        StatusCode::FORBIDDEN
    );

    let missing_confirmation = Request::builder()
        .method("POST")
        .uri("/api/channels/reload")
        .header("authorization", format!("Bearer {ADMIN_ACCESS_TOKEN}"))
        .header("x-request-id", TEST_REQUEST_ID)
        .body(Body::empty())
        .unwrap();
    assert_eq!(
        app.clone()
            .oneshot(missing_confirmation)
            .await
            .unwrap()
            .status(),
        StatusCode::UNPROCESSABLE_ENTITY
    );

    let missing_request_id = Request::builder()
        .method("POST")
        .uri("/api/channels/7/reconcile")
        .header("authorization", format!("Bearer {ADMIN_ACCESS_TOKEN}"))
        .header("x-aether-confirmed", "true")
        .body(Body::empty())
        .unwrap();
    assert_eq!(
        app.clone()
            .oneshot(missing_request_id)
            .await
            .unwrap()
            .status(),
        StatusCode::BAD_REQUEST
    );

    let invalid_channel_id =
        governed_reconciliation_request("/api/channels/not-a-number/reconcile");
    assert_eq!(
        app.oneshot(invalid_channel_id).await.unwrap().status(),
        StatusCode::BAD_REQUEST
    );
    assert!(reconciler.scopes().is_empty());
}

#[tokio::test]
async fn channel_reconciliation_failures_and_terminal_audit_are_sanitized() {
    let pre_audit_reconciler = RecordingChannelReconciler::successful(reconciliation_items());
    let pre_audit_app = recording_reconciliation_router(
        Arc::clone(&pre_audit_reconciler),
        Arc::new(UnavailableAuditSink),
    )
    .await;
    let response = pre_audit_app
        .oneshot(governed_reconciliation_request("/api/channels/reconcile"))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let payload = extract_json(response).await.to_string();
    assert!(!payload.contains("sensitive audit backend detail"));
    assert!(pre_audit_reconciler.scopes().is_empty());

    let port_failure = RecordingChannelReconciler::failing(PortErrorKind::Unavailable);
    let port_app = recording_reconciliation_router(
        Arc::clone(&port_failure),
        Arc::new(aether_store_local::MemoryAuditSink::new()),
    )
    .await;
    let response = port_app
        .oneshot(governed_reconciliation_request("/api/channels/reconcile"))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let payload = extract_json(response).await.to_string();
    assert!(!payload.contains("sensitive protocol credential"));
    assert_eq!(port_failure.scopes(), vec![ChannelReconciliationScope::All]);

    let terminal_reconciler = RecordingChannelReconciler::successful(reconciliation_items());
    let terminal_app = recording_reconciliation_router(
        Arc::clone(&terminal_reconciler),
        Arc::new(TerminalAuditFailure),
    )
    .await;
    let response = terminal_app
        .oneshot(governed_reconciliation_request("/api/channels/reconcile"))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let payload = extract_json(response).await;
    assert_eq!(payload["data"]["completion_audit"]["status"], "incomplete");
    assert_eq!(payload["data"]["retryable"], false);
    assert!(!payload.to_string().contains("terminal audit unavailable"));
    assert_eq!(
        terminal_reconciler.scopes(),
        vec![ChannelReconciliationScope::All]
    );
}

#[tokio::test]
async fn channel_management_requires_authentication_and_confirmation_before_side_effects() {
    let mutator =
        RecordingChannelMutator::successful(Some(aether_ports::ChannelRuntimeProjection::Stopped));
    let app = recording_channel_router(Arc::clone(&mutator)).await;
    let body = json!({
        "channel_id": 7,
        "name": "Packaging PLC",
        "protocol": "virtual",
        "parameters": {}
    });

    let missing_auth = app
        .clone()
        .oneshot(governed_channel_request(
            "POST",
            "/api/channels",
            Some(body.clone()),
            false,
            true,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(missing_auth.status(), StatusCode::FORBIDDEN);
    assert!(mutator.mutations().is_empty());

    let missing_confirmation = app
        .oneshot(governed_channel_request(
            "POST",
            "/api/channels",
            Some(body),
            true,
            false,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(
        missing_confirmation.status(),
        StatusCode::UNPROCESSABLE_ENTITY
    );
    assert!(mutator.mutations().is_empty());
}

#[tokio::test]
async fn invalid_channel_http_inputs_never_reach_the_mutator() {
    let mutator =
        RecordingChannelMutator::successful(Some(aether_ports::ChannelRuntimeProjection::Stopped));
    let app = recording_channel_router(Arc::clone(&mutator)).await;

    for request in [
        Request::builder()
            .method("POST")
            .uri("/api/channels")
            .header("authorization", format!("Bearer {ADMIN_ACCESS_TOKEN}"))
            .header("x-aether-confirmed", "true")
            .body(Body::from(r#"{"name":"missing content type"}"#))
            .unwrap(),
        Request::builder()
            .method("POST")
            .uri("/api/channels")
            .header("content-type", "application/json")
            .header("authorization", format!("Bearer {ADMIN_ACCESS_TOKEN}"))
            .header("x-aether-confirmed", "true")
            .body(Body::from("{invalid-json"))
            .unwrap(),
        governed_channel_request(
            "POST",
            "/api/channels",
            Some(json!({
                "channel_id": 7,
                "name": "cannot compare a create",
                "protocol": "virtual",
                "parameters": {}
            })),
            true,
            true,
            Some("1"),
        ),
        governed_channel_request(
            "PUT",
            "/api/channels/not-a-number",
            Some(json!({"name": "renamed"})),
            true,
            true,
            None,
        ),
        governed_channel_request(
            "PUT",
            "/api/channels/7",
            Some(json!({"name": "renamed"})),
            true,
            true,
            Some("not-a-revision"),
        ),
        governed_channel_request(
            "PUT",
            "/api/channels/7",
            Some(json!({"name": "renamed"})),
            true,
            true,
            Some("0"),
        ),
        governed_channel_request(
            "PUT",
            "/api/channels/10000",
            Some(json!({"name": "renamed"})),
            true,
            true,
            None,
        ),
        governed_channel_request("PUT", "/api/channels/7", Some(json!({})), true, true, None),
        governed_channel_request(
            "PUT",
            "/api/channels/7",
            Some(json!({"channel_id": 8, "name": "renamed"})),
            true,
            true,
            None,
        ),
    ] {
        let response = app.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }
    assert!(mutator.mutations().is_empty());
}

#[tokio::test]
async fn channel_application_errors_have_stable_http_statuses_without_internal_details() {
    for (kind, expected_status) in [
        (PortErrorKind::InvalidData, StatusCode::BAD_REQUEST),
        (PortErrorKind::NotFound, StatusCode::NOT_FOUND),
        (PortErrorKind::Rejected, StatusCode::CONFLICT),
        (PortErrorKind::Conflict, StatusCode::CONFLICT),
        (PortErrorKind::Unavailable, StatusCode::SERVICE_UNAVAILABLE),
        (PortErrorKind::Timeout, StatusCode::GATEWAY_TIMEOUT),
        (PortErrorKind::Permanent, StatusCode::INTERNAL_SERVER_ERROR),
    ] {
        let app = recording_channel_router(RecordingChannelMutator::failing(kind)).await;
        let response = app
            .oneshot(channel_mutation_request(
                "PUT",
                "/api/channels/7",
                Some(json!({"name": "Packaging PLC"})),
            ))
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            expected_status,
            "unexpected status for {kind:?}"
        );
        let payload = extract_json(response).await.to_string();
        assert!(
            !payload.contains("test failure"),
            "{kind:?} leaked adapter detail"
        );
    }

    let mutator = RecordingChannelMutator::successful(None);
    let app =
        recording_channel_router_with_audit(Arc::clone(&mutator), Arc::new(UnavailableAuditSink))
            .await;
    let response = app
        .oneshot(channel_mutation_request(
            "POST",
            "/api/channels",
            Some(json!({
                "channel_id": 7,
                "name": "Packaging PLC",
                "protocol": "virtual",
                "parameters": {}
            })),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(mutator.mutation_count(), 0);
    assert!(
        !extract_json(response)
            .await
            .to_string()
            .contains("sensitive audit backend detail")
    );
}

#[tokio::test]
async fn confirmed_channel_requests_forward_exact_typed_mutations() {
    use aether_ports::{
        ChannelDefinition, ChannelLoggingPolicy, ChannelMutation, ChannelParameterValue,
        ChannelPatch, ChannelRevision,
    };

    let mutator =
        RecordingChannelMutator::successful(Some(aether_ports::ChannelRuntimeProjection::Active));
    let app = recording_channel_router(Arc::clone(&mutator)).await;

    let requests = [
        governed_channel_request(
            "POST",
            "/api/channels",
            Some(json!({
                "channel_id": 7,
                "name": "Packaging PLC",
                "description": "Line one",
                "protocol": "modbus_tcp",
                "parameters": {"port": 502},
                "logging": {"enabled": true, "level": "debug", "file": "channel.log"}
            })),
            true,
            true,
            None,
        ),
        governed_channel_request(
            "PUT",
            "/api/channels/7",
            Some(json!({
                "name": "Packaging PLC 2",
                "parameters": {"timeout_ms": 1000},
                "logging": {"enabled": false, "level": null, "file": null}
            })),
            true,
            true,
            Some("3"),
        ),
        governed_channel_request(
            "PUT",
            "/api/channels/7/enabled",
            Some(json!({"enabled": true})),
            true,
            true,
            Some("4"),
        ),
        governed_channel_request("DELETE", "/api/channels/7", None, true, true, Some("5")),
    ];

    for request in requests {
        assert_eq!(
            app.clone().oneshot(request).await.unwrap().status(),
            StatusCode::OK
        );
    }

    let expected_create = ChannelDefinition::new(
        Some(aether_domain::ChannelId::new(7)),
        "Packaging PLC",
        "modbus_tcp",
        std::collections::BTreeMap::from([(
            "port".to_string(),
            ChannelParameterValue::Integer(502),
        )]),
    )
    .with_description("Line one")
    .with_logging(
        ChannelLoggingPolicy::default()
            .with_enabled(true)
            .with_level("debug")
            .with_file("channel.log"),
    );
    let expected_update = ChannelPatch::new()
        .with_name("Packaging PLC 2")
        .with_parameters(std::collections::BTreeMap::from([(
            "timeout_ms".to_string(),
            ChannelParameterValue::Integer(1000),
        )]))
        .with_logging(ChannelLoggingPolicy::default());
    assert_eq!(
        mutator.mutations(),
        vec![
            ChannelMutation::create(expected_create),
            ChannelMutation::update_with_revision(
                aether_domain::ChannelId::new(7),
                ChannelRevision::new(3),
                expected_update,
            ),
            ChannelMutation::enable_with_revision(
                aether_domain::ChannelId::new(7),
                ChannelRevision::new(4),
            ),
            ChannelMutation::delete_with_revision(
                aether_domain::ChannelId::new(7),
                ChannelRevision::new(5),
            ),
        ]
    );
}

#[tokio::test]
async fn degraded_and_terminal_audit_incomplete_are_explicit_non_retryable_acceptances() {
    for (fail_terminal_audit, expected_audit) in [(false, "recorded"), (true, "incomplete")] {
        let mutator = RecordingChannelMutator::successful(Some(
            aether_ports::ChannelRuntimeProjection::Degraded,
        ));
        let audit: Arc<dyn AuditSink> = if fail_terminal_audit {
            Arc::new(TerminalAuditFailure)
        } else {
            Arc::new(aether_store_local::MemoryAuditSink::new())
        };
        let app = recording_channel_router_with_audit(mutator, audit).await;
        let response = app
            .oneshot(governed_channel_request(
                "PUT",
                "/api/channels/7/enabled",
                Some(json!({"enabled": true})),
                true,
                true,
                Some("8"),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let payload = extract_json(response).await;
        assert_eq!(payload["data"]["runtime_projection"], "degraded");
        assert_eq!(payload["data"]["reconciliation_required"], true);
        assert_eq!(
            payload["data"]["completion_audit"]["status"],
            expected_audit
        );
        assert_eq!(payload["data"]["completion_audit"]["retryable"], false);
        assert_eq!(payload["data"]["retryable"], false);
    }
}

#[tokio::test]
async fn legacy_route_constructor_fails_closed_for_channel_mutations() {
    let pool = create_test_sqlite_pool().await;
    let manager = Arc::new(
        ChannelManager::new(
            crate::test_utils::create_test_shm_handle(),
            crate::test_utils::create_test_routing_cache(),
        )
        .unwrap(),
    );
    let app = create_api_routes(
        manager,
        pool.clone(),
        Arc::new(crate::api::command_cache::CommandTxCache::new()),
    );
    let response = app
        .oneshot(governed_channel_request(
            "POST",
            "/api/channels",
            Some(json!({
                "channel_id": 91,
                "name": "Must Not Be Created",
                "protocol": "virtual",
                "parameters": {}
            })),
            true,
            true,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM channels WHERE channel_id = 91")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
}

// ========================================================================
// OpenAPI Spec Completeness Tests
// ========================================================================

#[cfg(feature = "openapi")]
mod openapi_tests {
    use crate::api::routes::IoApiDoc;
    use utoipa::OpenApi;

    fn spec() -> serde_json::Value {
        serde_json::to_value(IoApiDoc::openapi()).expect("serialize io OpenAPI document")
    }

    fn assert_path_methods(
        paths: &serde_json::Map<String, serde_json::Value>,
        path: &str,
        methods: &[&str],
    ) {
        let path_item = paths
            .get(path)
            .unwrap_or_else(|| panic!("missing OpenAPI path: {path}"));
        for method in methods {
            assert!(
                path_item[*method].is_object(),
                "OpenAPI path {path} is missing {method}"
            );
        }
    }

    fn schema_property<'a>(
        schema: &'a serde_json::Value,
        property: &str,
    ) -> Option<&'a serde_json::Value> {
        schema
            .get("properties")
            .and_then(|properties| properties.get(property))
            .or_else(|| {
                schema
                    .get("allOf")
                    .and_then(serde_json::Value::as_array)
                    .and_then(|items| {
                        items
                            .iter()
                            .find_map(|item| schema_property(item, property))
                    })
            })
    }

    #[test]
    fn test_openapi_spec_generates_without_panic() {
        let doc = IoApiDoc::openapi();
        let json = doc.to_pretty_json().unwrap();
        assert!(!json.is_empty());
    }

    #[test]
    fn test_openapi_metadata_matches_io_service() {
        let spec = spec();

        assert_eq!(spec["info"]["title"], "Aether I/O Service API");
        assert_eq!(spec["info"]["version"], env!("CARGO_PKG_VERSION"));
    }

    #[test]
    fn test_openapi_examples_are_industry_neutral() {
        let serialized = serde_json::to_string(&spec())
            .expect("I/O OpenAPI document should serialize")
            .to_ascii_lowercase();

        for energy_pack_identity in [
            "pv inverter",
            "battery bms",
            "pcs modbus",
            "diesel generator",
            "pcs#",
            "bams",
            "power converter",
            "soc,",
        ] {
            assert!(
                !serialized.contains(energy_pack_identity),
                "Kernel Swagger must not embed Energy Pack identity {energy_pack_identity}"
            );
        }
    }

    #[test]
    fn channel_create_openapi_documents_disabled_as_the_default() {
        let specification = spec();
        let enabled = &specification["components"]["schemas"]["ChannelCreateRequest"]["properties"]
            ["enabled"];

        assert_eq!(enabled["default"], false);
        assert_eq!(enabled["example"], false);
        let examples = &specification["paths"]["/api/channels"]["post"]["requestBody"]["content"]["application/json"]
            ["examples"];
        for (name, example) in examples.as_object().expect("channel creation examples") {
            assert_eq!(
                example["value"]["enabled"], false,
                "channel creation example {name:?} must be disabled"
            );
        }
    }

    #[test]
    fn test_openapi_contains_protocol_and_admin_routes() {
        let spec = spec();
        let paths = spec["paths"].as_object().expect("OpenAPI paths object");

        assert_path_methods(paths, "/api/protocols", &["get"]);
        assert_path_methods(paths, "/api/admin/logs/files", &["get"]);
        assert_path_methods(paths, "/api/admin/logs/view", &["get"]);
    }

    #[test]
    fn protocol_discovery_openapi_matches_strict_modbus_configuration() {
        let spec = spec();
        let operation = spec
            .pointer("/paths/~1api~1protocols/get")
            .expect("protocol discovery operation");
        let description = operation["description"]
            .as_str()
            .expect("protocol discovery description")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .to_ascii_lowercase();

        for contract in [
            "host: non-empty string",
            "port: integer 1..65535",
            "device: non-empty string",
            "baud_rate: integer 1..4294967295",
            "poll_interval_ms: integer 1..86400000",
            "read_timeout_ms: integer 1..86400000",
            "no type coercion or fallback",
        ] {
            assert!(
                description.contains(contract),
                "protocol discovery must state the exact Modbus contract: {contract}"
            );
        }

        let response_ref = operation
            .pointer("/responses/200/content/application~1json/schema/$ref")
            .and_then(serde_json::Value::as_str)
            .expect("typed protocol discovery success envelope");
        assert!(
            response_ref.contains("SuccessResponse"),
            "protocol discovery must document its actual SuccessResponse envelope"
        );

        let parameter_schema = spec
            .pointer("/components/schemas/ParameterInfo/properties")
            .expect("protocol parameter schema");
        for constraint in ["minimum", "maximum", "min_length"] {
            assert!(
                parameter_schema.get(constraint).is_some(),
                "ParameterInfo must expose {constraint} for dynamic forms"
            );
        }

        let create_parameters = spec
            .pointer("/components/schemas/ChannelCreateRequest/properties/parameters/description")
            .and_then(serde_json::Value::as_str)
            .expect("channel-create parameter description")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .to_ascii_lowercase();
        assert!(create_parameters.contains("no type coercion or fallback"));
        assert!(create_parameters.contains("port: integer 1..65535"));
        assert!(create_parameters.contains("baud_rate: integer 1..4294967295"));

        let create_examples = spec
            .pointer("/paths/~1api~1channels/post/requestBody/content/application~1json/examples")
            .and_then(serde_json::Value::as_object)
            .expect("channel-create examples");
        for (name, example) in create_examples {
            let parameters = example["value"]["parameters"]
                .as_object()
                .unwrap_or_else(|| panic!("channel-create example {name} parameters"));
            assert!(parameters["host"].is_string());
            assert!(parameters["port"].is_u64());
            for parameter in parameters.keys() {
                assert!(
                    ["host", "port", "read_timeout_ms", "poll_interval_ms"]
                        .contains(&parameter.as_str()),
                    "channel-create example {name} advertises ignored parameter {parameter}"
                );
            }
        }
    }

    #[test]
    fn test_openapi_simulation_write_matches_the_fail_closed_runtime_gate() {
        let spec = spec();
        let operation = spec
            .pointer("/paths/~1api~1channels~1{channel_id}~1write/post")
            .expect("simulation write operation");

        for status in ["200", "400", "403", "500"] {
            assert!(
                operation.pointer(&format!("/responses/{status}")).is_some(),
                "simulation write is missing response {status}"
            );
        }
        let description = operation["description"]
            .as_str()
            .expect("simulation write description");
        assert!(description.contains("AETHER_ALLOW_SIMULATION_WRITES=true"));
        assert!(description.contains("C/A device commands are always rejected"));
    }

    #[test]
    fn channel_management_openapi_is_the_governed_application_contract() {
        let spec = spec();

        assert_eq!(
            spec.pointer("/components/securitySchemes/bearer_auth/type")
                .and_then(serde_json::Value::as_str),
            Some("http")
        );
        assert_eq!(
            spec.pointer("/components/securitySchemes/bearer_auth/scheme")
                .and_then(serde_json::Value::as_str),
            Some("bearer")
        );

        for (pointer, request_schema, statuses, has_revision_header) in [
            (
                "/paths/~1api~1channels/post",
                "ChannelCreateRequest",
                &["200", "400", "403", "409", "422", "500", "503", "504"][..],
                false,
            ),
            (
                "/paths/~1api~1channels~1{id}/put",
                "ChannelConfigUpdateRequest",
                &[
                    "200", "400", "403", "404", "409", "422", "500", "503", "504",
                ][..],
                true,
            ),
            (
                "/paths/~1api~1channels~1{id}/delete",
                "",
                &[
                    "200", "400", "403", "404", "409", "422", "500", "503", "504",
                ][..],
                true,
            ),
            (
                "/paths/~1api~1channels~1{id}~1enabled/put",
                "ChannelEnabledRequest",
                &[
                    "200", "400", "403", "404", "409", "422", "500", "503", "504",
                ][..],
                true,
            ),
        ] {
            let operation = spec
                .pointer(pointer)
                .unwrap_or_else(|| panic!("missing channel management operation {pointer}"));

            let security = operation["security"]
                .as_array()
                .expect("channel mutation security array");
            assert_eq!(security.len(), 1, "{pointer} accepts only Bearer JWTs");
            assert!(
                security[0].get("bearer_auth").is_some(),
                "{pointer} must require bearer_auth"
            );

            for header in ["x-request-id", "x-aether-confirmed"] {
                let parameter = operation["parameters"]
                    .as_array()
                    .and_then(|parameters| {
                        parameters.iter().find(|parameter| {
                            parameter["name"] == header && parameter["in"] == "header"
                        })
                    })
                    .unwrap_or_else(|| panic!("{pointer} must document {header}"));
                if header == "x-request-id" {
                    assert_eq!(parameter["schema"]["format"], "uuid", "{pointer}");
                }
            }
            let has_documented_revision =
                operation["parameters"]
                    .as_array()
                    .is_some_and(|parameters| {
                        parameters.iter().any(|parameter| {
                            parameter["name"] == "x-aether-expected-revision"
                                && parameter["in"] == "header"
                        })
                    });
            assert_eq!(
                has_documented_revision, has_revision_header,
                "{pointer} revision-header documentation must match resource semantics"
            );
            if has_revision_header {
                let revision_parameter = operation["parameters"]
                    .as_array()
                    .and_then(|parameters| {
                        parameters.iter().find(|parameter| {
                            parameter["name"] == "x-aether-expected-revision"
                                && parameter["in"] == "header"
                        })
                    })
                    .expect("expected-revision parameter");
                assert_eq!(revision_parameter["schema"]["minimum"], 1);
                assert_eq!(
                    revision_parameter["schema"]["maximum"],
                    9_223_372_036_854_775_807_u64
                );

                let channel_id_parameter = operation["parameters"]
                    .as_array()
                    .and_then(|parameters| {
                        parameters.iter().find(|parameter| {
                            parameter["name"] == "id" && parameter["in"] == "path"
                        })
                    })
                    .expect("channel ID path parameter");
                assert_eq!(channel_id_parameter["schema"]["maximum"], 9999);
            }

            for status in statuses {
                assert!(
                    operation.pointer(&format!("/responses/{status}")).is_some(),
                    "{pointer} must document HTTP {status}"
                );
            }

            let accepted_description = operation
                .pointer("/responses/200/description")
                .and_then(serde_json::Value::as_str)
                .expect("accepted mutation semantics");
            assert!(
                accepted_description
                    .contains("reported with request_id for operator reconciliation")
            );
            assert!(accepted_description.contains("do not retry automatically"));
            assert!(!accepted_description.contains("is reconciled by request_id"));

            let response_ref = operation
                .pointer("/responses/200/content/application~1json/schema/$ref")
                .and_then(serde_json::Value::as_str)
                .expect("typed channel mutation success envelope");
            assert!(response_ref.ends_with("ChannelMutationResponse"));

            if !request_schema.is_empty() {
                let request_ref = operation
                    .pointer("/requestBody/content/application~1json/schema/$ref")
                    .and_then(serde_json::Value::as_str)
                    .expect("typed channel mutation request body");
                assert!(request_ref.ends_with(request_schema));
            }
        }

        assert!(
            spec.pointer("/paths/~1api~1channels/post/responses/404")
                .is_none(),
            "create cannot report an existing target as not found"
        );

        let create_channel_id = spec
            .pointer("/components/schemas/ChannelCreateRequest/properties/channel_id")
            .expect("optional create channel ID schema");
        assert_eq!(create_channel_id["maximum"], 9999);
        let create_channel_id_description = create_channel_id["description"]
            .as_str()
            .expect("automatic channel ID allocation description")
            .to_ascii_lowercase();
        assert!(create_channel_id_description.contains("lowest id"));
        assert!(create_channel_id_description.contains("revision tombstones"));
        assert!(!create_channel_id_description.contains("max+1"));

        let update_channel_id = spec
            .pointer("/components/schemas/ChannelConfigUpdateRequest/properties/channel_id/maximum")
            .expect("update compatibility channel ID maximum");
        assert_eq!(update_channel_id, 9999);

        let update = spec
            .pointer("/paths/~1api~1channels~1{id}/put")
            .expect("channel update operation");
        let update_description = update["description"]
            .as_str()
            .expect("channel update description")
            .to_ascii_lowercase();
        assert!(update_description.contains("patch semantics"));
        assert!(update_description.contains("identity migration is forbidden"));

        let receipt = spec
            .pointer("/components/schemas/ChannelMutationResult/properties")
            .expect("channel mutation receipt schema");
        for field in [
            "request_id",
            "operation",
            "resulting_revision",
            "desired_enabled",
            "runtime_projection",
            "reconciliation_required",
            "completion_audit",
            "retryable",
        ] {
            assert!(receipt.get(field).is_some(), "receipt is missing {field}");
        }
        assert_eq!(receipt["request_id"]["format"], "uuid");
        for field in ["id", "channel_id"] {
            assert_eq!(receipt[field]["maximum"], 9999);
        }
        assert_eq!(receipt["resulting_revision"]["minimum"], 1);
        assert_eq!(
            receipt["resulting_revision"]["maximum"],
            9_223_372_036_854_775_807_u64
        );
        let success_description = update
            .pointer("/responses/200/description")
            .and_then(serde_json::Value::as_str)
            .expect("accepted outcome semantics");
        assert!(success_description.contains("must not be retried automatically"));
        assert!(success_description.contains("degraded"));

        for schema in ["ChannelStatusResponse", "ChannelDetail"] {
            let revision = schema_property(&spec["components"]["schemas"][schema], "revision")
                .unwrap_or_else(|| panic!("{schema} must expose desired-state revision"));
            assert_eq!(revision["type"], "integer");
            assert_eq!(revision["minimum"], 1);
            assert_eq!(revision["maximum"], 9_223_372_036_854_775_807_u64);
        }
    }

    #[test]
    fn channel_reconciliation_openapi_matches_the_governed_runtime_contract() {
        let spec = spec();

        for pointer in [
            "/paths/~1api~1channels~1reconcile/post",
            "/paths/~1api~1channels~1{id}~1reconcile/post",
            "/paths/~1api~1channels~1reload/post",
        ] {
            let operation = spec
                .pointer(pointer)
                .unwrap_or_else(|| panic!("missing channel reconciliation operation {pointer}"));
            assert!(operation["security"][0].get("bearer_auth").is_some());

            for header in ["x-request-id", "x-aether-confirmed"] {
                let parameter = operation["parameters"]
                    .as_array()
                    .and_then(|parameters| {
                        parameters.iter().find(|parameter| {
                            parameter["name"] == header && parameter["in"] == "header"
                        })
                    })
                    .unwrap_or_else(|| panic!("{pointer} must document {header}"));
                assert_eq!(parameter["required"], true, "{pointer} {header}");
                if header == "x-request-id" {
                    assert_eq!(parameter["schema"]["format"], "uuid", "{pointer}");
                }
            }

            for status in ["200", "400", "403", "409", "422", "500", "503", "504"] {
                assert!(
                    operation.pointer(&format!("/responses/{status}")).is_some(),
                    "{pointer} must document HTTP {status}"
                );
            }
            let response_ref = operation
                .pointer("/responses/200/content/application~1json/schema/$ref")
                .and_then(serde_json::Value::as_str)
                .expect("typed reconciliation response");
            assert!(response_ref.ends_with("ChannelReconciliationResponse"));
            assert!(operation.get("requestBody").is_none());

            let description = operation
                .pointer("/responses/200/description")
                .and_then(serde_json::Value::as_str)
                .expect("accepted reconciliation semantics");
            assert!(description.contains("non-idempotent"));
            assert!(description.contains("do not retry automatically"));
        }

        let one = spec
            .pointer("/paths/~1api~1channels~1{id}~1reconcile/post")
            .expect("single-channel reconciliation");
        assert!(
            one["responses"].get("404").is_none(),
            "an absent desired channel is a successful fencing receipt, not not-found"
        );
        assert!(
            one["responses"]["200"]["description"]
                .as_str()
                .is_some_and(|description| description.contains("absent"))
        );
        let channel_id = one["parameters"]
            .as_array()
            .and_then(|parameters| {
                parameters
                    .iter()
                    .find(|parameter| parameter["name"] == "id" && parameter["in"] == "path")
            })
            .expect("single-channel ID");
        assert_eq!(channel_id["schema"]["maximum"], 9999);

        let receipt = spec
            .pointer("/components/schemas/ChannelReconciliationResult/properties")
            .expect("channel reconciliation receipt schema");
        for field in [
            "request_id",
            "scope",
            "channel_id",
            "items",
            "degraded_count",
            "reconciliation_required",
            "completion_audit",
            "retryable",
        ] {
            assert!(receipt.get(field).is_some(), "receipt is missing {field}");
        }
        assert_eq!(receipt["request_id"]["format"], "uuid");

        let reload = spec
            .pointer("/paths/~1api~1channels~1reload/post")
            .expect("compatibility reload alias");
        assert_eq!(reload["deprecated"], true);
        assert!(
            reload["description"]
                .as_str()
                .is_some_and(|description| description.contains("/api/channels/reconcile"))
        );
        let serialized = serde_json::to_string(
            spec.pointer("/components/schemas/ChannelReconciliationItemResult")
                .expect("sanitized reconciliation item schema"),
        )
        .unwrap()
        .to_ascii_lowercase();
        for forbidden in ["parameters", "logging", "config", "credential"] {
            assert!(!serialized.contains(forbidden));
        }
    }

    #[test]
    fn channel_control_openapi_is_a_governed_application_contract() {
        let spec = spec();
        let operation = spec
            .pointer("/paths/~1api~1channels~1{id}~1control/post")
            .expect("channel control operation");

        assert!(operation["security"][0].get("bearer_auth").is_some());
        for header in ["x-request-id", "x-aether-confirmed"] {
            let parameter = operation["parameters"]
                .as_array()
                .and_then(|parameters| {
                    parameters.iter().find(|parameter| {
                        parameter["name"] == header && parameter["in"] == "header"
                    })
                })
                .unwrap_or_else(|| panic!("channel control must document {header}"));
            assert_eq!(parameter["required"], true, "{header}");
            if header == "x-request-id" {
                assert_eq!(parameter["schema"]["format"], "uuid");
            }
        }
        let channel_id = operation["parameters"]
            .as_array()
            .and_then(|parameters| {
                parameters
                    .iter()
                    .find(|parameter| parameter["name"] == "id" && parameter["in"] == "path")
            })
            .expect("channel control ID");
        assert_eq!(channel_id["schema"]["maximum"], 9999);

        for status in [
            "200", "400", "403", "404", "409", "422", "500", "503", "504",
        ] {
            assert!(
                operation.pointer(&format!("/responses/{status}")).is_some(),
                "channel control must document HTTP {status}"
            );
        }
        let response_ref = operation
            .pointer("/responses/200/content/application~1json/schema/$ref")
            .and_then(serde_json::Value::as_str)
            .expect("typed channel control response");
        assert!(response_ref.ends_with("ChannelControlResponse"));
        let request_ref = operation
            .pointer("/requestBody/content/application~1json/schema/$ref")
            .and_then(serde_json::Value::as_str)
            .expect("typed channel control request");
        assert!(request_ref.ends_with("ChannelOperation"));
        let operation_kind_ref = spec
            .pointer("/components/schemas/ChannelOperation/properties/operation/$ref")
            .and_then(serde_json::Value::as_str)
            .expect("strongly typed channel operation enum");
        assert!(operation_kind_ref.ends_with("ChannelOperationKind"));
        let operation_values = spec
            .pointer("/components/schemas/ChannelOperationKind/enum")
            .and_then(serde_json::Value::as_array)
            .expect("channel operation enum values");
        assert_eq!(operation_values.len(), 3);
        for (actual, expected) in operation_values.iter().zip(["start", "stop", "restart"]) {
            assert_eq!(actual, expected);
        }

        let accepted = operation
            .pointer("/responses/200/description")
            .and_then(serde_json::Value::as_str)
            .expect("channel control acceptance semantics");
        assert!(accepted.contains("non-idempotent"));
        assert!(accepted.contains("do not retry automatically"));

        let receipt = spec
            .pointer("/components/schemas/ChannelControlResult/properties")
            .expect("channel control receipt schema");
        for field in [
            "channel_id",
            "request_id",
            "operation",
            "desired_revision",
            "desired_enabled",
            "runtime_projection",
            "reconciliation_required",
            "completion_audit",
            "retryable",
        ] {
            assert!(receipt.get(field).is_some(), "receipt is missing {field}");
        }
        assert_eq!(receipt["request_id"]["format"], "uuid");
    }

    #[test]
    fn test_openapi_point_crud_matches_literal_router_paths() {
        let spec = spec();
        let paths = spec["paths"].as_object().expect("OpenAPI paths object");

        for point_type in ["T", "S", "C", "A"] {
            let path = format!("/api/channels/{{channel_id}}/{point_type}/points/{{point_id}}");
            assert_path_methods(paths, &path, &["get", "post", "put", "delete"]);
        }

        assert!(
            paths.keys().all(|path| !path.ends_with("/config")),
            "OpenAPI must not expose the phantom point /config route"
        );
        assert!(
            !paths.contains_key("/api/channels/{channel_id}/{type}/points/{point_id}"),
            "OpenAPI must use the Router's literal T/S/C/A paths"
        );

        for point_type in ["T", "S", "C", "A"] {
            let operation = &paths
                [&format!("/api/channels/{{channel_id}}/{point_type}/points/{{point_id}}")]["post"];
            assert!(
                operation["responses"]["200"].is_object(),
                "{point_type} point create returns HTTP 200"
            );
            assert!(
                operation["responses"].get("201").is_none(),
                "{point_type} point create must not advertise HTTP 201"
            );
        }
    }

    #[test]
    fn test_openapi_operations_only_use_declared_tags() {
        let spec = spec();
        let declared: std::collections::HashSet<&str> = spec["tags"]
            .as_array()
            .expect("OpenAPI tags array")
            .iter()
            .filter_map(|tag| tag["name"].as_str())
            .collect();

        for (path, item) in spec["paths"].as_object().expect("OpenAPI paths object") {
            for method in ["get", "post", "put", "delete", "patch"] {
                let Some(operation) = item.get(method) else {
                    continue;
                };
                for tag in operation["tags"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(serde_json::Value::as_str)
                {
                    assert!(
                        declared.contains(tag),
                        "{method} {path} uses undeclared tag {tag}"
                    );
                }
            }
        }
    }

    #[test]
    fn test_openapi_http_operation_count_requires_router_parity_review() {
        const HTTP_METHODS: [&str; 8] = [
            "get", "post", "put", "delete", "patch", "options", "head", "trace",
        ];

        let spec = spec();
        let operation_count = spec["paths"]
            .as_object()
            .expect("OpenAPI paths object")
            .values()
            .map(|path_item| {
                HTTP_METHODS
                    .iter()
                    .filter(|method| path_item[**method].is_object())
                    .count()
            })
            .sum::<usize>();

        assert_eq!(
            operation_count, 59,
            "HTTP operation count changed; re-audit Router/OpenAPI parity before updating this guard"
        );
    }

    #[test]
    fn test_openapi_contains_template_paths() {
        let doc = IoApiDoc::openapi();
        let json_str = doc.to_pretty_json().unwrap();
        let spec: serde_json::Value = serde_json::from_str(&json_str).unwrap();

        let paths = spec["paths"].as_object().unwrap();

        // All 5 template path patterns should exist
        assert!(
            paths.contains_key("/api/templates"),
            "Missing /api/templates"
        );
        assert!(
            paths.contains_key("/api/templates/{id}"),
            "Missing /api/templates/{{id}}"
        );
        assert!(
            paths.contains_key("/api/templates/from-channel/{channel_id}"),
            "Missing /api/templates/from-channel/{{channel_id}}"
        );
        assert!(
            paths.contains_key("/api/templates/{id}/apply/{channel_id}"),
            "Missing /api/templates/{{id}}/apply/{{channel_id}}"
        );
    }

    #[test]
    fn test_openapi_template_methods() {
        let doc = IoApiDoc::openapi();
        let json_str = doc.to_pretty_json().unwrap();
        let spec: serde_json::Value = serde_json::from_str(&json_str).unwrap();

        // /api/templates should have GET and POST
        let templates = &spec["paths"]["/api/templates"];
        assert!(templates["get"].is_object(), "/api/templates missing GET");
        assert!(templates["post"].is_object(), "/api/templates missing POST");

        // /api/templates/{id} should have GET, PUT, DELETE
        let templates_id = &spec["paths"]["/api/templates/{id}"];
        assert!(
            templates_id["get"].is_object(),
            "/api/templates/{{id}} missing GET"
        );
        assert!(
            templates_id["put"].is_object(),
            "/api/templates/{{id}} missing PUT"
        );
        assert!(
            templates_id["delete"].is_object(),
            "/api/templates/{{id}} missing DELETE"
        );

        // /api/templates/from-channel/{channel_id} should have POST
        let from_channel = &spec["paths"]["/api/templates/from-channel/{channel_id}"];
        assert!(
            from_channel["post"].is_object(),
            "from-channel missing POST"
        );

        // /api/templates/{id}/apply/{channel_id} should have POST
        let apply = &spec["paths"]["/api/templates/{id}/apply/{channel_id}"];
        assert!(apply["post"].is_object(), "apply missing POST");
    }

    #[test]
    fn test_openapi_contains_template_schemas() {
        let doc = IoApiDoc::openapi();
        let json_str = doc.to_pretty_json().unwrap();
        let spec: serde_json::Value = serde_json::from_str(&json_str).unwrap();

        let schemas = spec["components"]["schemas"].as_object().unwrap();

        let expected = [
            "TemplateListItem",
            "TemplateDetail",
            "CreateTemplateReq",
            "CreateTemplateFromChannelReq",
            "UpdateTemplateReq",
            "ApplyTemplateReq",
            "TemplateListQuery",
        ];

        for name in &expected {
            assert!(
                schemas.contains_key(*name),
                "Missing schema: {}. Available: {:?}",
                name,
                schemas.keys().collect::<Vec<_>>()
            );
        }
    }

    #[test]
    fn test_openapi_templates_tag_exists() {
        let doc = IoApiDoc::openapi();
        let json_str = doc.to_pretty_json().unwrap();
        let spec: serde_json::Value = serde_json::from_str(&json_str).unwrap();

        let tags = spec["tags"].as_array().unwrap();
        let has_templates_tag = tags.iter().any(|t| t["name"] == "templates");
        assert!(has_templates_tag, "Missing 'templates' tag in OpenAPI spec");
    }
}
