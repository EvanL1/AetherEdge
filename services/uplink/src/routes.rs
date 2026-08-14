use std::sync::Arc;

use aether_cloudlink::{AlarmEvent, CloudLinkCodec};
use aether_domain::TimestampMs;
use aether_ports::{
    CloudLinkMessageKind, CloudLinkReceiptRetention, CloudLinkSpool, CloudLinkSpoolErrorReason,
};
use axum::{
    Json, Router,
    body::Body,
    extract::{DefaultBodyLimit, State},
    http::{HeaderMap, StatusCode, header},
    response::Response,
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
#[cfg(feature = "openapi")]
use utoipa::{OpenApi, ToSchema};

use crate::cloudlink_runtime::now_ms;
use crate::state::AppState;

const MAX_ALARM_BODY_BYTES: usize = 64 * 1024;
const INTERNAL_AUTH_SCHEME: &str = "Bearer";

pub fn build_router(state: Arc<AppState>) -> Router {
    let router = Router::new()
        .route("/", get(root))
        .route("/ping", get(ping))
        .route("/health", get(health))
        .route(
            "/api/internal/alarm-events",
            post(alarm_event).layer(DefaultBodyLimit::max(MAX_ALARM_BODY_BYTES)),
        )
        .route(
            "/api/admin/logs/level",
            get(common::admin_api::get_log_level).post(common::admin_api::set_log_level),
        )
        .route(
            "/api/admin/logs/files",
            get(common::admin_api::list_log_files),
        )
        .route(
            "/api/admin/logs/view",
            get(common::admin_api::view_log_file),
        )
        .with_state(state);
    #[cfg(feature = "openapi")]
    let router = router.route("/openapi.json", get(openapi_document));
    router
}

#[cfg(feature = "openapi")]
async fn openapi_document() -> Json<utoipa::openapi::OpenApi> {
    Json(ApiDoc::openapi())
}

#[cfg(feature = "openapi")]
#[derive(OpenApi)]
#[openapi(
    paths(
        root,
        ping,
        health,
        alarm_event,
        common::admin_api::get_log_level,
        common::admin_api::set_log_level,
        common::admin_api::list_log_files,
        common::admin_api::view_log_file,
    ),
    components(schemas(
        AlarmEventRequest,
        AlarmAdmissionResponse,
        common::admin_api::SetLogLevelRequest,
        common::admin_api::LogLevelResponse,
    )),
    tags(
        (name = "Health", description = "Uplink process and CloudLink state"),
        (name = "Internal", description = "Authenticated service-local ingestion"),
        (name = "admin", description = "Host-local service administration"),
    ),
    info(
        title = "Aether Uplink Service API",
        version = env!("CARGO_PKG_VERSION"),
        description = "Internal loopback API for the single CloudLink uplink. No generic MQTT configuration, command, certificate, or compatibility routes are exposed."
    )
)]
pub struct ApiDoc;

#[derive(Debug, Deserialize)]
#[cfg_attr(feature = "openapi", derive(ToSchema))]
#[serde(transparent)]
struct AlarmEventRequest(Value);

#[derive(Debug, Serialize)]
#[cfg_attr(feature = "openapi", derive(ToSchema))]
struct AlarmAdmissionResponse {
    success: bool,
    event_id: String,
    stream_id: String,
    stream_epoch: u64,
    position: u64,
    duplicate: bool,
}

#[utoipa::path(get, path = "/", tag = "Health", responses((status = 200)))]
async fn root() -> Json<Value> {
    Json(json!({
        "service": "aether-uplink",
        "cloud_protocol": "aether.cloudlink",
        "status": "running"
    }))
}

#[utoipa::path(get, path = "/ping", tag = "Health", responses((status = 200)))]
async fn ping() -> &'static str {
    "pong"
}

