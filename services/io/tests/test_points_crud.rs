//! Point CRUD Integration Tests
//!
//! This test suite verifies the complete CRUD lifecycle for all four point types:
//! - Telemetry (T)
//! - Signal (S)
//! - Control (C)
//! - Adjustment (A)
//!
//! Test flow for each point type:
//! 1. CREATE: POST to create point
//! 2. READ: GET to verify point was created with correct data
//! 3. UPDATE: PUT to modify point
//! 4. READ: GET to verify update was applied
//! 5. DELETE: DELETE to remove point
//! 6. VERIFY: GET should return 404 after deletion

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

    // Create channels table
    sqlx::query(
        r#"CREATE TABLE IF NOT EXISTS channels (
            channel_id INTEGER PRIMARY KEY,
            name TEXT NOT NULL,
            protocol_type TEXT NOT NULL,
            enabled BOOLEAN NOT NULL DEFAULT TRUE,
            description TEXT,
            config_json TEXT
        )"#,
    )
    .execute(&pool)
    .await?;

    // Insert a test channel (ID 1001)
    sqlx::query(
        r#"INSERT INTO channels (channel_id, name, protocol_type, enabled, description, config_json)
           VALUES (1001, 'Test Channel', 'Virtual', 1, 'Test channel for CRUD tests', '{}')"#,
    )
    .execute(&pool)
    .await?;

    // Create telemetry_points table
    sqlx::query(
        r#"CREATE TABLE IF NOT EXISTS telemetry_points (
            channel_id INTEGER NOT NULL,
            point_id INTEGER NOT NULL,
            signal_name TEXT NOT NULL,
            scale REAL NOT NULL DEFAULT 1.0,
            offset REAL NOT NULL DEFAULT 0.0,
            unit TEXT,
            reverse BOOLEAN NOT NULL DEFAULT FALSE,
            data_type TEXT NOT NULL,
            description TEXT,
            protocol_mappings TEXT,
            PRIMARY KEY (channel_id, point_id),
            FOREIGN KEY (channel_id) REFERENCES channels(channel_id)
        )"#,
    )
    .execute(&pool)
    .await?;

    // Create signal_points table
    sqlx::query(
        r#"CREATE TABLE IF NOT EXISTS signal_points (
            channel_id INTEGER NOT NULL,
            point_id INTEGER NOT NULL,
            signal_name TEXT NOT NULL,
            scale REAL NOT NULL DEFAULT 1.0,
            offset REAL NOT NULL DEFAULT 0.0,
            unit TEXT,
            reverse BOOLEAN NOT NULL DEFAULT FALSE,
            normal_state INTEGER DEFAULT 0,
            data_type TEXT NOT NULL,
            description TEXT,
            protocol_mappings TEXT,
            PRIMARY KEY (channel_id, point_id),
            FOREIGN KEY (channel_id) REFERENCES channels(channel_id)
        )"#,
    )
    .execute(&pool)
    .await?;

    // Create control_points table
    sqlx::query(
        r#"CREATE TABLE IF NOT EXISTS control_points (
            channel_id INTEGER NOT NULL,
            point_id INTEGER NOT NULL,
            signal_name TEXT NOT NULL,
            scale REAL NOT NULL DEFAULT 1.0,
            offset REAL NOT NULL DEFAULT 0.0,
            unit TEXT,
            reverse BOOLEAN NOT NULL DEFAULT FALSE,
            data_type TEXT NOT NULL,
            description TEXT,
            protocol_mappings TEXT,
            PRIMARY KEY (channel_id, point_id),
            FOREIGN KEY (channel_id) REFERENCES channels(channel_id)
        )"#,
    )
    .execute(&pool)
    .await?;

    // Create adjustment_points table
    sqlx::query(
        r#"CREATE TABLE IF NOT EXISTS adjustment_points (
            channel_id INTEGER NOT NULL,
            point_id INTEGER NOT NULL,
            signal_name TEXT NOT NULL,
            scale REAL NOT NULL DEFAULT 1.0,
            offset REAL NOT NULL DEFAULT 0.0,
            unit TEXT,
            reverse BOOLEAN NOT NULL DEFAULT FALSE,
            data_type TEXT NOT NULL,
            description TEXT,
            protocol_mappings TEXT,
            PRIMARY KEY (channel_id, point_id),
            FOREIGN KEY (channel_id) REFERENCES channels(channel_id)
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
// Telemetry Point CRUD Tests
// ============================================================================

#[tokio::test]
async fn test_telemetry_point_crud_lifecycle() -> Result<()> {
    let mut app = create_test_app().await?;

    let channel_id = 1001;
    let point_id = 100;

    // Step 1: CREATE - Create a telemetry point
    let create_payload = json!({
        "channel_id": channel_id,
        "point_id": point_id,
        "signal_name": "Temperature_Sensor_1",
        "scale": 0.1,
        "offset": -40.0,
        "unit": "°C",
        "reverse": "false",
        "data_type": "int16",
        "description": "Outdoor temperature sensor"
    });

    let (status, create_body) = make_request(
        &mut app,
        "POST",
        &format!("/api/channels/{}/T/points/{}", channel_id, point_id),
        Some(create_payload),
    )
    .await?;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(create_body["success"], true);
    assert_eq!(create_body["data"]["channel_id"], channel_id);
    assert_eq!(create_body["data"]["point_id"], point_id);
    assert_eq!(create_body["data"]["point_type"], "T");

    // Step 2: READ - Verify point was created
    let (status, read_body_1) = make_request(
        &mut app,
        "GET",
        &format!("/api/channels/{}/T/points/{}", channel_id, point_id),
        None,
    )
    .await?;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(read_body_1["success"], true);
    assert_eq!(read_body_1["data"]["point_id"], point_id);
    assert_eq!(read_body_1["data"]["signal_name"], "Temperature_Sensor_1");
    assert_eq!(read_body_1["data"]["scale"], 0.1);
    assert_eq!(read_body_1["data"]["offset"], -40.0);
    assert_eq!(read_body_1["data"]["unit"], "°C");
    assert_eq!(read_body_1["data"]["data_type"], "int16");

    // Step 3: UPDATE - Modify the point
    let update_payload = json!({
        "signal_name": "Temperature_Sensor_1_Updated",
        "scale": 0.2,
        "description": "Updated outdoor temperature sensor"
    });

    let (status, update_body) = make_request(
        &mut app,
        "PUT",
        &format!("/api/channels/{}/T/points/{}", channel_id, point_id),
        Some(update_payload),
    )
    .await?;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(update_body["success"], true);

    // Step 4: READ - Verify update was applied
    let (status, read_body_2) = make_request(
        &mut app,
        "GET",
        &format!("/api/channels/{}/T/points/{}", channel_id, point_id),
        None,
    )
    .await?;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(read_body_2["success"], true);
    assert_eq!(
        read_body_2["data"]["signal_name"],
        "Temperature_Sensor_1_Updated"
    );
    assert_eq!(read_body_2["data"]["scale"], 0.2);
    assert_eq!(read_body_2["data"]["offset"], -40.0); // Should remain unchanged
    assert_eq!(
        read_body_2["data"]["description"],
        "Updated outdoor temperature sensor"
    );

    // Step 5: DELETE - Remove the point
    let (status, delete_body) = make_request(
        &mut app,
        "DELETE",
        &format!("/api/channels/{}/T/points/{}", channel_id, point_id),
        None,
    )
    .await?;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(delete_body["success"], true);
    assert_eq!(delete_body["data"]["channel_id"], channel_id);
    assert_eq!(delete_body["data"]["point_id"], point_id);

    // Step 6: VERIFY - GET should return 404 after deletion
    let (status, _) = make_request(
        &mut app,
        "GET",
        &format!("/api/channels/{}/T/points/{}", channel_id, point_id),
        None,
    )
    .await?;

    assert_eq!(status, StatusCode::NOT_FOUND);

    Ok(())
}

// ============================================================================
// Signal Point CRUD Tests
// ============================================================================

#[tokio::test]
async fn test_signal_point_crud_lifecycle() -> Result<()> {
    let mut app = create_test_app().await?;

    let channel_id = 1001;
    let point_id = 200;

    // Step 1: CREATE
    let create_payload = json!({
        "channel_id": channel_id,
        "point_id": point_id,
        "signal_name": "Door_Status_1",
        "scale": 1.0,
        "offset": 0.0,
        "unit": "",
        "reverse": "false",
        "data_type": "bool",
        "description": "Front door open/close status"
    });

    let (status, _) = make_request(
        &mut app,
        "POST",
        &format!("/api/channels/{}/S/points/{}", channel_id, point_id),
        Some(create_payload),
    )
    .await?;

    assert_eq!(status, StatusCode::OK);

    // Step 2: READ
    let (status, read_body_1) = make_request(
        &mut app,
        "GET",
        &format!("/api/channels/{}/S/points/{}", channel_id, point_id),
        None,
    )
    .await?;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(read_body_1["data"]["signal_name"], "Door_Status_1");
    assert_eq!(read_body_1["data"]["data_type"], "bool");

    // Step 3: UPDATE
    let update_payload = json!({
        "signal_name": "Door_Status_1_Updated",
        "reverse": true,
        "description": "Front door status (inverted logic)"
    });

    let (status, _) = make_request(
        &mut app,
        "PUT",
        &format!("/api/channels/{}/S/points/{}", channel_id, point_id),
        Some(update_payload),
    )
    .await?;

    assert_eq!(status, StatusCode::OK);

    // Step 4: READ - Verify update
    let (status, read_body_2) = make_request(
        &mut app,
        "GET",
        &format!("/api/channels/{}/S/points/{}", channel_id, point_id),
        None,
    )
    .await?;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(read_body_2["data"]["signal_name"], "Door_Status_1_Updated");
    assert_eq!(read_body_2["data"]["reverse"], true);

    // Step 5: DELETE
    let (status, _) = make_request(
        &mut app,
        "DELETE",
        &format!("/api/channels/{}/S/points/{}", channel_id, point_id),
        None,
    )
    .await?;

    assert_eq!(status, StatusCode::OK);

    // Step 6: VERIFY deletion
    let (status, _) = make_request(
        &mut app,
        "GET",
        &format!("/api/channels/{}/S/points/{}", channel_id, point_id),
        None,
    )
    .await?;

    assert_eq!(status, StatusCode::NOT_FOUND);

    Ok(())
}

// ============================================================================
// Control Point CRUD Tests
// ============================================================================

#[tokio::test]
async fn test_control_point_crud_lifecycle() -> Result<()> {
    let mut app = create_test_app().await?;

    let channel_id = 1001;
    let point_id = 300;

    // Step 1: CREATE
    let create_payload = json!({
        "channel_id": channel_id,
        "point_id": point_id,
        "signal_name": "Pump_Control_1",
        "scale": 1.0,
        "offset": 0.0,
        "unit": "",
        "reverse": "false",
        "data_type": "bool",
        "description": "Water pump on/off control"
    });

    let (status, _) = make_request(
        &mut app,
        "POST",
        &format!("/api/channels/{}/C/points/{}", channel_id, point_id),
        Some(create_payload),
    )
    .await?;

    assert_eq!(status, StatusCode::OK);

    // Step 2: READ
    let (status, read_body_1) = make_request(
        &mut app,
        "GET",
        &format!("/api/channels/{}/C/points/{}", channel_id, point_id),
        None,
    )
    .await?;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(read_body_1["data"]["signal_name"], "Pump_Control_1");

    // Step 3: UPDATE
    let update_payload = json!({
        "signal_name": "Pump_Control_1_Updated",
        "reverse": true
    });

    let (status, _) = make_request(
        &mut app,
        "PUT",
        &format!("/api/channels/{}/C/points/{}", channel_id, point_id),
        Some(update_payload),
    )
    .await?;

    assert_eq!(status, StatusCode::OK);

    // Step 4: READ - Verify update
    let (status, read_body_2) = make_request(
        &mut app,
        "GET",
        &format!("/api/channels/{}/C/points/{}", channel_id, point_id),
        None,
    )
    .await?;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(read_body_2["data"]["signal_name"], "Pump_Control_1_Updated");
    assert_eq!(read_body_2["data"]["reverse"], true);

    // Step 5: DELETE
    let (status, _) = make_request(
        &mut app,
        "DELETE",
        &format!("/api/channels/{}/C/points/{}", channel_id, point_id),
        None,
    )
    .await?;

    assert_eq!(status, StatusCode::OK);

    // Step 6: VERIFY deletion
    let (status, _) = make_request(
        &mut app,
        "GET",
        &format!("/api/channels/{}/C/points/{}", channel_id, point_id),
        None,
    )
    .await?;

    assert_eq!(status, StatusCode::NOT_FOUND);

    Ok(())
}

// ============================================================================
// Adjustment Point CRUD Tests
// ============================================================================

#[tokio::test]
async fn test_adjustment_point_crud_lifecycle() -> Result<()> {
    let mut app = create_test_app().await?;

    let channel_id = 1001;
    let point_id = 400;

    // Step 1: CREATE
    let create_payload = json!({
        "channel_id": channel_id,
        "point_id": point_id,
        "signal_name": "Setpoint_Temperature",
        "scale": 0.1,
        "offset": 0.0,
        "unit": "°C",
        "reverse": "false",
        "data_type": "int16",
        "description": "Target temperature setpoint"
    });

    let (status, _) = make_request(
        &mut app,
        "POST",
        &format!("/api/channels/{}/A/points/{}", channel_id, point_id),
        Some(create_payload),
    )
    .await?;

    assert_eq!(status, StatusCode::OK);

    // Step 2: READ
    let (status, read_body_1) = make_request(
        &mut app,
        "GET",
        &format!("/api/channels/{}/A/points/{}", channel_id, point_id),
        None,
    )
    .await?;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(read_body_1["data"]["signal_name"], "Setpoint_Temperature");
    assert_eq!(read_body_1["data"]["scale"], 0.1);

    // Step 3: UPDATE
    let update_payload = json!({
        "signal_name": "Setpoint_Temperature_Updated",
        "scale": 0.01,
        "unit": "K"
    });

    let (status, _) = make_request(
        &mut app,
        "PUT",
        &format!("/api/channels/{}/A/points/{}", channel_id, point_id),
        Some(update_payload),
    )
    .await?;

    assert_eq!(status, StatusCode::OK);

    // Step 4: READ - Verify update
    let (status, read_body_2) = make_request(
        &mut app,
        "GET",
        &format!("/api/channels/{}/A/points/{}", channel_id, point_id),
        None,
    )
    .await?;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        read_body_2["data"]["signal_name"],
        "Setpoint_Temperature_Updated"
    );
    assert_eq!(read_body_2["data"]["scale"], 0.01);
    assert_eq!(read_body_2["data"]["unit"], "K");

    // Step 5: DELETE
    let (status, _) = make_request(
        &mut app,
        "DELETE",
        &format!("/api/channels/{}/A/points/{}", channel_id, point_id),
        None,
    )
    .await?;

    assert_eq!(status, StatusCode::OK);

    // Step 6: VERIFY deletion
    let (status, _) = make_request(
        &mut app,
        "GET",
        &format!("/api/channels/{}/A/points/{}", channel_id, point_id),
        None,
    )
    .await?;

    assert_eq!(status, StatusCode::NOT_FOUND);

    Ok(())
}

