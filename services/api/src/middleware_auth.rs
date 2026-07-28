//! Axum middleware that enforces JWT auth on protected routes.
//!
//! Accepts access JWTs only in the `Authorization: Bearer` header. Query-string
//! credentials are not a supported transport.

use std::sync::Arc;

use axum::{
    Json,
    extract::{Request, State},
    http::StatusCode,
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
    AuthenticationRequired,
    InvalidAccessToken,
}

impl IntoResponse for AuthFailure {
    fn into_response(self) -> Response {
        match self {
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
    let Some(token) = extract_bearer(req) else {
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

    #[tokio::test]
    async fn verified_bearer_claims_are_injected() {
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
    }

    #[tokio::test]
    async fn query_tokens_do_not_authenticate_requests() {
        let state = app_state().await;
        let mut request = Request::builder()
            .uri("/api/models?token=leaked")
            .body(Body::empty())
            .expect("valid request");
        let response = authenticate_request(&state, &mut request)
            .expect_err("query tokens are not credentials")
            .into_response();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }
}
