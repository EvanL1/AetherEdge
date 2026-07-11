//! Channel Management API Integration Tests
//!
//! This test suite covers the channel management CRUD handlers:
//! - POST /api/channels - Create channel
//! - PUT /api/channels/{id} - Update channel configuration
//! - PUT /api/channels/{id}/enabled - Enable/disable channel
//! - DELETE /api/channels/{id} - Delete channel
//! - POST /api/channels/reload - Reload all channels
//! - POST /api/routing/reload - Reload routing cache
//!
//! Test scenarios cover:
//! - Happy path (success cases)
//! - Error handling (not found, conflict, validation)
//! - Edge cases (duplicate names, auto-assigned IDs)

#![allow(clippy::disallowed_methods)] // Integration test - unwrap is acceptable

mod support;

use anyhow::Result;
use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use http_body_util::BodyExt;
use serde_json::json;
use std::sync::Arc;
use tower::ServiceExt;

/// Create test SQLite database with required schema
async fn create_test_database() -> Result<sqlx::SqlitePool> {
    let pool = sqlx::SqlitePool::connect("sqlite::memory:").await?;

    // Create channels table (matches production schema)
    sqlx::query(
        r#"CREATE TABLE IF NOT EXISTS channels (
            channel_id INTEGER PRIMARY KEY,
            name TEXT NOT NULL UNIQUE,
            protocol TEXT NOT NULL,
            enabled BOOLEAN NOT NULL DEFAULT TRUE,
            config TEXT
        )"#,
    )
    .execute(&pool)
    .await?;

    // Create routing tables
    sqlx::query(
        r#"CREATE TABLE IF NOT EXISTS c2m_routing (
            channel_id INTEGER NOT NULL,
            point_type TEXT NOT NULL,
            point_id INTEGER NOT NULL,
            instance_id INTEGER NOT NULL,
            signal_name TEXT,
            PRIMARY KEY (channel_id, point_type, point_id)
        )"#,
    )
    .execute(&pool)
    .await?;

    sqlx::query(
        r#"CREATE TABLE IF NOT EXISTS m2c_routing (
            instance_id INTEGER NOT NULL,
            point_type TEXT NOT NULL,
            signal_name TEXT NOT NULL,
            channel_id INTEGER NOT NULL,
            point_id INTEGER NOT NULL,
            PRIMARY KEY (instance_id, point_type, signal_name)
        )"#,
    )
    .execute(&pool)
    .await?;

    sqlx::query(
        r#"CREATE TABLE IF NOT EXISTS c2c_routing (
            src_channel_id INTEGER NOT NULL,
            src_point_type TEXT NOT NULL,
            src_point_id INTEGER NOT NULL,
            dst_channel_id INTEGER NOT NULL,
            dst_point_type TEXT NOT NULL,
            dst_point_id INTEGER NOT NULL,
            scale REAL DEFAULT 1.0,
            offset REAL DEFAULT 0.0,
            PRIMARY KEY (src_channel_id, src_point_type, src_point_id, dst_channel_id, dst_point_type, dst_point_id)
        )"#,
    )
    .execute(&pool)
    .await?;

    Ok(pool)
}

/// Helper function to create a test app router
async fn create_test_app() -> Result<axum::Router> {
    let pool = create_test_database().await?;

    // Create routing cache (empty for integration test)
    let routing_cache = Arc::new(aether_routing::RoutingCache::new());

    // Create the channel manager over an available temporary SHM layout.
    let channel_manager = Arc::new(aether_io::ChannelManager::new(
        support::create_test_shm_handle(),
        routing_cache,
    )?);

    // Create command TX cache
    let command_tx_cache = Arc::new(aether_io::api::command_cache::CommandTxCache::new());

    // Create router
    let router = aether_io::api::routes::create_api_routes(channel_manager, pool, command_tx_cache);
    Ok(router)
}

/// Helper function to make HTTP requests and extract response
async fn make_request(
    app: &mut axum::Router,
    method: &str,
    uri: &str,
    body: Option<serde_json::Value>,
) -> Result<(StatusCode, serde_json::Value)> {
    let mut req_builder = Request::builder().method(method).uri(uri);

    let body_bytes = if let Some(json_body) = body {
        req_builder = req_builder.header("content-type", "application/json");
        serde_json::to_vec(&json_body)?
    } else {
        Vec::new()
    };

    let request = req_builder.body(Body::from(body_bytes))?;

    let response = app.clone().oneshot(request).await?;
    let status = response.status();

    let body_bytes = response.into_body().collect().await?.to_bytes();
    let response_json: serde_json::Value = if body_bytes.is_empty() {
        json!({})
    } else {
        match serde_json::from_slice(&body_bytes) {
            Ok(json) => json,
            Err(e) => {
                eprintln!("JSON parse error on {} {}: {}", method, uri, e);
                eprintln!("Response body: {:?}", std::str::from_utf8(&body_bytes));
                return Err(e.into());
            },
        }
    };

    Ok((status, response_json))
}

