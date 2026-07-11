//! Axum middleware that enforces JWT auth on protected routes.
//!
//! Accepts Bearer tokens in the `Authorization` header for REST calls.
//! WebSocket upgrades cannot set custom headers from a browser, so the
//! middleware also accepts `?token=...` as a query-string fallback.

use std::sync::Arc;

use axum::{
    extract::{Request, State},
    http::StatusCode,
    middleware::Next,
    response::{IntoResponse, Response},
};

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

pub async fn require_jwt(State(state): State<Arc<AppState>>, req: Request, next: Next) -> Response {
    let token = extract_bearer(&req).or_else(|| extract_query_token(&req));

    let Some(token) = token else {
        return (StatusCode::UNAUTHORIZED, "missing token").into_response();
    };

    if verify_access_token(&token, &state.config.jwt_secret).is_none() {
        return (StatusCode::UNAUTHORIZED, "invalid token").into_response();
    }

    next.run(req).await
}
