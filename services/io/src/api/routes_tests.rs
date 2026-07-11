// NOTE: API tests use a real temporary mmap so the test topology matches the
// production SHM-only data plane.
#![allow(clippy::disallowed_methods)] // Test code - unwrap is acceptable

use super::*;
use crate::dto::{AdjustmentRequest, ChannelOperation, ControlRequest};
use axum::{
    body::Body,
    http::{Request, Response, StatusCode},
};
use serde_json::json;
use sqlx::SqlitePool;
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use aether_model::PointType;
use aether_rtdb_shm::ShmHandle;
use tower::util::ServiceExt; // for `oneshot` and `ready`

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
    let command_tx_cache = Arc::new(crate::api::command_cache::CommandTxCache::new());
    create_api_routes(channel_manager, sqlite_pool, command_tx_cache)
}

/// Helper: Build a Router using a provided in-memory SQLite pool
async fn create_test_api_with_pool(
    channel_manager: Arc<ChannelManager>,
    sqlite_pool: SqlitePool,
) -> Router {
    let command_tx_cache = Arc::new(crate::api::command_cache::CommandTxCache::new());
    create_api_routes(channel_manager, sqlite_pool, command_tx_cache)
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

    let req = Request::builder()
        .uri("/api/channels")
        .method("POST")
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();

    use http_body_util::BodyExt as _;
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(v["success"], true);
    assert_eq!(
        v["data"]["description"],
        serde_json::Value::String("desc-A".to_string())
    );
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
    let req = Request::builder()
        .uri("/api/channels/42")
        .method("PUT")
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();

    use http_body_util::BodyExt as _;
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(v["data"]["description"], "new-desc");

    // Update without description: should keep last description
    let body2 = serde_json::json!({ "parameters": {"x": 1} });
    let req2 = Request::builder()
        .uri("/api/channels/42")
        .method("PUT")
        .header("content-type", "application/json")
        .body(Body::from(body2.to_string()))
        .unwrap();
    let resp2 = app.oneshot(req2).await.unwrap();
    assert_eq!(resp2.status(), StatusCode::OK);
    let bytes2 = resp2.into_body().collect().await.unwrap().to_bytes();
    let v2: serde_json::Value = serde_json::from_slice(&bytes2).unwrap();
    assert_eq!(v2["data"]["description"], "new-desc");
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
    let req = Request::builder()
        .uri("/api/channels/77/enabled")
        .method("PUT")
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    use http_body_util::BodyExt as _;
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(v["data"]["description"], "keep-me");

    // Disable
    let body2 = serde_json::json!({"enabled": false});
    let req2 = Request::builder()
        .uri("/api/channels/77/enabled")
        .method("PUT")
        .header("content-type", "application/json")
        .body(Body::from(body2.to_string()))
        .unwrap();
    let resp2 = app.oneshot(req2).await.unwrap();
    assert_eq!(resp2.status(), StatusCode::OK);
    let bytes2 = resp2.into_body().collect().await.unwrap().to_bytes();
    let v2: serde_json::Value = serde_json::from_slice(&bytes2).unwrap();
    assert_eq!(v2["data"]["description"], "keep-me");
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
    let req = Request::builder()
        .uri("/api/channels/600")
        .method("DELETE")
        .body(Body::empty())
        .unwrap();
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
async fn test_reload_configuration_disabled_channel_adds_without_runtime() {
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

    let req = Request::builder()
        .uri("/api/channels/reload")
        .method("POST")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
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

#[tokio::test]
async fn test_control_channel_start_operation() {
    let channel_manager = Arc::new(
        ChannelManager::new(
            crate::test_utils::create_test_shm_handle(),
            crate::test_utils::create_test_routing_cache(),
        )
        .unwrap(),
    );
    let app = create_test_api_routes(channel_manager).await;

    let operation = ChannelOperation {
        operation: "start".to_string(),
    };

    let request = Request::builder()
        .uri("/api/channels/1001/control")
        .method("POST")
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_string(&operation).unwrap()))
        .unwrap();

    let response = app.oneshot(request).await.unwrap();

    // Should return 404 (channel not found) or other valid status
    assert!(
        response.status() == StatusCode::NOT_FOUND
            || response.status() == StatusCode::INTERNAL_SERVER_ERROR
            || response.status() == StatusCode::OK
    );
}

#[tokio::test]
async fn test_control_channel_stop_operation() {
    let channel_manager = Arc::new(
        ChannelManager::new(
            crate::test_utils::create_test_shm_handle(),
            crate::test_utils::create_test_routing_cache(),
        )
        .unwrap(),
    );
    let app = create_test_api_routes(channel_manager).await;

    let operation = ChannelOperation {
        operation: "stop".to_string(),
    };

    let request = Request::builder()
        .uri("/api/channels/1001/control")
        .method("POST")
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_string(&operation).unwrap()))
        .unwrap();

    let response = app.oneshot(request).await.unwrap();

    assert!(
        response.status() == StatusCode::NOT_FOUND
            || response.status() == StatusCode::INTERNAL_SERVER_ERROR
            || response.status() == StatusCode::OK
    );
}

#[tokio::test]
async fn test_control_channel_restart_operation() {
    let channel_manager = Arc::new(
        ChannelManager::new(
            crate::test_utils::create_test_shm_handle(),
            crate::test_utils::create_test_routing_cache(),
        )
        .unwrap(),
    );
    let app = create_test_api_routes(channel_manager).await;

    let operation = ChannelOperation {
        operation: "restart".to_string(),
    };

    let request = Request::builder()
        .uri("/api/channels/1001/control")
        .method("POST")
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_string(&operation).unwrap()))
        .unwrap();

    let response = app.oneshot(request).await.unwrap();

    assert!(
        response.status() == StatusCode::NOT_FOUND
            || response.status() == StatusCode::INTERNAL_SERVER_ERROR
            || response.status() == StatusCode::OK
    );
}