// ============================================================================
// Channel Creation Tests
// ============================================================================

#[tokio::test]
async fn test_create_channel_with_auto_id() -> Result<()> {
    let mut app = create_test_app().await?;

    // Create a channel without specifying ID (auto-assign)
    let create_payload = json!({
        "name": "Test Virtual Channel",
        "description": "A test channel for integration testing",
        "protocol": "virtual",
        "enabled": true,
        "parameters": {
            "update_interval_ms": 1000
        }
    });

    let (status, body) =
        make_request(&mut app, "POST", "/api/channels", Some(create_payload)).await?;

    assert_eq!(status, StatusCode::OK, "Response: {:?}", body);
    assert_eq!(body["success"], true);
    assert!(
        body["data"]["id"].as_u64().is_some(),
        "Should have auto-assigned ID"
    );
    assert_eq!(body["data"]["name"], "Test Virtual Channel");
    assert_eq!(body["data"]["protocol"], "virtual");
    assert_eq!(body["data"]["enabled"], true);

    Ok(())
}

#[tokio::test]
async fn test_create_channel_with_manual_id() -> Result<()> {
    let mut app = create_test_app().await?;

    // Create a channel with specific ID
    let create_payload = json!({
        "channel_id": 5001,
        "name": "Manual ID Channel",
        "description": "Channel with manually specified ID",
        "protocol": "modbus_tcp",
        "enabled": true,
        "parameters": {
            "host": "192.168.1.100",
            "port": 502
        }
    });

    let (status, body) =
        make_request(&mut app, "POST", "/api/channels", Some(create_payload)).await?;

    assert_eq!(status, StatusCode::OK, "Response: {:?}", body);
    assert_eq!(body["success"], true);
    assert_eq!(body["data"]["id"], 5001);
    assert_eq!(body["data"]["name"], "Manual ID Channel");

    Ok(())
}

#[tokio::test]
async fn test_create_channel_duplicate_name() -> Result<()> {
    let mut app = create_test_app().await?;

    // Create first channel
    let create_payload1 = json!({
        "name": "Duplicate Name Test",
        "protocol": "virtual",
        "enabled": true,
        "parameters": {}
    });

    let (status, _) = make_request(
        &mut app,
        "POST",
        "/api/channels",
        Some(create_payload1.clone()),
    )
    .await?;
    assert_eq!(status, StatusCode::OK);

    // Try to create another channel with same name
    let (status, body) =
        make_request(&mut app, "POST", "/api/channels", Some(create_payload1)).await?;

    assert_eq!(status, StatusCode::CONFLICT, "Should reject duplicate name");
    assert_eq!(body["success"], false);
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("already exists")
    );

    Ok(())
}

#[tokio::test]
async fn test_create_channel_duplicate_id() -> Result<()> {
    let mut app = create_test_app().await?;

    // Create first channel with specific ID
    let create_payload1 = json!({
        "channel_id": 9001,
        "name": "First Channel",
        "protocol": "virtual",
        "enabled": true,
        "parameters": {}
    });

    let (status, _) =
        make_request(&mut app, "POST", "/api/channels", Some(create_payload1)).await?;
    assert_eq!(status, StatusCode::OK);

    // Try to create another channel with same ID
    let create_payload2 = json!({
        "channel_id": 9001,
        "name": "Second Channel",
        "protocol": "virtual",
        "enabled": true,
        "parameters": {}
    });

    let (status, body) =
        make_request(&mut app, "POST", "/api/channels", Some(create_payload2)).await?;

    assert_eq!(status, StatusCode::CONFLICT, "Should reject duplicate ID");
    assert_eq!(body["success"], false);

    Ok(())
}

#[tokio::test]
async fn test_create_channel_disabled() -> Result<()> {
    let mut app = create_test_app().await?;

    // Create a disabled channel
    let create_payload = json!({
        "name": "Disabled Channel",
        "protocol": "virtual",
        "enabled": false,
        "parameters": {}
    });

    let (status, body) =
        make_request(&mut app, "POST", "/api/channels", Some(create_payload)).await?;

    assert_eq!(status, StatusCode::OK, "Response: {:?}", body);
    assert_eq!(body["success"], true);
    assert_eq!(body["data"]["enabled"], false);
    assert_eq!(body["data"]["runtime_status"], "stopped");

    Ok(())
}

