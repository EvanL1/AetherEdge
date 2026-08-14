use std::sync::Arc;

use axum::{
    Json,
    extract::State,
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
};
use chrono::Utc;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use tracing::error;

use crate::db::{AlarmEventAcceptance, AlarmEventAcceptanceError};
use crate::state::AppState;

const MAX_EVENT_ID_BYTES: usize = 128;
const MAX_ENVELOPE_ID_BYTES: usize = 128;
const INTERNAL_AUTH_SCHEME: &str = "Bearer";

// ── POST /api/broadcast ───────────────────────────────────────────────────────

/// Broadcast a JSON message to all connected WebSocket clients.
///
/// This is the operator/JWT surface. The alarm service credential is accepted
/// only by `/api/internal/alarm-events` and cannot authorize this generic
/// broadcast endpoint.
#[utoipa::path(post, path = "/api/broadcast", tag = "WebSocket",
    security(("bearer_auth" = [])),
    request_body(content = serde_json::Value, description = "Arbitrary JSON payload to broadcast to all connected WebSocket clients"),
    responses((status = 200, description = "Broadcast delivered", body = crate::models::GatewayDataResponse<serde_json::Value>)))]
pub async fn broadcast_message(
    State(state): State<Arc<AppState>>,
    Json(body): Json<Value>,
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

// ── POST /api/internal/alarm-events ──────────────────────────────────────────

/// Exact service-to-service ingress for alarm transitions and snapshots.
///
/// This route is intentionally omitted from the public operator OpenAPI. It
/// accepts only the dedicated alarm credential and validates the complete
/// event-id contract before any durable or WebSocket side effect.
pub async fn receive_alarm_event(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    if !valid_service_authorization(&headers, &state.config.alarm_broadcast_token) {
        return internal_error(
            StatusCode::UNAUTHORIZED,
            "INVALID_SERVICE_CREDENTIAL",
            "invalid alarm service credentials",
        );
    }

    let incoming = match validate_internal_envelope(&headers, &body) {
        Ok(incoming) => incoming,
        Err(message) => {
            return internal_error(StatusCode::BAD_REQUEST, "INVALID_ALARM_ENVELOPE", message);
        },
    };
    let msg = match serde_json::to_string(&body) {
        Ok(msg) => msg,
        Err(error) => {
            error!("Serialize validated alarm envelope: {error}");
            return internal_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "ALARM_SERIALIZATION_FAILED",
                "failed to serialize validated alarm envelope",
            );
        },
    };

    match incoming {
        InternalAlarmEnvelope::Transition { event_id } => {
            let canonical = match serde_json_canonicalizer::to_vec(&body) {
                Ok(canonical) => canonical,
                Err(error) => {
                    error!("Canonicalize validated alarm envelope: {error}");
                    return internal_error(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "ALARM_CANONICALIZATION_FAILED",
                        "failed to canonicalize validated alarm envelope",
                    );
                },
            };
            let payload_sha256 = format!("{:x}", Sha256::digest(canonical));
            let _delivery_guard = state.ws_hub.alarm_event_delivery_guard().await;
            let acceptance = crate::db::accept_alarm_event(
                &state.db,
                event_id,
                &payload_sha256,
                Utc::now().timestamp_millis(),
            )
            .await;
            let retry = match acceptance {
                Ok(AlarmEventAcceptance::Pending { retry }) => retry,
                Ok(AlarmEventAcceptance::Delivered) => {
                    return Json(json!({
                        "success": true,
                        "message": "Alarm event was already delivered",
                        "data": {
                            "event_id": event_id,
                            "retry": true,
                            "client_count": 0,
                            "already_seen_client_count": 0,
                            "clients": [],
                            "broadcast_data": body,
                        }
                    }))
                    .into_response();
                },
                Err(AlarmEventAcceptanceError::Conflict) => {
                    return internal_error(
                        StatusCode::CONFLICT,
                        "ALARM_EVENT_ID_CONFLICT",
                        "event_id is already associated with a different payload",
                    );
                },
                Err(AlarmEventAcceptanceError::Storage(error)) => {
                    error!(event_id, "Persist alarm event ledger: {error}");
                    return internal_error(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "ALARM_LEDGER_UNAVAILABLE",
                        "alarm event ledger is unavailable",
                    );
                },
            };

            // The durable ledger is committed first. A crash before this call
            // returns no HTTP success and the Alarm outbox retries. A crash
            // after queuing disconnects every old WS generation; a retry in the
            // same process is suppressed by connection-local pending markers.
            let (count, clients, already_seen) = state.ws_hub.broadcast_event_once(event_id, &msg);
            if let Err(error) = crate::db::mark_alarm_event_delivered(
                &state.db,
                event_id,
                &payload_sha256,
                Utc::now().timestamp_millis(),
            )
            .await
            {
                error!(event_id, "Mark alarm event delivered: {error}");
                // Keep pending markers until SQLite acknowledges delivery. A
                // same-process retry skips those queues; process restart first
                // disconnects every marked connection generation.
                return internal_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "ALARM_DELIVERY_ACK_FAILED",
                    "alarm event delivery acknowledgement failed",
                );
            }
            state.ws_hub.finish_event_delivery(event_id);
            Json(json!({
                "success": true,
                "message": format!("Alarm event accepted for {} client(s)", count),
                "data": {
                    "event_id": event_id,
                    "retry": retry,
                    "client_count": count,
                    "already_seen_client_count": already_seen,
                    "clients": clients,
                    "broadcast_data": body,
                }
            }))
            .into_response()
        },
        InternalAlarmEnvelope::Replay { event_id } => {
            let (count, clients) = state.ws_hub.broadcast(&msg);
            Json(json!({
                "success": true,
                "message": format!("Alarm replay broadcast to {} client(s)", count),
                "data": {
                    "event_id": event_id,
                    "replay": true,
                    "client_count": count,
                    "clients": clients,
                    "broadcast_data": body,
                }
            }))
            .into_response()
        },
        InternalAlarmEnvelope::CountSnapshot => {
            let (count, clients) = state.ws_hub.broadcast(&msg);
            Json(json!({
                "success": true,
                "message": format!("Alarm count snapshot broadcast to {} client(s)", count),
                "data": {
                    "client_count": count,
                    "clients": clients,
                    "broadcast_data": body,
                }
            }))
            .into_response()
        },
    }
}