// ============================================================================
// Boundary Condition Tests
// ============================================================================

#[tokio::test]
async fn test_duplicate_point_creation() -> Result<()> {
    let mut app = create_test_app().await?;

    let channel_id = 1001;
    let point_id = 500;

    let create_payload = json!({
        "channel_id": channel_id,
        "point_id": point_id,
        "signal_name": "Test_Point",
        "data_type": "float32"
    });

    // First creation should succeed
    let (status, _) = make_request(
        &mut app,
        "POST",
        &format!("/api/channels/{}/T/points/{}", channel_id, point_id),
        Some(create_payload.clone()),
    )
    .await?;

    assert_eq!(status, StatusCode::OK);

    // Second creation with same ID should fail with 409 Conflict
    let (status, _) = make_request(
        &mut app,
        "POST",
        &format!("/api/channels/{}/T/points/{}", channel_id, point_id),
        Some(create_payload),
    )
    .await?;

    assert_eq!(status, StatusCode::CONFLICT);

    Ok(())
}

#[tokio::test]
async fn test_update_nonexistent_point() -> Result<()> {
    let mut app = create_test_app().await?;

    let channel_id = 1001;
    let point_id = 999; // Non-existent point

    let update_payload = json!({
        "signal_name": "Should_Fail"
    });

    let (status, _) = make_request(
        &mut app,
        "PUT",
        &format!("/api/channels/{}/T/points/{}", channel_id, point_id),
        Some(update_payload),
    )
    .await?;

    assert_eq!(status, StatusCode::NOT_FOUND);

    Ok(())
}

