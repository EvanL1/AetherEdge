//! HTTP API for simulator state observability during E2E testing.

use std::sync::Arc;

use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::get,
};
use serde_json::{Map, Value, json};

use crate::state_machine::StateMachineStore;

type SharedStore = Arc<StateMachineStore>;

pub async fn run_http_server(addr: &str, sm_store: Arc<StateMachineStore>) -> anyhow::Result<()> {
    let app = Router::new()
        .route("/health", get(health))
        .route("/state", get(all_states))
        .route("/state/{unit_id}", get(single_state))
        .with_state(sm_store);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!("simulator HTTP API listening on {addr}");
    axum::serve(listener, app).await?;
    Ok(())
}

async fn health() -> impl IntoResponse {
    Json(json!({"status": "ok"}))
}

async fn all_states(State(store): State<SharedStore>) -> impl IntoResponse {
    let mut map = Map::new();
    for (unit_id, sm) in store.iter() {
        map.insert(
            unit_id.to_string(),
            Value::String(sm.current_state().as_str().to_string()),
        );
    }
    Json(Value::Object(map))
}

async fn single_state(
    State(store): State<SharedStore>,
    Path(unit_id): Path<u8>,
) -> impl IntoResponse {
    match store.get(&unit_id) {
        Some(sm) => {
            let body = json!({"unit_id": unit_id, "state": sm.current_state().as_str()});
            (StatusCode::OK, Json(body)).into_response()
        },
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state_machine::{DeviceState, StateMachine};
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt; // for `oneshot`

    fn test_app() -> Router {
        let mut store = crate::state_machine::StateMachineStore::new();
        store.insert(1, Arc::new(StateMachine::new(DeviceState::Standby, vec![])));
        store.insert(2, Arc::new(StateMachine::new(DeviceState::Running, vec![])));
        Router::new()
            .route("/health", get(health))
            .route("/state", get(all_states))
            .route("/state/{unit_id}", get(single_state))
            .with_state(Arc::new(store))
    }

    #[tokio::test]
    async fn test_health() {
        let app = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_all_states() {
        let app = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/state")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["1"], "standby");
        assert_eq!(json["2"], "running");
    }

    #[tokio::test]
    async fn test_single_state_found() {
        let app = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/state/1")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_single_state_not_found() {
        let app = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/state/99")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }
}
