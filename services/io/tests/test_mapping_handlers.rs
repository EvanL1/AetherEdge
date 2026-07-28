//! Read-only protocol mapping API integration tests.

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

const ADMIN_ACCESS_TOKEN: &str = "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJ1c2VyX2lkIjo3LCJyb2xlIjoiQWRtaW4iLCJ0eXBlIjoiYWNjZXNzIiwiaWF0IjoxNzAwMDAwMDAwLCJleHAiOjQxMDI0NDQ4MDB9.JtjQvDBo7j0bLOxwed6yC9-M9qFCloc4H2Dt0LjzF9E";

/// Create test SQLite database with required schema (including point tables)
async fn create_test_database() -> Result<sqlx::SqlitePool> {
    let pool = sqlx::SqlitePool::connect("sqlite::memory:").await?;

    // Create channels table (matches production schema)
    sqlx::query(
        r#"CREATE TABLE IF NOT EXISTS channels (
            channel_id INTEGER PRIMARY KEY,
            name TEXT NOT NULL UNIQUE,
            protocol TEXT NOT NULL,
            enabled BOOLEAN NOT NULL DEFAULT TRUE,
            config TEXT,
            revision INTEGER NOT NULL DEFAULT 1
        )"#,
    )
    .execute(&pool)
    .await?;

    // Create telemetry_points table (signal_name is required by handler)
    sqlx::query(
        r#"CREATE TABLE IF NOT EXISTS telemetry_points (
            channel_id INTEGER NOT NULL,
            point_id INTEGER NOT NULL,
            signal_name TEXT NOT NULL DEFAULT '',
            protocol_mappings TEXT,
            scale REAL DEFAULT 1.0,
            offset REAL DEFAULT 0.0,
            PRIMARY KEY (channel_id, point_id)
        )"#,
    )
    .execute(&pool)
    .await?;

    // Create signal_points table
    sqlx::query(
        r#"CREATE TABLE IF NOT EXISTS signal_points (
            channel_id INTEGER NOT NULL,
            point_id INTEGER NOT NULL,
            signal_name TEXT NOT NULL DEFAULT '',
            protocol_mappings TEXT,
            PRIMARY KEY (channel_id, point_id)
        )"#,
    )
    .execute(&pool)
    .await?;

    // Create control_points table
    sqlx::query(
        r#"CREATE TABLE IF NOT EXISTS control_points (
            channel_id INTEGER NOT NULL,
            point_id INTEGER NOT NULL,
            signal_name TEXT NOT NULL DEFAULT '',
            protocol_mappings TEXT,
            PRIMARY KEY (channel_id, point_id)
        )"#,
    )
    .execute(&pool)
    .await?;

    // Create adjustment_points table
    sqlx::query(
        r#"CREATE TABLE IF NOT EXISTS adjustment_points (
            channel_id INTEGER NOT NULL,
            point_id INTEGER NOT NULL,
            signal_name TEXT NOT NULL DEFAULT '',
            protocol_mappings TEXT,
            PRIMARY KEY (channel_id, point_id)
        )"#,
    )
    .execute(&pool)
    .await?;

    // Create routing tables (needed for app state initialization)
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

/// Create a test database with a Modbus channel and sample points
async fn create_test_database_with_modbus_channel() -> Result<sqlx::SqlitePool> {
    let pool = create_test_database().await?;

    // Insert a Modbus TCP channel
    sqlx::query(
        r#"INSERT INTO channels (channel_id, name, protocol, enabled, config)
           VALUES (1001, 'Test Modbus Channel', 'modbus_tcp', 1, '{"host": "127.0.0.1", "port": 502}')"#,
    )
    .execute(&pool)
    .await?;

    // Insert telemetry points (T)
    sqlx::query(
        r#"INSERT INTO telemetry_points (channel_id, point_id, signal_name, protocol_mappings)
           VALUES (1001, 101, 'Temperature', '{"slave_id": 1, "function_code": 3, "register_address": 100, "data_type": "float32", "byte_order": "ABCD"}')"#,
    )
    .execute(&pool)
    .await?;

    sqlx::query(
        r#"INSERT INTO telemetry_points (channel_id, point_id, signal_name, protocol_mappings)
           VALUES (1001, 102, 'Pressure', NULL)"#,
    )
    .execute(&pool)
    .await?;

    // Insert signal points (S)
    sqlx::query(
        r#"INSERT INTO signal_points (channel_id, point_id, signal_name, protocol_mappings)
           VALUES (1001, 201, 'Alarm_Status', '{"slave_id": 1, "function_code": 2, "register_address": 50}')"#,
    )
    .execute(&pool)
    .await?;

    // Insert control points (C)
    sqlx::query(
        r#"INSERT INTO control_points (channel_id, point_id, signal_name, protocol_mappings)
           VALUES (1001, 301, 'Pump_Control', '{"slave_id": 1, "function_code": 5, "register_address": 200}')"#,
    )
    .execute(&pool)
    .await?;

    // Insert adjustment points (A)
    sqlx::query(
        r#"INSERT INTO adjustment_points (channel_id, point_id, signal_name, protocol_mappings)
           VALUES (1001, 401, 'Setpoint', '{"slave_id": 1, "function_code": 6, "register_address": 300}')"#,
    )
    .execute(&pool)
    .await?;

    Ok(pool)
}

/// Helper function to create a test app router with custom database
async fn create_test_app_with_pool(pool: sqlx::SqlitePool) -> Result<axum::Router> {
    let manager = Arc::new(aether_io::core::channels::ChannelManager::new(
        support::create_test_shm_handle(),
        Arc::new(aether_routing::ChannelRoutingCache::new()),
    )?);
    Ok(aether_io::api::routes::create_api_routes(manager, pool))
}