// ============================================================================
// Channel Update Tests
// ============================================================================

#[tokio::test]
async fn test_update_channel_name() -> Result<()> {
    let mut app = create_test_app().await?;

    // Create a channel first
    let create_payload = json!({
        "channel_id": 2001,
        "name": "Original Name",
        "protocol": "virtual",
        "enabled": true,
        "parameters": {}
    });

    let (status, _) = make_request(&mut app, "POST", "/api/channels", Some(create_payload)).await?;
    assert_eq!(status, StatusCode::OK);

    // Update the channel name
    let update_payload = json!({
        "name": "Updated Name"
    });

    let (status, body) =
        make_request(&mut app, "PUT", "/api/channels/2001", Some(update_payload)).await?;

    assert_eq!(status, StatusCode::OK, "Response: {:?}", body);
    assert_eq!(body["success"], true);
    assert_eq!(body["data"]["name"], "Updated Name");

    Ok(())
}

#[tokio::test]
async fn test_update_channel_parameters() -> Result<()> {
    let mut app = create_test_app().await?;

    // Create a channel first
    let create_payload = json!({
        "channel_id": 2002,
        "name": "Param Test Channel",
        "protocol": "modbus_tcp",
        "enabled": true,
        "parameters": {
            "host": "192.168.1.100",
            "port": 502
        }
    });

    let (status, _) = make_request(&mut app, "POST", "/api/channels", Some(create_payload)).await?;
    assert_eq!(status, StatusCode::OK);

    // Update parameters (critical change - should trigger hot reload)
    let update_payload = json!({
        "parameters": {
            "host": "192.168.1.200",
            "port": 503
        }
    });

    let (status, body) =
        make_request(&mut app, "PUT", "/api/channels/2002", Some(update_payload)).await?;

    assert_eq!(status, StatusCode::OK, "Response: {:?}", body);
    assert_eq!(body["success"], true);
    // Runtime status should indicate update was applied
    assert!(
        body["data"]["runtime_status"].as_str().unwrap() == "updated"
            || body["data"]["runtime_status"].as_str().unwrap() == "running"
            || body["data"]["runtime_status"].as_str().unwrap() == "stopped"
    );

    Ok(())
}

#[tokio::test]
async fn test_update_channel_not_found() -> Result<()> {
    let mut app = create_test_app().await?;

    // Try to update non-existent channel
    let update_payload = json!({
        "name": "New Name"
    });

    let (status, body) =
        make_request(&mut app, "PUT", "/api/channels/99999", Some(update_payload)).await?;

    assert_eq!(status, StatusCode::NOT_FOUND, "Response: {:?}", body);
    assert_eq!(body["success"], false);

    Ok(())
}

#[tokio::test]
async fn test_update_channel_name_conflict() -> Result<()> {
    let mut app = create_test_app().await?;

    // Create two channels
    let create_payload1 = json!({
        "channel_id": 3001,
        "name": "Channel A",
        "protocol": "virtual",
        "enabled": true,
        "parameters": {}
    });

    let create_payload2 = json!({
        "channel_id": 3002,
        "name": "Channel B",
        "protocol": "virtual",
        "enabled": true,
        "parameters": {}
    });

    let (status, _) =
        make_request(&mut app, "POST", "/api/channels", Some(create_payload1)).await?;
    assert_eq!(status, StatusCode::OK);

    let (status, _) =
        make_request(&mut app, "POST", "/api/channels", Some(create_payload2)).await?;
    assert_eq!(status, StatusCode::OK);

    // Try to rename Channel B to Channel A (conflict)
    let update_payload = json!({
        "name": "Channel A"
    });

    let (status, body) =
        make_request(&mut app, "PUT", "/api/channels/3002", Some(update_payload)).await?;

    assert_eq!(status, StatusCode::CONFLICT, "Should reject duplicate name");
    assert_eq!(body["success"], false);

    Ok(())
}

// ============================================================================
// Channel Enable/Disable Tests
// ============================================================================