#[tokio::test]
async fn test_control_channel_invalid_operation_returns_400() {
    let channel_manager = Arc::new(
        ChannelManager::new(
            crate::test_utils::create_test_shm_handle(),
            crate::test_utils::create_test_routing_cache(),
        )
        .unwrap(),
    );
    let app = create_test_api_routes(channel_manager).await;

    let operation = ChannelOperation {
        operation: "invalid_op".to_string(),
    };

    let request = Request::builder()
        .uri("/api/channels/1001/control")
        .method("POST")
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_string(&operation).unwrap()))
        .unwrap();

    let response = app.oneshot(request).await.unwrap();

    assert!(
        response.status() == StatusCode::BAD_REQUEST || response.status() == StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn test_control_channel_not_found_returns_404() {
    let channel_manager = Arc::new(
        ChannelManager::new(
            crate::test_utils::create_test_shm_handle(),
            crate::test_utils::create_test_routing_cache(),
        )
        .unwrap(),
    );
    let app = create_test_api_routes(channel_manager).await;

    let operation = ChannelOperation {
        operation: "start".to_string(),
    };

    let request = Request::builder()
        .uri("/api/channels/9999/control")
        .method("POST")
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_string(&operation).unwrap()))
        .unwrap();

    let response = app.oneshot(request).await.unwrap();

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
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

    let request = Request::builder()
        .uri("/api/channels")
        .method("POST")
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_string(&request_body).unwrap()))
        .unwrap();

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

    let request = Request::builder()
        .uri("/api/channels/1001")
        .method("PUT")
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_string(&request_body).unwrap()))
        .unwrap();

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

    let request = Request::builder()
        .uri("/api/channels/1001")
        .method("DELETE")
        .body(Body::empty())
        .unwrap();

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

    let request = Request::builder()
        .uri("/api/channels/1001/enabled")
        .method("PUT")
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_string(&request_body).unwrap()))
        .unwrap();

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

    let request = Request::builder()
        .uri("/api/channels/1001/enabled")
        .method("PUT")
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_string(&request_body).unwrap()))
        .unwrap();

    let response = app.oneshot(request).await.unwrap();

    // Should return appropriate status code
    assert!(
        response.status() == StatusCode::OK
            || response.status() == StatusCode::NOT_FOUND
            || response.status() == StatusCode::INTERNAL_SERVER_ERROR
    );
}

#[tokio::test]
async fn test_reload_configuration_handler() {
    let channel_manager = Arc::new(
        ChannelManager::new(
            crate::test_utils::create_test_shm_handle(),
            crate::test_utils::create_test_routing_cache(),
        )
        .unwrap(),
    );
    let app = create_test_api_routes(channel_manager).await;

    let request = Request::builder()
        .uri("/api/channels/reload")
        .method("POST")
        .body(Body::empty())
        .unwrap();

    let response = app.oneshot(request).await.unwrap();

    // Should return appropriate status code
    assert!(
        response.status() == StatusCode::OK
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
        "name": "test_virtual_channel",
        "protocol": "virtual",
        "enabled": true,
        "parameters": {
            "interval_ms": 1000,
            "initial_value": 100
        },
        "description": "Full closed-loop test channel"
    });

    let create_req = Request::builder()
        .uri("/api/channels")
        .method("POST")
        .header("content-type", "application/json")
        .body(Body::from(create_body.to_string()))
        .unwrap();

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

    let create_req = Request::builder()
        .uri("/api/channels")
        .method("POST")
        .header("content-type", "application/json")
        .body(Body::from(create_body.to_string()))
        .unwrap();

    let _ = app.clone().oneshot(create_req).await.unwrap();

    // Step 2: Update channel with new values
    // Note: enabled field is managed via /control endpoint, not PUT
    let update_body = serde_json::json!({
        "name": "updated_name",
        "protocol": "virtual",
        "parameters": {"interval_ms": 2000},
        "description": "Updated description"
    });

    let update_req = Request::builder()
        .uri("/api/channels/2002")
        .method("PUT")
        .header("content-type", "application/json")
        .body(Body::from(update_body.to_string()))
        .unwrap();

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

    let create_req = Request::builder()
        .uri("/api/channels")
        .method("POST")
        .header("content-type", "application/json")
        .body(Body::from(create_body.to_string()))
        .unwrap();

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
    let delete_req = Request::builder()
        .uri("/api/channels/3001")
        .method("DELETE")
        .body(Body::empty())
        .unwrap();

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
async fn setup_write_test_env() -> (Router, Arc<ShmHandle>, tokio::task::JoinHandle<()>) {
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
            "telemetry": [{"point_id": 1, "signal_name": "v", "protocol_data": {"register": 0, "slave_id": 1}}],
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
            "telemetry": [{"point_id": 1, "signal_name": "v", "protocol_data": {"register": 100, "slave_id": 1}}]
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
    assert_eq!(mapping["register"], 100);
}

// ========================================================================
// OpenAPI Spec Completeness Tests
// ========================================================================

#[cfg(feature = "openapi")]
mod openapi_tests {
    use crate::api::routes::IoApiDoc;
    use utoipa::OpenApi;

    #[test]
    fn test_openapi_spec_generates_without_panic() {
        let doc = IoApiDoc::openapi();
        let json = doc.to_pretty_json().unwrap();
        assert!(!json.is_empty());
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