async fn make_request(
    app: &axum::Router,
    method: &str,
    uri: &str,
    body: Option<serde_json::Value>,
) -> Result<(StatusCode, serde_json::Value)> {
    let mut req_builder = Request::builder().method(method).uri(uri);
    if method == "PUT" {
        let channel_id = uri
            .split('/')
            .nth(3)
            .ok_or_else(|| anyhow::anyhow!("missing channel ID"))?;
        let revision_request = Request::builder()
            .method("GET")
            .uri(format!("/api/channels/{channel_id}"))
            .body(Body::empty())?;
        let revision_response = app.clone().oneshot(revision_request).await?;
        let revision_body = revision_response.into_body().collect().await?.to_bytes();
        let revision_json: serde_json::Value = serde_json::from_slice(&revision_body)?;
        let revision = revision_json["data"]["revision"].as_u64().unwrap_or(1);
        req_builder = req_builder
            .header("authorization", format!("Bearer {ADMIN_ACCESS_TOKEN}"))
            .header("x-aether-confirmed", "true")
            .header("x-aether-expected-revision", revision.to_string());
    }

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
                let text = String::from_utf8_lossy(&body_bytes);
                eprintln!("Raw response: {}", text);
                json!({ "raw": text.to_string() })
            },
        }
    };

    Ok((status, response_json))
}

// ============================================================================
// GET /api/channels/{id}/mappings Tests
// ============================================================================

#[tokio::test]
async fn test_get_mappings_for_nonexistent_channel() -> Result<()> {
    let pool = create_test_database().await?;
    let app = create_test_app_with_pool(pool).await?;

    let (status, _body) = make_request(&app, "GET", "/api/channels/9999/mappings", None).await?;

    // Should return 404 with channel not found error
    assert_eq!(status, StatusCode::NOT_FOUND);
    Ok(())
}

#[tokio::test]
async fn test_get_mappings_returns_grouped_format() -> Result<()> {
    let pool = create_test_database_with_modbus_channel().await?;
    let app = create_test_app_with_pool(pool).await?;

    let (status, body) = make_request(&app, "GET", "/api/channels/1001/mappings", None).await?;

    assert_eq!(status, StatusCode::OK, "Response: {:?}", body);
    assert!(
        body.get("data").is_some(),
        "Response should have data field"
    );

    // GroupedMappings contains telemetry, signal, control, adjustment arrays
    let data = &body["data"];
    assert!(
        data.get("telemetry").is_some(),
        "Should have telemetry field"
    );
    assert!(data.get("signal").is_some(), "Should have signal field");
    assert!(data.get("control").is_some(), "Should have control field");
    assert!(
        data.get("adjustment").is_some(),
        "Should have adjustment field"
    );

    Ok(())
}

#[tokio::test]
async fn test_get_mappings_telemetry_points() -> Result<()> {
    let pool = create_test_database_with_modbus_channel().await?;
    let app = create_test_app_with_pool(pool).await?;

    let (status, body) = make_request(&app, "GET", "/api/channels/1001/mappings", None).await?;

    assert_eq!(status, StatusCode::OK, "Response: {:?}", body);
    let data = &body["data"];

    // Should have 2 telemetry points (101 with mapping, 102 without)
    let telemetry = data["telemetry"]
        .as_array()
        .expect("telemetry should be array");
    assert_eq!(telemetry.len(), 2);

    // Check first point has protocol_data
    let point_101 = telemetry
        .iter()
        .find(|p| p["point_id"] == 101)
        .expect("Should find point 101");
    assert!(
        !point_101["protocol_data"].as_object().unwrap().is_empty(),
        "Point 101 should have protocol_data"
    );

    Ok(())
}

#[tokio::test]
async fn test_get_mappings_signal_points() -> Result<()> {
    let pool = create_test_database_with_modbus_channel().await?;
    let app = create_test_app_with_pool(pool).await?;

    let (status, body) = make_request(&app, "GET", "/api/channels/1001/mappings", None).await?;

    assert_eq!(status, StatusCode::OK);
    let data = &body["data"];

    // Should have 1 signal point
    let signal = data["signal"].as_array().expect("signal should be array");
    assert_eq!(signal.len(), 1);
    assert_eq!(signal[0]["point_id"], 201);

    Ok(())
}

#[tokio::test]
async fn test_get_mappings_control_points() -> Result<()> {
    let pool = create_test_database_with_modbus_channel().await?;
    let app = create_test_app_with_pool(pool).await?;

    let (status, body) = make_request(&app, "GET", "/api/channels/1001/mappings", None).await?;

    assert_eq!(status, StatusCode::OK);
    let data = &body["data"];

    // Should have 1 control point
    let control = data["control"].as_array().expect("control should be array");
    assert_eq!(control.len(), 1);
    assert_eq!(control[0]["point_id"], 301);

    Ok(())
}

#[tokio::test]
async fn test_get_mappings_adjustment_points() -> Result<()> {
    let pool = create_test_database_with_modbus_channel().await?;
    let app = create_test_app_with_pool(pool).await?;

    let (status, body) = make_request(&app, "GET", "/api/channels/1001/mappings", None).await?;

    assert_eq!(status, StatusCode::OK);
    let data = &body["data"];

    // Should have 1 adjustment point
    let adjustment = data["adjustment"]
        .as_array()
        .expect("adjustment should be array");
    assert_eq!(adjustment.len(), 1);
    assert_eq!(adjustment[0]["point_id"], 401);

    Ok(())
}

// ============================================================================
// PUT /api/channels/{id}/mappings Tests - Replace Mode
// ============================================================================
