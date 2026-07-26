//! Gateway-owned OpenAPI document proxy and path normalization.
//!
//! Internal services publish a loopback-only `/openapi.json`. The remote API
//! gateway is the sole Swagger UI owner and rewrites each service document so
//! Swagger's "Try it out" requests use the authenticated gateway namespace.

use std::sync::Arc;

use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde_json::{Map, Value, json};

use crate::{service_gateway::ServiceName, state::AppState};

/// Returns one service document through the remote gateway.
///
/// This route exists only with the gateway `swagger-ui` feature. It never
/// exposes an arbitrary upstream URL: the path selects one fixed loopback
/// service from [`ServiceName`].
pub(crate) async fn service_openapi(
    State(state): State<Arc<AppState>>,
    Path(document_name): Path<String>,
) -> Response {
    let Some(service_name) = document_name.strip_suffix(".json") else {
        return document_error(
            StatusCode::NOT_FOUND,
            "OPENAPI_DOCUMENT_NOT_FOUND",
            "the requested OpenAPI document does not exist",
        );
    };
    let Some(service) = ServiceName::from_openapi_name(service_name) else {
        return document_error(
            StatusCode::NOT_FOUND,
            "OPENAPI_DOCUMENT_NOT_FOUND",
            "the requested OpenAPI document does not exist",
        );
    };

    let upstream = format!(
        "{}/openapi.json",
        service.base_url(&state.config).trim_end_matches('/')
    );
    let response = match state.service_client.get(upstream).send().await {
        Ok(response) => response,
        Err(_) => {
            return document_error(
                StatusCode::BAD_GATEWAY,
                "OPENAPI_UPSTREAM_UNAVAILABLE",
                "the service OpenAPI document is unavailable",
            );
        },
    };
    if !response.status().is_success() {
        return document_error(
            StatusCode::BAD_GATEWAY,
            "OPENAPI_UPSTREAM_UNAVAILABLE",
            "the service OpenAPI document is unavailable",
        );
    }

    let bytes = match response.bytes().await {
        Ok(bytes) => bytes,
        Err(_) => {
            return document_error(
                StatusCode::BAD_GATEWAY,
                "OPENAPI_UPSTREAM_INVALID",
                "the service returned an unreadable OpenAPI document",
            );
        },
    };
    let mut document: Value = match serde_json::from_slice(&bytes) {
        Ok(document) => document,
        Err(_) => {
            return document_error(
                StatusCode::BAD_GATEWAY,
                "OPENAPI_UPSTREAM_INVALID",
                "the service returned an invalid OpenAPI document",
            );
        },
    };
    if normalize_for_gateway(service, &mut document).is_err() {
        return document_error(
            StatusCode::BAD_GATEWAY,
            "OPENAPI_UPSTREAM_INVALID",
            "the service returned an invalid OpenAPI document",
        );
    }

    Json(document).into_response()
}

fn document_error(status: StatusCode, code: &str, message: &str) -> Response {
    (
        status,
        Json(json!({
            "error": {
                "code": code,
                "message": message,
            }
        })),
    )
        .into_response()
}

/// Rewrites a service-local document into the fixed remote-gateway namespace.
///
/// Health probes, root routes, and service-local administration are deliberately
/// omitted: they are not supported remote application APIs. The remaining
/// paths preserve their service-owned schemas and security declarations.
fn normalize_for_gateway(service: ServiceName, document: &mut Value) -> Result<(), ()> {
    let paths = document
        .get_mut("paths")
        .and_then(Value::as_object_mut)
        .ok_or(())?;
    let original_paths = std::mem::take(paths);
    let mut gateway_paths = Map::new();
    for (path, item) in original_paths {
        if let Some(path) = gateway_path(service, &path) {
            gateway_paths.insert(path, item);
        }
    }
    *paths = gateway_paths;

    let document = document.as_object_mut().ok_or(())?;
    document.insert(
        "servers".to_string(),
        json!([{
            "url": gateway_prefix(service),
            "description": "Authenticated Aether API gateway namespace"
        }]),
    );
    Ok(())
}

fn gateway_path(service: ServiceName, path: &str) -> Option<String> {
    if path == "/api/admin" || path.starts_with("/api/admin/") {
        return None;
    }

    match service {
        ServiceName::Io | ServiceName::Automation => Some(path.to_string()),
        ServiceName::History => strip_service_prefix(path, "/hisApi"),
        ServiceName::Uplink => strip_service_prefix(path, "/netApi"),
        ServiceName::Alarm => strip_service_prefix(path, "/alarmApi"),
    }
}

fn strip_service_prefix(path: &str, prefix: &str) -> Option<String> {
    let suffix = path.strip_prefix(prefix)?;
    if suffix.is_empty() {
        Some("/".to_string())
    } else if suffix.starts_with('/') {
        Some(suffix.to_string())
    } else {
        None
    }
}

fn gateway_prefix(service: ServiceName) -> &'static str {
    match service {
        ServiceName::Io => "/api/v1/io",
        ServiceName::Automation => "/api/v1/automation",
        ServiceName::History => "/api/v1/history",
        ServiceName::Uplink => "/api/v1/uplink",
        ServiceName::Alarm => "/api/v1/alarm",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn history_document_is_rebased_to_the_gateway_namespace() {
        let mut document = json!({
            "paths": {
                "/hisApi/data/query": { "get": {} },
                "/hisApi/storage": { "put": {} },
                "/ping": { "get": {} },
                "/api/admin/logs/files": { "get": {} }
            }
        });

        normalize_for_gateway(ServiceName::History, &mut document).expect("valid document");

        assert!(document["paths"]["/data/query"].is_object());
        assert!(document["paths"]["/storage"].is_object());
        assert!(document["paths"]["/ping"].is_null());
        assert!(document["paths"]["/api/admin/logs/files"].is_null());
        assert_eq!(document["servers"][0]["url"], "/api/v1/history");
    }

    #[test]
    fn io_document_keeps_gateway_supported_paths_and_drops_admin() {
        let mut document = json!({
            "paths": {
                "/health": { "get": {} },
                "/api/channels": { "get": {} },
                "/api/admin/logs/files": { "get": {} }
            }
        });

        normalize_for_gateway(ServiceName::Io, &mut document).expect("valid document");

        assert!(document["paths"]["/health"].is_object());
        assert!(document["paths"]["/api/channels"].is_object());
        assert!(document["paths"]["/api/admin/logs/files"].is_null());
        assert_eq!(document["servers"][0]["url"], "/api/v1/io");
    }
}