#[tokio::test]
async fn test_enable_disable_channel() -> Result<()> {
    let mut app = create_test_app().await?;

    // Create a channel
    let create_payload = json!({
        "channel_id": 4001,
        "name": "Enable/Disable Test",
        "protocol": "virtual",
        "enabled": true,
        "parameters": {}
    });

    let (status, _) = make_request(&mut app, "POST", "/api/channels", Some(create_payload)).await?;
    assert_eq!(status, StatusCode::OK);

    // Disable the channel
    let disable_payload = json!({ "enabled": false });
    let (status, body) = make_request(
        &mut app,
        "PUT",
        "/api/channels/4001/enabled",
        Some(disable_payload),
    )
    .await?;

    assert_eq!(status, StatusCode::OK, "Response: {:?}", body);
    assert_eq!(body["success"], true);
    assert_eq!(body["data"]["enabled"], false);
    assert_eq!(body["data"]["runtime_status"], "stopped");

    // Re-enable the channel
    let enable_payload = json!({ "enabled": true });
    let (status, body) = make_request(
        &mut app,
        "PUT",
        "/api/channels/4001/enabled",
        Some(enable_payload),
    )
    .await?;

    assert_eq!(status, StatusCode::OK, "Response: {:?}", body);
    assert_eq!(body["success"], true);
    assert_eq!(body["data"]["enabled"], true);

    Ok(())
}

#[tokio::test]
async fn test_enable_already_enabled_channel() -> Result<()> {
    let mut app = create_test_app().await?;

    // Create an enabled channel
    let create_payload = json!({
        "channel_id": 4002,
        "name": "Already Enabled Test",
        "protocol": "virtual",
        "enabled": true,
        "parameters": {}
    });

    let (status, _) = make_request(&mut app, "POST", "/api/channels", Some(create_payload)).await?;
    assert_eq!(status, StatusCode::OK);

    // Try to enable again (should return success with "already enabled" message)
    let enable_payload = json!({ "enabled": true });
    let (status, body) = make_request(
        &mut app,
        "PUT",
        "/api/channels/4002/enabled",
        Some(enable_payload),
    )
    .await?;

    assert_eq!(status, StatusCode::OK, "Response: {:?}", body);
    assert_eq!(body["success"], true);
    assert!(
        body["data"]["message"]
            .as_str()
            .unwrap()
            .contains("already")
    );

    Ok(())
}

#[tokio::test]
async fn test_enable_nonexistent_channel() -> Result<()> {
    let mut app = create_test_app().await?;

    let enable_payload = json!({ "enabled": true });
    let (status, body) = make_request(
        &mut app,
        "PUT",
        "/api/channels/99999/enabled",
        Some(enable_payload),
    )
    .await?;

    assert_eq!(status, StatusCode::NOT_FOUND, "Response: {:?}", body);
    assert_eq!(body["success"], false);

    Ok(())
}

// ============================================================================
// Channel Delete Tests
// ============================================================================

#[tokio::test]
async fn test_delete_channel() -> Result<()> {
    let mut app = create_test_app().await?;

    // Create a channel
    let create_payload = json!({
        "channel_id": 5001,
        "name": "Delete Test Channel",
        "protocol": "virtual",
        "enabled": true,
        "parameters": {}
    });

    let (status, _) = make_request(&mut app, "POST", "/api/channels", Some(create_payload)).await?;
    assert_eq!(status, StatusCode::OK);

    // Delete the channel
    let (status, body) = make_request(&mut app, "DELETE", "/api/channels/5001", None).await?;

    assert_eq!(status, StatusCode::OK, "Response: {:?}", body);
    assert_eq!(body["success"], true);
    assert!(body["data"].as_str().unwrap().contains("deleted"));

    // Verify channel is gone
    let (status, _) = make_request(&mut app, "GET", "/api/channels/5001", None).await?;
    assert_eq!(status, StatusCode::NOT_FOUND);

    Ok(())
}

#[tokio::test]
async fn test_delete_nonexistent_channel() -> Result<()> {
    let mut app = create_test_app().await?;

    let (status, body) = make_request(&mut app, "DELETE", "/api/channels/99999", None).await?;

    assert_eq!(status, StatusCode::NOT_FOUND, "Response: {:?}", body);
    assert_eq!(body["success"], false);

    Ok(())
}

// ============================================================================
// Reload Configuration Tests
// ============================================================================

#[tokio::test]
async fn test_reload_configuration() -> Result<()> {
    let mut app = create_test_app().await?;

    // Create some channels first
    for i in 1..=3 {
        let create_payload = json!({
            "channel_id": 6000 + i,
            "name": format!("Reload Test Channel {}", i),
            "protocol": "virtual",
            "enabled": true,
            "parameters": {}
        });

        let (status, _) =
            make_request(&mut app, "POST", "/api/channels", Some(create_payload)).await?;
        assert_eq!(status, StatusCode::OK);
    }

    // Trigger reload
    let (status, body) = make_request(&mut app, "POST", "/api/channels/reload", None).await?;

    assert_eq!(status, StatusCode::OK, "Response: {:?}", body);
    assert_eq!(body["success"], true);
    assert!(body["data"]["total_channels"].as_u64().is_some());

    Ok(())
}

