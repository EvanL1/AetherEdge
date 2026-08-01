//! Shared SHM-only fixtures for I/O integration tests.

#![allow(dead_code)] // Each test binary uses a subset of these fixtures.

use std::sync::Arc;

use aether_shm_bridge::{ChannelPointManifest, ShmRuntimeConfig, ShmWriterHandle};

pub const TEST_JWT_SECRET: &str = "0123456789abcdef0123456789abcdef";
pub const ADMIN_ACCESS_TOKEN: &str = "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJ1c2VyX2lkIjo3LCJyb2xlIjoiQWRtaW4iLCJ0eXBlIjoiYWNjZXNzIiwiaWF0IjoxNzAwMDAwMDAwLCJleHAiOjQxMDI0NDQ4MDB9.JtjQvDBo7j0bLOxwed6yC9-M9qFCloc4H2Dt0LjzF9E";

pub fn create_test_shm_handle() -> Arc<ShmWriterHandle> {
    let directory = tempfile::Builder::new()
        .prefix("aether-io-integration-shm-")
        .tempdir()
        .expect("create test SHM directory")
        .keep();
    let config = ShmRuntimeConfig::new(directory.join("io.shm"), 65_536);
    Arc::new(
        ShmWriterHandle::create_published(config, Arc::new(ChannelPointManifest::default()), None)
            .expect("compose typed SHM layout"),
    )
}

/// Send a prepared request and decode the JSON response.
pub async fn send(
    app: &axum::Router,
    req_builder: axum::http::request::Builder,
    body: Option<serde_json::Value>,
) -> anyhow::Result<(axum::http::StatusCode, serde_json::Value)> {
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    let (req_builder, body_bytes) = match body {
        Some(json_body) => (
            req_builder.header("content-type", "application/json"),
            serde_json::to_vec(&json_body)?,
        ),
        None => (req_builder, Vec::new()),
    };
    let request = req_builder.body(axum::body::Body::from(body_bytes))?;
    let response = app.clone().oneshot(request).await?;
    let status = response.status();
    let body_bytes = response.into_body().collect().await?.to_bytes();
    if body_bytes.is_empty() {
        return Ok((status, serde_json::json!({})));
    }
    let response_json = serde_json::from_slice(&body_bytes).map_err(|error| {
        anyhow::anyhow!(
            "invalid JSON response {:?}: {error}",
            String::from_utf8_lossy(&body_bytes)
        )
    })?;
    Ok((status, response_json))
}

/// Build the point-topology router over an existing configuration pool.
pub async fn create_test_app_with_pool(pool: sqlx::SqlitePool) -> anyhow::Result<axum::Router> {
    let channel_manager = Arc::new(aether_io::ChannelManager::new(
        create_test_shm_handle(),
        Arc::new(aether_routing::RoutingCache::new()),
    )?);
    let point_topology = Arc::new(aether_io::point_topology::PointTopologyApplication::new(
        pool.clone(),
        Arc::new(aether_store_local::MemoryAuditSink::new()),
    ));
    let authenticator = Arc::new(
        aether_auth_jwt::AccessTokenAuthenticator::new(TEST_JWT_SECRET)
            .expect("test authenticator"),
    );
    let router = aether_io::api::routes::create_api_routes_with_point_topology(
        channel_manager,
        pool,
        point_topology,
        authenticator,
    );
    Ok(router)
}