#[utoipa::path(
    get,
    path = "/health",
    tag = "Health",
    responses(
        (status = 200, description = "Durable admission is ready and CloudLink is disabled or session-established"),
        (status = 503, description = "Durable spool unavailable or configured CloudLink session is not ready")
    )
)]
async fn health(State(state): State<Arc<AppState>>) -> (StatusCode, Json<Value>) {
    let spool = match state.spool.status().await {
        Ok(status) => status,
        Err(error) => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({"success": false, "message": error.to_string()})),
            );
        },
    };
    let protected_receipt_slots = spool
        .acknowledged_receipts()
        .saturating_add(spool.pending_receipt_reservations());
    let quota_ready = spool.current_live_bytes() < spool.ordinary_max_live_bytes()
        && spool.journal_bytes() < spool.max_journal_bytes();
    let admission_ready = spool.ordinary_pending_records() < spool.record_capacity()
        && protected_receipt_slots < spool.acknowledged_receipt_capacity()
        && quota_ready;
    let ready = admission_ready && state.cloudlink.ready();
    let cloudlink = state.cloudlink.snapshot().await;
    (
        if ready {
            StatusCode::OK
        } else {
            StatusCode::SERVICE_UNAVAILABLE
        },
        Json(json!({
            "success": ready,
            "cloudlink": cloudlink,
            "spool": {
                "stream_id": spool.stream_id(),
                "stream_epoch": spool.stream_epoch(),
                "next_position": spool.next_position(),
                "earliest_retained_position": spool.earliest_retained_position(),
                "last_acknowledged_position": spool.last_acknowledged_position(),
                "pending_records": spool.pending_records(),
                "ordinary_pending_records": spool.ordinary_pending_records(),
                "system_pending_records": spool.system_pending_records(),
                "record_capacity": spool.record_capacity(),
                "acknowledged_receipts": spool.acknowledged_receipts(),
                "pending_receipt_reservations": spool.pending_receipt_reservations(),
                "protected_receipt_slots": protected_receipt_slots,
                "acknowledged_receipt_capacity": spool.acknowledged_receipt_capacity(),
                "lossless_admission_ready": admission_ready,
                "current_live_bytes": spool.current_live_bytes(),
                "max_live_bytes": spool.max_live_bytes(),
                "ordinary_max_live_bytes": spool.ordinary_max_live_bytes(),
                "journal_bytes": spool.journal_bytes(),
                "max_journal_bytes": spool.max_journal_bytes(),
                "quota_rejections": spool.quota_rejections(),
                "data_loss_pending": spool.data_loss().is_some(),
            }
        })),
    )
}

#[utoipa::path(
    post,
    path = "/api/internal/alarm-events",
    tag = "Internal",
    request_body = AlarmEventRequest,
    responses(
        (status = 200, description = "Transition durably admitted to the CloudLink spool", body = AlarmAdmissionResponse),
        (status = 400, description = "Invalid transition or identity headers"),
        (status = 401, description = "Missing or invalid service credential"),
        (status = 409, description = "event_id is bound to different content"),
        (status = 503, description = "Lossless CloudLink spool admission unavailable")
    )
)]
async fn alarm_event(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(AlarmEventRequest(body)): Json<AlarmEventRequest>,
) -> Response<Body> {
    if !valid_service_authorization(&headers, &state.alarm_broadcast_token) {
        return error_response(StatusCode::UNAUTHORIZED, "invalid alarm service credential");
    }
    if headers.contains_key("X-Aether-Replay") {
        return error_response(
            StatusCode::BAD_REQUEST,
            "CloudLink alarm ingestion accepts transitions only",
        );
    }
    let header_event_id = match required_single_header(&headers, "X-Aether-Event-ID") {
        Ok(value) => value,
        Err(message) => return error_response(StatusCode::BAD_REQUEST, message),
    };
    let idempotency_key = match required_single_header(&headers, "Idempotency-Key") {
        Ok(value) => value,
        Err(message) => return error_response(StatusCode::BAD_REQUEST, message),
    };
    let event = match AlarmEvent::from_value(body.clone()) {
        Ok(event) => event,
        Err(error) => return error_response(StatusCode::BAD_REQUEST, error.to_string()),
    };
    if event.event_id() != header_event_id || event.event_id() != idempotency_key {
        return error_response(
            StatusCode::BAD_REQUEST,
            "event_id, X-Aether-Event-ID, and Idempotency-Key must match",
        );
    }
    let created_at = match now_ms() {
        Ok(value) => TimestampMs::new(value),
        Err(error) => return error_response(StatusCode::SERVICE_UNAVAILABLE, error.to_string()),
    };
    let input = match CloudLinkCodec::prepare_value(
        CloudLinkMessageKind::AlarmEvent,
        event.event_id(),
        body,
        created_at,
        None,
    ) {
        Ok(input) => input,
        Err(error) => return error_response(StatusCode::BAD_REQUEST, error.to_string()),
    };
    match state
        .spool
        .admit_lossless(input, CloudLinkReceiptRetention::RetainForIdempotency)
        .await
    {
        Ok(admission) => {
            state.delivery_wake.notify_one();
            let identity = admission.identity();
            json_response(
                StatusCode::OK,
                &AlarmAdmissionResponse {
                    success: true,
                    event_id: event.event_id().to_owned(),
                    stream_id: identity.stream_id().to_owned(),
                    stream_epoch: identity.stream_epoch(),
                    position: identity.position(),
                    duplicate: admission.duplicate(),
                },
            )
        },
        Err(error) => match error.reason() {
            Some(CloudLinkSpoolErrorReason::ConflictingIdentity) => {
                error_response(StatusCode::CONFLICT, error.to_string())
            },
            Some(CloudLinkSpoolErrorReason::CapacityExceeded)
            | Some(CloudLinkSpoolErrorReason::Storage) => {
                error_response(StatusCode::SERVICE_UNAVAILABLE, error.to_string())
            },
            _ => error_response(StatusCode::BAD_REQUEST, error.to_string()),
        },
    }
}

