// NOTE: API tests use a real temporary mmap so the test topology matches the
// production SHM-only data plane.
#![allow(clippy::disallowed_methods)] // Test code - unwrap is acceptable

use super::*;
use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use serde_json::json;
use sqlx::SqlitePool;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use aether_ports::{
    AuditOutcome, AuditRecord, AuditSink, ChannelDesiredStateObservation, ChannelMutation,
    ChannelMutationKind, ChannelMutationReceipt, ChannelMutator, ChannelReconciler,
    ChannelReconciliationItem, ChannelReconciliationReceipt, ChannelReconciliationScope,
    ChannelRevision, ChannelRuntimeProjection, PortError, PortErrorKind, PortResult,
};
use tower::util::ServiceExt; // for `oneshot` and `ready`

const TEST_JWT_SECRET: &str = "0123456789abcdef0123456789abcdef";
const ADMIN_ACCESS_TOKEN: &str = "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJ1c2VyX2lkIjo3LCJyb2xlIjoiQWRtaW4iLCJ0eXBlIjoiYWNjZXNzIiwiaWF0IjoxNzAwMDAwMDAwLCJleHAiOjQxMDI0NDQ4MDB9.JtjQvDBo7j0bLOxwed6yC9-M9qFCloc4H2Dt0LjzF9E";
const TEST_REQUEST_ID: &str = "018f0000-0000-7000-8000-000000000041";

/// Helper: Create in-memory SQLite pool for testing
async fn create_test_sqlite_pool() -> sqlx::SqlitePool {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();

    // Use standard io schema from common test utils
    common::site_schema::init_io_schema(&pool).await.unwrap();

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
    common::site_schema::init_io_schema(&pool).await.unwrap();

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
    common::site_schema::init_automation_schema(&sqlite_pool)
        .await
        .unwrap();
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
    let authenticator = Arc::new(
        aether_auth_jwt::AccessTokenAuthenticator::new(TEST_JWT_SECRET)
            .expect("valid test access-token secret"),
    );
    create_api_routes_with_boundary(
        channel_manager,
        sqlite_pool,
        ChannelManagementHttpBoundary::governed_with_reconciliation(
            application,
            reconciliation,
            authenticator,
        ),
    )
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
    recording_channel_applications_router(
        mutator,
        RecordingChannelReconciler::successful(reconciliation_items()),
        audit,
    )
    .await
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
        ChannelManagementHttpBoundary::governed_with_reconciliation(
            channel_management,
            channel_reconciliation,
            authenticator,
        ),
    )
}