#[tokio::test]
async fn test_delete_nonexistent_point() -> Result<()> {
    let mut app = create_test_app().await?;

    let channel_id = 1001;
    let point_id = 999; // Non-existent point

    let (status, _) = make_request(
        &mut app,
        "DELETE",
        &format!("/api/channels/{}/T/points/{}", channel_id, point_id),
        None,
    )
    .await?;

    assert_eq!(status, StatusCode::NOT_FOUND);

    Ok(())
}

#[tokio::test]
async fn test_create_point_with_nonexistent_channel() -> Result<()> {
    let mut app = create_test_app().await?;

    let channel_id = 9999; // Non-existent channel
    let point_id = 1;

    let create_payload = json!({
        "channel_id": channel_id,
        "point_id": point_id,
        "signal_name": "Should_Fail",
        "data_type": "float32"
    });

    let (status, _) = make_request(
        &mut app,
        "POST",
        &format!("/api/channels/{}/T/points/{}", channel_id, point_id),
        Some(create_payload),
    )
    .await?;

    assert_eq!(status, StatusCode::NOT_FOUND);

    Ok(())
}

#[tokio::test]
async fn test_update_with_empty_fields() -> Result<()> {
    let mut app = create_test_app().await?;

    let channel_id = 1001;
    let point_id = 600;

    // First create a point
    let create_payload = json!({
        "channel_id": channel_id,
        "point_id": point_id,
        "signal_name": "Test_Point",
        "data_type": "float32"
    });

    let (status, _) = make_request(
        &mut app,
        "POST",
        &format!("/api/channels/{}/T/points/{}", channel_id, point_id),
        Some(create_payload),
    )
    .await?;

    assert_eq!(status, StatusCode::OK);

    // Try to update with empty object (no fields)
    let update_payload = json!({});

    let (status, _) = make_request(
        &mut app,
        "PUT",
        &format!("/api/channels/{}/T/points/{}", channel_id, point_id),
        Some(update_payload),
    )
    .await?;

    // Should return 400 Bad Request (no fields to update)
    assert_eq!(status, StatusCode::BAD_REQUEST);

    Ok(())
}