#[tokio::test]
async fn test_reload_routing() -> Result<()> {
    let mut app = create_test_app().await?;

    // Trigger routing reload (even with empty routing tables)
    let (status, body) = make_request(&mut app, "POST", "/api/routing/reload", None).await?;

    assert_eq!(status, StatusCode::OK, "Response: {:?}", body);
    assert_eq!(body["success"], true);
    assert!(body["data"]["c2m_count"].as_u64().is_some());
    assert!(body["data"]["m2c_count"].as_u64().is_some());
    assert!(body["data"]["c2c_count"].as_u64().is_some());
    assert!(body["data"]["duration_ms"].as_u64().is_some());

    Ok(())
}

// ============================================================================
// Edge Cases and Validation Tests
// ============================================================================

#[tokio::test]
async fn test_create_channel_missing_required_fields() -> Result<()> {
    let app = create_test_app().await?;

    // Missing name (required field)
    let create_payload = json!({
        "protocol": "virtual",
        "enabled": true,
        "parameters": {}
    });

    // Make request without parsing JSON response (may be plain text error)
    let req = Request::builder()
        .method("POST")
        .uri("/api/channels")
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&create_payload)?))
        .unwrap();

    let response = app.clone().oneshot(req).await?;
    let status = response.status();

    // Axum returns 422 (UNPROCESSABLE_ENTITY) or 400 (BAD_REQUEST) for validation errors
    assert!(
        status == StatusCode::UNPROCESSABLE_ENTITY || status == StatusCode::BAD_REQUEST,
        "Expected 422 or 400 for missing required field, got {}",
        status
    );

    Ok(())
}

#[tokio::test]
async fn test_create_channel_with_logging_config() -> Result<()> {
    let mut app = create_test_app().await?;

    let create_payload = json!({
        "name": "Logging Config Test",
        "protocol": "virtual",
        "enabled": true,
        "parameters": {},
        "logging": {
            "enabled": true,
            "level": "debug"
        }
    });

    let (status, body) =
        make_request(&mut app, "POST", "/api/channels", Some(create_payload)).await?;

    // Note: logging config might be stored in config JSON, check if request succeeds
    assert_eq!(status, StatusCode::OK, "Response: {:?}", body);
    assert_eq!(body["success"], true);

    Ok(())
}

#[tokio::test]
async fn test_update_channel_logging_config() -> Result<()> {
    let mut app = create_test_app().await?;

    // Create a channel
    let create_payload = json!({
        "channel_id": 7001,
        "name": "Logging Update Test",
        "protocol": "virtual",
        "enabled": true,
        "parameters": {}
    });

    let (status, _) = make_request(&mut app, "POST", "/api/channels", Some(create_payload)).await?;
    assert_eq!(status, StatusCode::OK);

    // Update logging config (metadata-only change, no hot reload needed)
    let update_payload = json!({
        "logging": {
            "enabled": true,
            "level": "trace"
        }
    });

    let (status, body) =
        make_request(&mut app, "PUT", "/api/channels/7001", Some(update_payload)).await?;

    assert_eq!(status, StatusCode::OK, "Response: {:?}", body);
    assert_eq!(body["success"], true);

    Ok(())
}

#[tokio::test]
async fn test_sequential_channel_id_assignment() -> Result<()> {
    let mut app = create_test_app().await?;

    // Create first channel with auto-assigned ID
    let create_payload1 = json!({
        "name": "Auto ID 1",
        "protocol": "virtual",
        "enabled": true,
        "parameters": {}
    });

    let (status, body1) =
        make_request(&mut app, "POST", "/api/channels", Some(create_payload1)).await?;
    assert_eq!(status, StatusCode::OK);
    let first_id = body1["data"]["id"].as_u64().unwrap();

    // Create second channel with auto-assigned ID
    let create_payload2 = json!({
        "name": "Auto ID 2",
        "protocol": "virtual",
        "enabled": true,
        "parameters": {}
    });

    let (status, body2) =
        make_request(&mut app, "POST", "/api/channels", Some(create_payload2)).await?;
    assert_eq!(status, StatusCode::OK);
    let second_id = body2["data"]["id"].as_u64().unwrap();

    // Second ID should be greater than first
    assert!(
        second_id > first_id,
        "IDs should be sequential: {} > {}",
        second_id,
        first_id
    );

    Ok(())
}