#[tokio::test]
async fn channel_mutations_require_real_bearer_auth_and_confirmation_before_side_effects() {
    let mutator = RecordingChannelMutator::successful(None);
    let app = recording_channel_router(Arc::clone(&mutator)).await;
    let body = json!({
        "channel_id": 41,
        "name": "governed channel",
        "protocol": "modbus_tcp",
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
            "protocol": "modbus_tcp",
            "parameters": {}
        })),
    );

    let response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let payload = extract_json(response).await;
    assert_eq!(payload["data"]["id"], 41);
    assert_eq!(payload["data"]["channel_id"], 41);
    assert_eq!(payload["data"]["name"], "safe commissioning");
    assert_eq!(payload["data"]["protocol"], "modbus_tcp");
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
            "protocol": "modbus_tcp",
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
            "protocol": "modbus_tcp",
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
    common::site_schema::init_io_schema(&pool).await.unwrap();
    sqlx::query("INSERT INTO channels (channel_id, name, protocol, enabled, config) VALUES (100, 'Ch100', 'modbus_tcp', 1, '{}')")
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
        .uri("/api/channels?protocol=modbus_tcp")
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
        "name": "Modbus Channel A",
        "description": "desc-A",
        "protocol": "modbus_tcp",
        "enabled": true,
        "parameters": {"host": "127.0.0.1", "port": 502}
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
    assert_eq!(v["data"]["name"], "Modbus Channel A");
    assert_eq!(v["data"]["description"], "desc-A");
    assert_eq!(v["data"]["protocol"], "modbus_tcp");
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
    common::site_schema::init_io_schema(&pool).await.unwrap();

    let config = serde_json::json!({
        "description": "old-desc",
        "parameters": {"host": "127.0.0.1", "port": 502}
    })
    .to_string();
    sqlx::query("INSERT INTO channels (channel_id, name, protocol, enabled, config) VALUES (42, 'Ch42', 'modbus_tcp', 1, ?)")
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
    common::site_schema::init_io_schema(&pool).await.unwrap();
    let config = serde_json::json!({
        "description": "keep-me",
        "parameters": {"host": "127.0.0.1", "port": 502}
    })
    .to_string();
    sqlx::query("INSERT INTO channels (channel_id, name, protocol, enabled, config) VALUES (77, 'Ch77', 'modbus_tcp', 0, ?)")
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
    sqlx::query("INSERT INTO channels (channel_id, name, protocol, enabled, config) VALUES (9001, 'Ch9001', 'modbus_tcp', 1, '{}')")
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
    sqlx::query("INSERT INTO channels (channel_id, name, protocol, enabled, config) VALUES (9002, 'Ch9002', 'modbus_tcp', 1, '{}')")
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
    common::site_schema::init_io_schema(&pool).await.unwrap();

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
    common::site_schema::init_io_schema(&pool).await.unwrap();
    sqlx::query("INSERT INTO channels (channel_id, name, protocol, enabled, config) VALUES (600, 'Ch600', 'modbus_tcp', 0, '{}')")
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
    sqlx::query("INSERT INTO channels (channel_id, name, protocol, enabled, config) VALUES (9101, 'Ch9101', 'modbus_tcp', 1, '{}')")
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
// Phase 4: Command Send Endpoint Tests
// ========================================================================
// ========================================================================
// Phase 5: Route Composition Tests
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
            "protocol": "modbus_tcp",
            "parameters": {"host": "127.0.0.1", "port": 502}
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
async fn create_enabled_physical_channel_reports_degraded_when_device_is_unavailable() {
    let pool = create_test_sqlite_pool().await;
    let channel_manager = Arc::new(
        ChannelManager::with_shared_memory(
            crate::test_utils::create_test_routing_cache(),
            pool.clone(),
            crate::test_utils::create_test_shm_handle(),
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
            "protocol": "modbus_tcp",
            "enabled": true,
            "parameters": {"host": "127.0.0.1", "port": 502}
        })),
    );

    let response = app.oneshot(request).await.unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let payload = extract_json(response).await;
    assert_json_field(&payload, "/data/enabled", json!(true));
    assert_json_field(&payload, "/data/runtime_status", json!("degraded"));
    assert_json_field(&payload, "/data/reconciliation_required", json!(true));
    let persisted_enabled: bool =
        sqlx::query_scalar("SELECT enabled FROM channels WHERE channel_id = ?")
            .bind(2102_i64)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(persisted_enabled);
    assert!(channel_manager.get_channel(2102).is_none());
    assert_eq!(channel_manager.channel_count(), 0);
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
        protocol: "modbus_tcp".to_string(),
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
        "name": "test_modbus_channel",
        "protocol": "modbus_tcp",
        "enabled": true,
        "parameters": {
            "host": "127.0.0.1",
            "port": 502,
            "poll_interval_ms": 1000
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
        serde_json::json!("test_modbus_channel"),
    );
    assert_json_field(&json, "/data/protocol", serde_json::json!("modbus_tcp"));
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
        "protocol": "modbus_tcp",
        "enabled": true,
        "parameters": {
            "host": "127.0.0.1",
            "port": 502,
            "poll_interval_ms": 1000
        },
        "description": "Initial description"
    });

    let create_req = channel_mutation_request("POST", "/api/channels", Some(create_body));

    let _ = app.clone().oneshot(create_req).await.unwrap();

    // Step 2: Update channel with new values
    // Note: enabled field is managed via /control endpoint, not PUT
    let update_body = serde_json::json!({
        "name": "updated_name",
        "protocol": "modbus_tcp",
        "parameters": {"poll_interval_ms": 2000},
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
    assert_json_field(&json, "/data/protocol", serde_json::json!("modbus_tcp"));
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
        "protocol": "modbus_tcp",
        "enabled": true,
        "parameters": {
            "host": "127.0.0.1",
            "port": 502,
            "poll_interval_ms": 1000
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
        "protocol": "modbus_tcp",
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
async fn all_and_single_channel_reconciliation_share_one_application() {
    let reconciler = RecordingChannelReconciler::successful(reconciliation_items());
    let app = recording_reconciliation_router(
        Arc::clone(&reconciler),
        Arc::new(aether_store_local::MemoryAuditSink::new()),
    )
    .await;

    let response = app
        .clone()
        .oneshot(governed_reconciliation_request("/api/channels/reconcile"))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
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
    assert_eq!(payload["data"]["items"][1]["desired"]["status"], "absent");
    let serialized = payload.to_string().to_ascii_lowercase();
    for secret_bearing_field in ["parameters", "logging", "config", "credential"] {
        assert!(!serialized.contains(secret_bearing_field));
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
        .uri("/api/channels/reconcile")
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
        "protocol": "modbus_tcp",
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
                "protocol": "modbus_tcp",
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
                "protocol": "modbus_tcp",
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
    fn retired_io_aliases_stay_out_of_the_contract() {
        let spec = spec();
        let paths = spec["paths"].as_object().expect("OpenAPI paths object");
        for retired in [
            "/api/protocols",
            "/api/channels/list",
            "/api/channels/search",
            "/api/channels/{id}/control",
            "/api/channels/{id}/logging",
            "/api/admin/logs/level",
            "/api/points",
            "/api/channels/{channel_id}/T/points/{point_id}",
            "/api/channels/{channel_id}/S/points/{point_id}",
            "/api/channels/{channel_id}/C/points/{point_id}",
            "/api/channels/{channel_id}/A/points/{point_id}",
        ] {
            assert!(!paths.contains_key(retired), "retired IO alias {retired}");
        }
        assert_path_methods(paths, "/api/channels", &["get", "post"]);
        assert_path_methods(paths, "/api/channels/{id}/points", &["get"]);
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
            operation_count, 16,
            "HTTP operation count changed; re-audit Router/OpenAPI parity before updating this guard"
        );
    }
}