enum InternalAlarmEnvelope<'a> {
    Transition { event_id: &'a str },
    Replay { event_id: &'a str },
    CountSnapshot,
}

fn validate_internal_envelope<'a>(
    headers: &HeaderMap,
    body: &'a Value,
) -> Result<InternalAlarmEnvelope<'a>, &'static str> {
    let event_header = single_header(headers, "X-Aether-Event-ID")?;
    let idempotency_key = single_header(headers, "Idempotency-Key")?;
    let replay = match single_header(headers, "X-Aether-Replay")? {
        None => false,
        Some("true") => true,
        Some(_) => return Err("X-Aether-Replay must be exactly 'true' when present"),
    };

    let object = body
        .as_object()
        .ok_or("alarm envelope must be a JSON object")?;
    let event_type = object
        .get("type")
        .and_then(Value::as_str)
        .ok_or("alarm envelope type is required")?;
    validate_bounded_text(
        object.get("id"),
        "alarm envelope id is invalid",
        MAX_ENVELOPE_ID_BYTES,
    )?;
    object
        .get("timestamp")
        .and_then(Value::as_i64)
        .ok_or("alarm envelope timestamp must be an integer")?;
    let data = object
        .get("data")
        .and_then(Value::as_object)
        .ok_or("alarm envelope data must be an object")?;

    match event_type {
        "alarm" => {
            require_exact_keys(
                object,
                &["type", "id", "event_id", "timestamp", "data"],
                "alarm envelope contains missing or unexpected fields",
            )?;
            require_exact_keys(
                data,
                &[
                    "alarm_id",
                    "event_id",
                    "service_type",
                    "source",
                    "device",
                    "channel_id",
                    "data_type",
                    "point_id",
                    "status",
                    "level",
                    "value",
                    "message",
                ],
                "alarm data contains missing or unexpected fields",
            )?;
            let payload_event_id = object
                .get("event_id")
                .and_then(Value::as_str)
                .ok_or("alarm event_id is required")?;
            validate_event_id(payload_event_id)?;
            let data_event_id = data
                .get("event_id")
                .and_then(Value::as_str)
                .ok_or("alarm data.event_id is required")?;
            let header_event_id = event_header.ok_or("X-Aether-Event-ID is required")?;
            if payload_event_id != data_event_id || payload_event_id != header_event_id {
                return Err("X-Aether-Event-ID and payload event_id fields must match");
            }
            validate_bounded_text(
                data.get("alarm_id"),
                "alarm_id is invalid",
                MAX_EVENT_ID_BYTES,
            )?;
            for (field, max_bytes) in [
                ("service_type", 128),
                ("source", 128),
                ("device", 128),
                ("data_type", 128),
                ("message", 4_096),
            ] {
                validate_bounded_text(data.get(field), "alarm text field is invalid", max_bytes)?;
            }
            let service_type = data["service_type"]
                .as_str()
                .ok_or("service_type must be text")?;
            if data["source"].as_str() != Some(service_type) {
                return Err("alarm source must match service_type");
            }
            let channel_id = data["channel_id"]
                .as_i64()
                .ok_or("alarm channel_id must be an integer")?;
            if data["device"].as_str() != Some(channel_id.to_string().as_str()) {
                return Err("alarm device must match channel_id");
            }
            data["point_id"]
                .as_i64()
                .ok_or("alarm point_id must be an integer")?;
            data["level"]
                .as_i64()
                .ok_or("alarm level must be an integer")?;
            let value = data["value"]
                .as_f64()
                .ok_or("alarm value must be a number")?;
            if !value.is_finite() {
                return Err("alarm value must be finite");
            }
            match data.get("status").and_then(Value::as_u64) {
                Some(0 | 1) => {},
                _ => return Err("alarm status must be 0 or 1"),
            }

            if replay {
                if idempotency_key.is_some() {
                    return Err("alarm replay must not include Idempotency-Key");
                }
                Ok(InternalAlarmEnvelope::Replay {
                    event_id: payload_event_id,
                })
            } else {
                let key = idempotency_key.ok_or("Idempotency-Key is required")?;
                if key != payload_event_id {
                    return Err("Idempotency-Key and event_id must match");
                }
                Ok(InternalAlarmEnvelope::Transition {
                    event_id: payload_event_id,
                })
            }
        },
        "alarm_num" => {
            require_exact_keys(
                object,
                &["type", "id", "timestamp", "data"],
                "alarm count envelope contains missing or unexpected fields",
            )?;
            require_exact_keys(
                data,
                &["current_alarms", "1", "2", "3", "update_time", "server_id"],
                "alarm count data contains missing or unexpected fields",
            )?;
            if replay || event_header.is_some() || idempotency_key.is_some() {
                return Err("alarm count snapshot must not include event identity headers");
            }
            for field in ["current_alarms", "1", "2", "3", "update_time"] {
                data.get(field)
                    .and_then(Value::as_i64)
                    .ok_or("alarm count snapshot contains an invalid numeric field")?;
            }
            validate_bounded_text(
                data.get("server_id"),
                "alarm count server_id is invalid",
                MAX_EVENT_ID_BYTES,
            )?;
            Ok(InternalAlarmEnvelope::CountSnapshot)
        },
        _ => Err("internal alarm route accepts only alarm and alarm_num envelopes"),
    }
}