fn required_single_header<'a>(
    headers: &'a HeaderMap,
    name: &'static str,
) -> Result<&'a str, &'static str> {
    let mut values = headers.get_all(name).iter();
    let first = values.next().ok_or("required identity header is missing")?;
    if values.next().is_some() {
        return Err("identity headers must appear exactly once");
    }
    first
        .to_str()
        .map_err(|_| "identity headers must contain visible ASCII")
}

fn valid_service_authorization(headers: &HeaderMap, expected: &str) -> bool {
    let Ok(authorization) = required_single_header(headers, header::AUTHORIZATION.as_str()) else {
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

fn error_response(status: StatusCode, message: impl Into<String>) -> Response<Body> {
    json_response(
        status,
        &json!({"success": false, "message": message.into()}),
    )
}

fn json_response(status: StatusCode, value: &impl Serialize) -> Response<Body> {
    match serde_json::to_vec(value) {
        Ok(body) => Response::builder()
            .status(status)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body))
            .unwrap_or_else(|_| Response::new(Body::empty())),
        Err(_) => Response::builder()
            .status(StatusCode::INTERNAL_SERVER_ERROR)
            .body(Body::empty())
            .unwrap_or_else(|_| Response::new(Body::empty())),
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use aether_ports::{CloudLinkDurableAck, CloudLinkSessionBinding, DurableAckOutcome};
    use axum::body::to_bytes;
    use axum::http::Request;
    use tower::ServiceExt;

    use super::*;
    use crate::cloudlink_runtime::CloudLinkRuntimeStatus;

    const TOKEN: &str = "alarm-service-token-0123456789abcdef";

    fn payload(value: f64) -> Value {
        json!({
            "type": "alarm",
            "id": "alarm_042",
            "event_id": "alarm-trigger-42",
            "timestamp": 1721000000,
            "data": {
                "alarm_id": "42",
                "event_id": "alarm-trigger-42",
                "service_type": "io",
                "source": "io",
                "device": "10",
                "channel_id": 10,
                "data_type": "T",
                "point_id": 8,
                "status": 1,
                "level": 2,
                "value": value,
                "message": "High temperature"
            }
        })
    }

    fn state(path: &Path, capacity: usize) -> Arc<AppState> {
        state_with_receipt_capacity(path, capacity, 100_000)
    }

    fn state_with_receipt_capacity(
        path: &Path,
        capacity: usize,
        receipt_capacity: usize,
    ) -> Arc<AppState> {
        Arc::new(AppState {
            spool: Arc::new(
                aether_store_local::FileCloudLinkSpool::open_with_receipt_capacity(
                    path,
                    "business",
                    capacity,
                    receipt_capacity,
                )
                .expect("spool"),
            ),
            alarm_broadcast_token: Arc::from(TOKEN),
            cloudlink: Arc::new(CloudLinkRuntimeStatus::new(false)),
            delivery_wake: Arc::new(tokio::sync::Notify::new()),
        })
    }

    fn request(body: Value) -> Request<Body> {
        request_for_event(body, "alarm-trigger-42")
    }

    fn request_for_event(body: Value, event_id: &str) -> Request<Body> {
        Request::builder()
            .method("POST")
            .uri("/api/internal/alarm-events")
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
            .header("X-Aether-Event-ID", event_id)
            .header("Idempotency-Key", event_id)
            .body(Body::from(serde_json::to_vec(&body).expect("body")))
            .expect("request")
    }

    async fn response_json(response: Response<Body>) -> Value {
        serde_json::from_slice(
            &to_bytes(response.into_body(), MAX_ALARM_BODY_BYTES)
                .await
                .expect("body"),
        )
        .expect("JSON")
    }

    #[tokio::test]
    async fn exact_retry_is_idempotent_across_process_reopen_and_conflict_is_closed() {
        let root = tempfile::tempdir().expect("temp dir");
        let path = root.path().join("cloudlink.spool");
        let first = build_router(state(&path, 8));
        let accepted = first
            .clone()
            .oneshot(request(payload(12.5)))
            .await
            .expect("accepted");
        assert_eq!(accepted.status(), StatusCode::OK);
        let accepted = response_json(accepted).await;
        assert_eq!(accepted["duplicate"], false);
        drop(first);

        let reopened = build_router(state(&path, 8));
        let duplicate = reopened
            .clone()
            .oneshot(request(payload(12.5)))
            .await
            .expect("duplicate");
        assert_eq!(duplicate.status(), StatusCode::OK);
        assert_eq!(response_json(duplicate).await["duplicate"], true);

        let conflict = reopened
            .oneshot(request(payload(99.0)))
            .await
            .expect("conflict");
        assert_eq!(conflict.status(), StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn successful_alarm_admission_wakes_cloudlink_delivery() {
        let root = tempfile::tempdir().expect("temp dir");
        let app_state = state(&root.path().join("cloudlink.spool"), 8);
        let delivery_wake = Arc::clone(&app_state.delivery_wake);
        let response = build_router(app_state)
            .oneshot(request(payload(12.5)))
            .await
            .expect("alarm admission");
        assert_eq!(response.status(), StatusCode::OK);
        tokio::time::timeout(std::time::Duration::from_secs(1), delivery_wake.notified())
            .await
            .expect("delivery wake");
    }

    #[tokio::test]
    async fn retry_after_cloud_ack_keeps_original_identity_across_reopen() {
        let root = tempfile::tempdir().expect("temp dir");
        let path = root.path().join("cloudlink.spool");
        let first_state = state(&path, 8);
        let first = build_router(Arc::clone(&first_state));
        let accepted = first
            .clone()
            .oneshot(request(payload(12.5)))
            .await
            .expect("accepted");
        assert_eq!(accepted.status(), StatusCode::OK);
        let accepted = response_json(accepted).await;
        let original_position = accepted["position"].as_u64().expect("position");

        let record = first_state
            .spool
            .replay_from(original_position, 1)
            .await
            .expect("retained alarm")
            .records()[0]
            .clone();
        let cloud_session = CloudLinkSessionBinding::new("cloud-session-1", 1);
        first_state
            .spool
            .mark_offered(record.identity(), &cloud_session)
            .await
            .expect("offer alarm");
        first_state
            .spool
            .mark_transport_published(record.identity(), &cloud_session)
            .await
            .expect("publish alarm");
        let ack = CloudLinkDurableAck::new(
            cloud_session,
            record.identity().stream_id(),
            record.identity().stream_epoch(),
            record.identity().position(),
            record.batch_id(),
            record.digest(),
            "alarm-receipt-42",
        );
        assert_eq!(
            first_state
                .spool
                .acknowledge(&ack)
                .await
                .expect("ACK alarm"),
            DurableAckOutcome::Applied { removed: 1 }
        );
        assert_eq!(
            first_state
                .spool
                .status()
                .await
                .expect("status")
                .pending_records(),
            0
        );
        drop(first);
        drop(first_state);

        let reopened = build_router(state(&path, 8));
        let duplicate = reopened
            .clone()
            .oneshot(request(payload(12.5)))
            .await
            .expect("duplicate after ACK");
        assert_eq!(duplicate.status(), StatusCode::OK);
        let duplicate = response_json(duplicate).await;
        assert_eq!(duplicate["duplicate"], true);
        assert_eq!(duplicate["position"], original_position);

        let conflict = reopened
            .oneshot(request(payload(99.0)))
            .await
            .expect("conflict after ACK");
        assert_eq!(conflict.status(), StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn full_alarm_receipt_ledger_is_visible_and_never_evicts_identity() {
        let root = tempfile::tempdir().expect("temp dir");
        let path = root.path().join("cloudlink.spool");
        let app_state = state_with_receipt_capacity(&path, 8, 1);
        let router = build_router(Arc::clone(&app_state));
        assert_eq!(
            router
                .clone()
                .oneshot(request(payload(12.5)))
                .await
                .expect("alarm admission")
                .status(),
            StatusCode::OK
        );

        let record = app_state
            .spool
            .replay_from(1, 1)
            .await
            .expect("retained alarm")
            .records()[0]
            .clone();
        let cloud_session = CloudLinkSessionBinding::new("cloud-session-1", 1);
        app_state
            .spool
            .mark_offered(record.identity(), &cloud_session)
            .await
            .expect("offer alarm");
        app_state
            .spool
            .acknowledge(&CloudLinkDurableAck::new(
                cloud_session,
                record.identity().stream_id(),
                record.identity().stream_epoch(),
                record.identity().position(),
                record.batch_id(),
                record.digest(),
                "alarm-receipt-42",
            ))
            .await
            .expect("ack alarm");

        let health = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/health")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("health");
        assert_eq!(health.status(), StatusCode::SERVICE_UNAVAILABLE);
        let health = response_json(health).await;
        assert_eq!(health["spool"]["acknowledged_receipts"], 1);
        assert_eq!(health["spool"]["pending_receipt_reservations"], 0);
        assert_eq!(health["spool"]["protected_receipt_slots"], 1);
        assert_eq!(health["spool"]["lossless_admission_ready"], false);
        assert_eq!(health["spool"]["ordinary_pending_records"], 0);
        assert_eq!(health["spool"]["system_pending_records"], 0);
        assert!(health["spool"]["current_live_bytes"].is_number());
        assert!(health["spool"]["ordinary_max_live_bytes"].is_number());
        assert!(health["spool"]["max_live_bytes"].is_number());
        assert!(health["spool"]["journal_bytes"].is_number());
        assert!(health["spool"]["max_journal_bytes"].is_number());
        assert_eq!(health["spool"]["quota_rejections"], 0);

        let duplicate = router
            .clone()
            .oneshot(request(payload(12.5)))
            .await
            .expect("exact retry");
        assert_eq!(duplicate.status(), StatusCode::OK);
        assert_eq!(response_json(duplicate).await["duplicate"], true);

        let mut second = payload(13.0);
        second["event_id"] = json!("alarm-trigger-43");
        second["data"]["event_id"] = json!("alarm-trigger-43");
        let rejected = router
            .oneshot(request_for_event(second, "alarm-trigger-43"))
            .await
            .expect("full receipt ledger");
        assert_eq!(rejected.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn lossless_alarm_admission_rejects_capacity_without_evicting() {
        let root = tempfile::tempdir().expect("temp dir");
        let path = root.path().join("cloudlink.spool");
        let state = state(&path, 1);
        let router = build_router(Arc::clone(&state));
        assert_eq!(
            router
                .clone()
                .oneshot(request(payload(12.5)))
                .await
                .expect("first")
                .status(),
            StatusCode::OK
        );
        let mut second = payload(13.0);
        second["event_id"] = json!("alarm-trigger-43");
        second["data"]["event_id"] = json!("alarm-trigger-43");
        let second_request = Request::builder()
            .method("POST")
            .uri("/api/internal/alarm-events")
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
            .header("X-Aether-Event-ID", "alarm-trigger-43")
            .header("Idempotency-Key", "alarm-trigger-43")
            .body(Body::from(serde_json::to_vec(&second).expect("body")))
            .expect("request");
        assert_eq!(
            router.oneshot(second_request).await.expect("full").status(),
            StatusCode::SERVICE_UNAVAILABLE
        );
        assert_eq!(
            state
                .spool
                .status()
                .await
                .expect("status")
                .pending_records(),
            1
        );
    }

    #[tokio::test]
    async fn generic_mqtt_and_old_alarm_routes_do_not_exist() {
        let root = tempfile::tempdir().expect("temp dir");
        let router = build_router(state(&root.path().join("cloudlink.spool"), 8));
        for path in [
            "/netApi/mqtt/status",
            "/netApi/mqtt/config",
            "/netApi/alarm/broadcast",
            "/netApi/inst-sync",
        ] {
            let response = router
                .clone()
                .oneshot(
                    Request::builder()
                        .uri(path)
                        .body(Body::empty())
                        .expect("request"),
                )
                .await
                .expect("response");
            assert_eq!(response.status(), StatusCode::NOT_FOUND, "{path}");
        }
    }

    #[tokio::test]
    async fn configured_but_disconnected_cloudlink_is_not_reported_ready() {
        let root = tempfile::tempdir().expect("temp dir");
        let disconnected = Arc::new(AppState {
            spool: Arc::new(
                aether_store_local::FileCloudLinkSpool::open_with_receipt_capacity(
                    root.path().join("cloudlink.spool"),
                    "business",
                    8,
                    100_000,
                )
                .expect("spool"),
            ),
            alarm_broadcast_token: Arc::from(TOKEN),
            cloudlink: Arc::new(CloudLinkRuntimeStatus::new(true)),
            delivery_wake: Arc::new(tokio::sync::Notify::new()),
        });
        let router = build_router(disconnected);
        let response = router
            .oneshot(
                Request::builder()
                    .uri("/health")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        let body = response_json(response).await;
        assert_eq!(body["success"], false);
        assert_eq!(body["cloudlink"]["configured"], true);
        assert_eq!(body["cloudlink"]["task_running"], false);
        assert_eq!(body["cloudlink"]["session_established"], false);
    }
}
