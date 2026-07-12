use std::sync::Arc;

use axum::{Json, extract::State, response::IntoResponse};
use serde_json::json;
use tracing::error;

use crate::state::AppState;

// ── POST /api/v1/broadcast ────────────────────────────────────────────────────

/// Broadcast a JSON message to all connected WebSocket clients.
///
/// Forwards the request body verbatim to every currently connected WebSocket
/// client with **no subscription filtering** — even clients subscribed to a
/// specific channel will receive the message. Returns the number of clients
/// reached and their metadata. Useful for pushing system notifications, forcing
/// a frontend cache refresh, or debugging the WebSocket pipeline.
#[utoipa::path(post, path = "/api/v1/broadcast", tag = "WebSocket",
    security(("bearer_auth" = [])),
    request_body(content = serde_json::Value, description = "Arbitrary JSON payload to broadcast to all connected WebSocket clients"),
    responses((status = 200, description = "Broadcast delivered", body = crate::models::GatewayDataResponse<serde_json::Value>)))]
pub async fn broadcast_message(
    State(state): State<Arc<AppState>>,
    Json(body): Json<serde_json::Value>,
) -> impl IntoResponse {
    let msg = match serde_json::to_string(&body) {
        Ok(s) => s,
        Err(e) => {
            error!("Serialize broadcast body error: {}", e);
            return Json(json!({"success": false, "message": "Invalid JSON data"})).into_response();
        },
    };

    let (count, clients) = state.ws_hub.broadcast(&msg);

    Json(json!({
        "success": true,
        "message": format!("Message broadcast to {} client(s)", count),
        "data": {
            "client_count": count,
            "clients": clients,
            "broadcast_data": body,
        }
    }))
    .into_response()
}

// ── GET /api/v1/broadcast/status ─────────────────────────────────────────────

/// Return the current connection status of the WebSocket hub.
///
/// Reports total connection count, subscribed-client count (clients with at
/// least one channel or data_type subscription), per-connection metadata
/// (client_id, connect time), and the full subscription table. Useful for
/// diagnosing why a client is not receiving push events: check whether the
/// connection exists and whether its subscription matches the pushed data.
#[utoipa::path(get, path = "/api/v1/broadcast/status", tag = "WebSocket",
    security(("bearer_auth" = [])),
    responses((status = 200, description = "WebSocket hub connection status", body = crate::models::GatewayDataResponse<serde_json::Value>)))]
pub async fn broadcast_status(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let status = state.ws_hub.get_status();

    let subscribed_count = status["subscriptions"]
        .as_object()
        .map(|m| {
            m.values()
                .filter(|v| {
                    v["channels"]
                        .as_array()
                        .map(|a| !a.is_empty())
                        .unwrap_or(false)
                        || v["data_types"]
                            .as_array()
                            .map(|a| !a.is_empty())
                            .unwrap_or(false)
                })
                .count()
        })
        .unwrap_or(0);

    Json(json!({
        "success": true,
        "message": "OK",
        "data": {
            "websocket_available": true,
            "connection_count": status["connection_count"],
            "subscribed_count": subscribed_count,
            "connections": status["connections_info"],
            "subscriptions": status["subscriptions"],
        }
    }))
    .into_response()
}