fn require_exact_keys(
    object: &serde_json::Map<String, Value>,
    expected: &[&str],
    message: &'static str,
) -> Result<(), &'static str> {
    if object.len() != expected.len() || expected.iter().any(|key| !object.contains_key(*key)) {
        return Err(message);
    }
    Ok(())
}

fn validate_event_id(value: &str) -> Result<(), &'static str> {
    if value.is_empty()
        || value.len() > MAX_EVENT_ID_BYTES
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':'))
    {
        return Err("event_id contains invalid characters or length");
    }
    Ok(())
}

fn validate_bounded_text(
    value: Option<&Value>,
    message: &'static str,
    max_bytes: usize,
) -> Result<(), &'static str> {
    match value.and_then(Value::as_str) {
        Some(text) if !text.is_empty() && text.len() <= max_bytes => Ok(()),
        _ => Err(message),
    }
}

fn single_header<'a>(
    headers: &'a HeaderMap,
    name: &'static str,
) -> Result<Option<&'a str>, &'static str> {
    let mut values = headers.get_all(name).iter();
    let Some(first) = values.next() else {
        return Ok(None);
    };
    if values.next().is_some() {
        return Err("identity headers must appear exactly once");
    }
    first
        .to_str()
        .map(Some)
        .map_err(|_| "identity headers must contain visible ASCII")
}

