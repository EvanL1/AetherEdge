//! Axum middleware that enforces JWT auth on protected routes.
//!
//! Accepts Bearer tokens in the `Authorization` header for REST calls.
//! Browser WebSocket upgrades to `/ws` may use `?token=...`; query-string
//! tokens are explicitly rejected everywhere else so credentials cannot leak
//! through normal REST URLs, access logs, or referrers.

use std::sync::Arc;

use axum::{
    Json,
    extract::{Request, State},
    http::{Method, StatusCode, header},
    middleware::Next,
    response::{IntoResponse, Response},
};
use serde_json::json;

use crate::auth::verify_access_token;
use crate::state::AppState;

fn extract_bearer(req: &Request) -> Option<String> {
    req.headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| {
            v.strip_prefix("Bearer ")
                .or_else(|| v.strip_prefix("bearer "))
        })
        .map(|s| s.to_string())
}

fn extract_query_token(req: &Request) -> Option<String> {
    // JWTs are base64url (A-Za-z0-9-_.) — URL-safe, no decoding needed.
    let q = req.uri().query()?;
    q.split('&')
        .find_map(|kv| kv.strip_prefix("token=").map(|s| s.to_string()))
}

fn has_query_token(req: &Request) -> bool {
    req.uri()
        .query()
        .is_some_and(|query| query.split('&').any(|pair| pair.starts_with("token=")))
}

fn is_websocket_upgrade(req: &Request) -> bool {
    if req.method() != Method::GET || req.uri().path() != "/ws" {
        return false;
    }
    let upgrade_is_websocket = req
        .headers()
        .get(header::UPGRADE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.eq_ignore_ascii_case("websocket"));
    let connection_has_upgrade = req
        .headers()
        .get(header::CONNECTION)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| {
            value
                .split(',')
                .any(|token| token.trim().eq_ignore_ascii_case("upgrade"))
        });
    upgrade_is_websocket && connection_has_upgrade
}

fn auth_error(status: StatusCode, code: &'static str, message: &'static str) -> Response {
    (
        status,
        Json(json!({
            "error": {
                "code": code,
                "message": message
            }
        })),
    )
        .into_response()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AuthFailure {
    QueryTokenNotAllowed,
    AuthenticationRequired,
    InvalidAccessToken,
}

impl IntoResponse for AuthFailure {
    fn into_response(self) -> Response {
        match self {
            Self::QueryTokenNotAllowed => auth_error(
                StatusCode::BAD_REQUEST,
                "QUERY_TOKEN_NOT_ALLOWED",
                "query-string tokens are allowed only for WebSocket upgrades",
            ),
            Self::AuthenticationRequired => auth_error(
                StatusCode::UNAUTHORIZED,
                "AUTHENTICATION_REQUIRED",
                "a Bearer access token is required",
            ),
            Self::InvalidAccessToken => auth_error(
                StatusCode::UNAUTHORIZED,
                "INVALID_ACCESS_TOKEN",
                "the access token is invalid or expired",
            ),
        }
    }
}

pub async fn require_jwt(
    State(state): State<Arc<AppState>>,
    mut req: Request,
    next: Next,
) -> Response {
    if let Err(error) = authenticate_request(&state, &mut req) {
        return error.into_response();
    }
    next.run(req).await
}

fn authenticate_request(state: &AppState, req: &mut Request) -> Result<(), AuthFailure> {
    let websocket_upgrade = is_websocket_upgrade(req);
    if has_query_token(req) && !websocket_upgrade {
        return Err(AuthFailure::QueryTokenNotAllowed);
    }

    let token = extract_bearer(req).or_else(|| {
        if websocket_upgrade {
            extract_query_token(req)
        } else {
            None
        }
    });

    let Some(token) = token else {
        return Err(AuthFailure::AuthenticationRequired);
    };

    let Some(claims) = verify_access_token(&token, &state.config.jwt_secret) else {
        return Err(AuthFailure::InvalidAccessToken);
    };

    req.extensions_mut().insert(claims);
    Ok(())
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::http::Request;

    use super::*;
    use crate::auth::Claims;
    use crate::test_support::{app_state, authorization_headers};

    #[test]
    fn query_tokens_are_classified_as_websocket_only() {
        let rest = Request::builder()
            .uri("/api/models?token=secret")
            .body(Body::empty())
            .expect("valid request");
        assert!(has_query_token(&rest));
        assert!(!is_websocket_upgrade(&rest));

        let websocket = Request::builder()
            .method(Method::GET)
            .uri("/ws?token=secret")
            .header(header::CONNECTION, "keep-alive, Upgrade")
            .header(header::UPGRADE, "websocket")
            .body(Body::empty())
            .expect("valid WebSocket request");
        assert!(has_query_token(&websocket));
        assert!(is_websocket_upgrade(&websocket));

        let spoofed_rest = Request::builder()
            .method(Method::GET)
            .uri("/api/models?token=secret")
            .header(header::CONNECTION, "Upgrade")
            .header(header::UPGRADE, "websocket")
            .body(Body::empty())
            .expect("valid request");
        assert!(!is_websocket_upgrade(&spoofed_rest));
    }

    #[tokio::test]
    async fn verified_claims_are_injected_and_rest_query_tokens_are_rejected() {
        let state = app_state().await;
        let mut authenticated = Request::builder()
            .uri("/api/models")
            .body(Body::empty())
            .expect("valid request");
        *authenticated.headers_mut() = authorization_headers("Engineer");

        authenticate_request(&state, &mut authenticated).expect("valid Bearer token");
        let claims = authenticated
            .extensions()
            .get::<Claims>()
            .expect("verified claims must be injected");
        assert_eq!(claims.role.as_deref(), Some("Engineer"));

        let mut leaked = Request::builder()
            .uri("/api/models?token=leaked")
            .body(Body::empty())
            .expect("valid request");
        *leaked.headers_mut() = authorization_headers("Admin");
        let response = authenticate_request(&state, &mut leaked)
            .expect_err("REST query tokens must be rejected even with a valid Bearer token")
            .into_response();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }
}