fn valid_service_authorization(headers: &HeaderMap, expected: &str) -> bool {
    let Ok(Some(authorization)) = single_header(headers, header::AUTHORIZATION.as_str()) else {
        return false;
    };
    let Some((scheme, credential)) = authorization.split_once(' ') else {
        return false;
    };
    if scheme != INTERNAL_AUTH_SCHEME
        || credential.is_empty()
        || credential.bytes().any(|byte| byte.is_ascii_whitespace())
    {
        return false;
    }
    let presented = Sha256::digest(credential.as_bytes());
    let configured = Sha256::digest(expected.as_bytes());
    presented.ct_eq(&configured).unwrap_u8() == 1
}

fn internal_error(status: StatusCode, code: &'static str, message: &'static str) -> Response {
    (
        status,
        Json(json!({
            "success": false,
            "message": message,
            "error": { "code": code }
        })),
    )
        .into_response()
}

// ── GET /api/broadcast/status ────────────────────────────────────────────────

/// Return the current connection and durable alarm-event ledger status.
#[utoipa::path(get, path = "/api/broadcast/status", tag = "WebSocket",
    security(("bearer_auth" = [])),
    responses((status = 200, description = "WebSocket hub connection status", body = crate::models::GatewayDataResponse<serde_json::Value>)))]
pub async fn broadcast_status(State(state): State<Arc<AppState>>) -> Response {
    let status = state.ws_hub.get_status();
    let ledger = match crate::db::alarm_event_ledger_stats(&state.db).await {
        Ok(ledger) => ledger,
        Err(error) => {
            error!("Read alarm event ledger status: {error}");
            return internal_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "ALARM_LEDGER_UNAVAILABLE",
                "alarm event ledger status is unavailable",
            );
        },
    };

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
            "alarm_event_ledger": ledger,
        }
    }))
    .into_response()
}

#[cfg(test)]
mod tests {
    use axum::Router;
    use axum::body::{Body, to_bytes};
    use axum::http::Request;
    use sqlx::sqlite::SqlitePoolOptions;
    use tower::ServiceExt;

    use super::*;
    use crate::test_support::{app_state, app_state_with_database, authorization_headers};

    const EVENT_ID: &str = "alarm-trigger-41";

    fn alarm_payload(event_id: &str, value: f64) -> Value {
        json!({
            "type": "alarm",
            "id": "alarm_041",
            "event_id": event_id,
            "timestamp": 1_700_000_000,
            "data": {
                "alarm_id": "41",
                "event_id": event_id,
                "service_type": "io",
                "source": "io",
                "device": "7",
                "channel_id": 7,
                "data_type": "T",
                "point_id": 3,
                "status": 1,
                "level": 2,
                "value": value,
                "message": "temperature threshold exceeded"
            }
        })
    }

    fn internal_request(
        token: &str,
        payload: &Value,
        event_id: Option<&str>,
        replay: bool,
    ) -> Request<Body> {
        let mut builder = Request::post("/api/internal/alarm-events")
            .header("content-type", "application/json")
            .header("authorization", format!("Bearer {token}"));
        if let Some(event_id) = event_id {
            builder = builder.header("X-Aether-Event-ID", event_id);
            if !replay {
                builder = builder.header("Idempotency-Key", event_id);
            }
        }
        if replay {
            builder = builder.header("X-Aether-Replay", "true");
        }
        builder
            .body(Body::from(payload.to_string()))
            .expect("internal alarm request")
    }

    async fn response_json(response: Response) -> Value {
        serde_json::from_slice(
            &to_bytes(response.into_body(), usize::MAX)
                .await
                .expect("response body"),
        )
        .expect("response JSON")
    }

    fn app(state: Arc<AppState>) -> Router {
        crate::build_router(state)
    }

    #[tokio::test]
    async fn same_event_and_payload_is_durable_and_queued_once_per_live_connection() {
        let state = app_state().await;
        let mut client = state.ws_hub.register_test_client("alarm-ui");
        let payload = alarm_payload(EVENT_ID, 95.0);
        let token = state.config.alarm_broadcast_token.clone();
        let router = app(Arc::clone(&state));

        let first = router
            .clone()
            .oneshot(internal_request(&token, &payload, Some(EVENT_ID), false))
            .await
            .expect("first response");
        assert_eq!(first.status(), StatusCode::OK);
        let first_body = response_json(first).await;
        assert_eq!(first_body["data"]["retry"], false);
        assert_eq!(first_body["data"]["client_count"], 1);
        assert_eq!(
            client.recv().await.as_deref(),
            Some(payload.to_string().as_str())
        );

        let retry = router
            .oneshot(internal_request(&token, &payload, Some(EVENT_ID), false))
            .await
            .expect("retry response");
        assert_eq!(retry.status(), StatusCode::OK);
        let retry_body = response_json(retry).await;
        assert_eq!(retry_body["data"]["retry"], true);
        assert_eq!(retry_body["data"]["client_count"], 0);
        assert_eq!(retry_body["data"]["already_seen_client_count"], 0);
        assert!(client.try_recv().is_err());

        let stats = crate::db::alarm_event_ledger_stats(&state.db)
            .await
            .expect("ledger stats");
        assert_eq!((stats.durable_events, stats.duplicate_requests), (1, 1));
    }

    #[tokio::test]
    async fn concurrent_same_event_requests_have_one_queue_winner() {
        let state = app_state().await;
        let mut client = state.ws_hub.register_test_client("concurrent-ui");
        let payload = alarm_payload(EVENT_ID, 95.0);
        let token = state.config.alarm_broadcast_token.clone();
        let router = app(Arc::clone(&state));

        let (first, second) = tokio::join!(
            router
                .clone()
                .oneshot(internal_request(&token, &payload, Some(EVENT_ID), false)),
            router.oneshot(internal_request(&token, &payload, Some(EVENT_ID), false)),
        );
        assert_eq!(first.expect("first response").status(), StatusCode::OK);
        assert_eq!(second.expect("second response").status(), StatusCode::OK);
        assert_eq!(
            client.recv().await.as_deref(),
            Some(payload.to_string().as_str())
        );
        assert!(client.try_recv().is_err());
        let stats = crate::db::alarm_event_ledger_stats(&state.db)
            .await
            .expect("ledger stats");
        assert_eq!((stats.durable_events, stats.duplicate_requests), (1, 1));
    }

    #[tokio::test]
    async fn same_event_id_with_different_payload_is_conflict_without_broadcast() {
        let state = app_state().await;
        let mut client = state.ws_hub.register_test_client("conflict-ui");
        let token = state.config.alarm_broadcast_token.clone();
        let router = app(Arc::clone(&state));
        let first_payload = alarm_payload(EVENT_ID, 95.0);
        router
            .clone()
            .oneshot(internal_request(
                &token,
                &first_payload,
                Some(EVENT_ID),
                false,
            ))
            .await
            .expect("first response");
        assert!(client.recv().await.is_some());

        let conflicting = router
            .oneshot(internal_request(
                &token,
                &alarm_payload(EVENT_ID, 96.0),
                Some(EVENT_ID),
                false,
            ))
            .await
            .expect("conflict response");
        assert_eq!(conflicting.status(), StatusCode::CONFLICT);
        assert!(client.try_recv().is_err());
    }

    #[tokio::test]
    async fn event_identity_headers_and_payload_must_match_exactly() {
        let state = app_state().await;
        let token = state.config.alarm_broadcast_token.clone();
        let payload = alarm_payload(EVENT_ID, 95.0);
        let response = app(state)
            .oneshot(internal_request(
                &token,
                &payload,
                Some("alarm-trigger-42"),
                false,
            ))
            .await
            .expect("mismatch response");
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn dedicated_service_token_is_scoped_to_the_exact_internal_route() {
        let state = app_state().await;
        let token = state.config.alarm_broadcast_token.clone();
        let payload = alarm_payload(EVENT_ID, 95.0);
        let router = app(Arc::clone(&state));

        let unauthorized = router
            .clone()
            .oneshot(internal_request(
                "wrong-alarm-token-0123456789abcdef",
                &payload,
                Some(EVENT_ID),
                false,
            ))
            .await
            .expect("unauthorized response");
        assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);

        let generic = Request::post("/api/broadcast")
            .header("content-type", "application/json")
            .header("authorization", format!("Bearer {token}"))
            .body(Body::from(json!({"type": "arbitrary"}).to_string()))
            .expect("generic broadcast request");
        let rejected = router.oneshot(generic).await.expect("generic response");
        assert_eq!(rejected.status(), StatusCode::UNAUTHORIZED);

        let mut operator = Request::post("/api/broadcast")
            .header("content-type", "application/json")
            .body(Body::from(json!({"type": "operator"}).to_string()))
            .expect("operator request");
        *operator.headers_mut() = {
            let mut headers = authorization_headers("Engineer");
            headers.insert(
                "content-type",
                "application/json".parse().expect("content type"),
            );
            headers
        };
        let accepted = app(state)
            .oneshot(operator)
            .await
            .expect("operator response");
        assert_eq!(accepted.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn explicit_replay_is_not_ledger_deduplicated() {
        let state = app_state().await;
        let mut client = state.ws_hub.register_test_client("replay-ui");
        let token = state.config.alarm_broadcast_token.clone();
        let payload = alarm_payload(EVENT_ID, 95.0);
        let router = app(Arc::clone(&state));

        for _ in 0..2 {
            let response = router
                .clone()
                .oneshot(internal_request(&token, &payload, Some(EVENT_ID), true))
                .await
                .expect("replay response");
            assert_eq!(response.status(), StatusCode::OK);
            assert_eq!(
                client.recv().await.as_deref(),
                Some(payload.to_string().as_str())
            );
        }
        assert_eq!(
            crate::db::alarm_event_ledger_stats(&state.db)
                .await
                .expect("ledger stats")
                .durable_events,
            0
        );
    }

    #[tokio::test]
    async fn delivered_event_retry_after_restart_does_not_rebroadcast() {
        let directory = tempfile::tempdir().expect("temporary database directory");
        let path = directory.path().join("api.db");
        let first_pool = SqlitePoolOptions::new()
            .max_connections(3)
            .connect_with(common::bootstrap_database::sqlite_connect_options(
                path.to_str().expect("UTF-8 database path"),
            ))
            .await
            .expect("first API database pool");
        let first_state = app_state_with_database(first_pool, false).await;
        let token = first_state.config.alarm_broadcast_token.clone();
        let payload = alarm_payload(EVENT_ID, 95.0);
        let first = app(Arc::clone(&first_state))
            .oneshot(internal_request(&token, &payload, Some(EVENT_ID), false))
            .await
            .expect("first acceptance");
        assert_eq!(first.status(), StatusCode::OK);
        drop(first_state);

        let second_pool = SqlitePoolOptions::new()
            .max_connections(3)
            .connect_with(common::bootstrap_database::sqlite_connect_options(
                path.to_str().expect("UTF-8 database path"),
            ))
            .await
            .expect("restarted API database pool");
        let second_state = app_state_with_database(second_pool, false).await;
        let mut reconnected = second_state.ws_hub.register_test_client("reconnected-ui");
        let retry = app(Arc::clone(&second_state))
            .oneshot(internal_request(&token, &payload, Some(EVENT_ID), false))
            .await
            .expect("post-restart retry");
        assert_eq!(retry.status(), StatusCode::OK);
        let retry_body = response_json(retry).await;
        assert_eq!(retry_body["data"]["retry"], true);
        assert_eq!(retry_body["data"]["client_count"], 0);
        assert!(reconnected.try_recv().is_err());
    }

    #[tokio::test]
    async fn pending_event_is_recovered_and_delivered_after_api_runtime_restart() {
        let directory = tempfile::tempdir().expect("temporary database directory");
        let path = directory.path().join("pending-api.db");
        let first_pool = SqlitePoolOptions::new()
            .max_connections(3)
            .connect_with(common::bootstrap_database::sqlite_connect_options(
                path.to_str().expect("UTF-8 database path"),
            ))
            .await
            .expect("first API database pool");
        let first_state = app_state_with_database(first_pool, false).await;
        let payload = alarm_payload(EVENT_ID, 95.0);
        let canonical = serde_json_canonicalizer::to_vec(&payload).expect("canonical payload");
        let digest = format!("{:x}", Sha256::digest(canonical));
        assert_eq!(
            crate::db::accept_alarm_event(&first_state.db, EVENT_ID, &digest, 1)
                .await
                .expect("persist pending event"),
            AlarmEventAcceptance::Pending { retry: false }
        );
        let token = first_state.config.alarm_broadcast_token.clone();
        drop(first_state);

        let second_pool = SqlitePoolOptions::new()
            .max_connections(3)
            .connect_with(common::bootstrap_database::sqlite_connect_options(
                path.to_str().expect("UTF-8 database path"),
            ))
            .await
            .expect("restarted API database pool");
        let second_state = app_state_with_database(second_pool, false).await;
        let mut reconnected = second_state.ws_hub.register_test_client("reconnected-ui");
        let retry = app(Arc::clone(&second_state))
            .oneshot(internal_request(&token, &payload, Some(EVENT_ID), false))
            .await
            .expect("pending retry");
        assert_eq!(retry.status(), StatusCode::OK);
        assert_eq!(
            reconnected.recv().await.as_deref(),
            Some(payload.to_string().as_str())
        );
        let stats = crate::db::alarm_event_ledger_stats(&second_state.db)
            .await
            .expect("ledger stats");
        assert_eq!((stats.pending_events, stats.delivered_events), (0, 1));
    }

    #[tokio::test]
    async fn pending_same_process_retry_keeps_live_connection_single_delivery() {
        let state = app_state().await;
        let mut client = state.ws_hub.register_test_client("pending-ui");
        let payload = alarm_payload(EVENT_ID, 95.0);
        let canonical = serde_json_canonicalizer::to_vec(&payload).expect("canonical payload");
        let digest = format!("{:x}", Sha256::digest(canonical));
        crate::db::accept_alarm_event(&state.db, EVENT_ID, &digest, 1)
            .await
            .expect("persist pending event");
        state
            .ws_hub
            .broadcast_event_once(EVENT_ID, &payload.to_string());
        assert!(client.recv().await.is_some());

        let token = state.config.alarm_broadcast_token.clone();
        let retry = app(Arc::clone(&state))
            .oneshot(internal_request(&token, &payload, Some(EVENT_ID), false))
            .await
            .expect("same-process pending retry");
        assert_eq!(retry.status(), StatusCode::OK);
        let body = response_json(retry).await;
        assert_eq!(body["data"]["client_count"], 0);
        assert_eq!(body["data"]["already_seen_client_count"], 1);
        assert!(client.try_recv().is_err());
    }

    #[tokio::test]
    async fn broadcast_status_exposes_durable_alarm_ledger_metrics() {
        let state = app_state().await;
        let payload = alarm_payload(EVENT_ID, 95.0);
        let token = state.config.alarm_broadcast_token.clone();
        app(Arc::clone(&state))
            .oneshot(internal_request(&token, &payload, Some(EVENT_ID), false))
            .await
            .expect("alarm acceptance");
        let mut request = Request::get("/api/broadcast/status")
            .body(Body::empty())
            .expect("status request");
        *request.headers_mut() = authorization_headers("Viewer");
        let response = app(state).oneshot(request).await.expect("status response");
        assert_eq!(response.status(), StatusCode::OK);
        let body = response_json(response).await;
        assert_eq!(body["data"]["alarm_event_ledger"]["durable_events"], 1);
        assert_eq!(body["data"]["alarm_event_ledger"]["pending_events"], 0);
        assert_eq!(body["data"]["alarm_event_ledger"]["delivered_events"], 1);
        assert_eq!(body["data"]["alarm_event_ledger"]["duplicate_requests"], 0);
    }
}
