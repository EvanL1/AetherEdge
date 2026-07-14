//! Manual rule execution must cross the shared application boundary.

#![allow(clippy::disallowed_methods)]

use std::sync::{Arc, Mutex};

use aether_application::{RuleExecutionApplication, SafetyPolicy};
use aether_automation::infra::application_control::ControlAuthenticator;
use aether_automation::rule_routes::{RuleEngineState, create_rule_routes};
use aether_domain::{RuleId, TimestampMs};
use aether_ports::{
    AuditRecord, AuditSink, AutomationRuleExecutor, PortError, PortErrorKind, PortResult,
    RuleExecutionReceipt,
};
use aether_rules::{MemoryRuleLiveState, RuleScheduler};
use aether_store_local::MemoryAuditSink;
use async_trait::async_trait;
use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use http_body_util::BodyExt;
use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};
use serde::Serialize;
use serde_json::json;
use tower::ServiceExt;

const JWT_SECRET: &str = "0123456789abcdef0123456789abcdef";

#[derive(Default)]
struct RecordingRuleExecutor {
    invocations: Mutex<Vec<RuleId>>,
}

#[async_trait]
impl AutomationRuleExecutor for RecordingRuleExecutor {
    async fn execute(&self, rule_id: RuleId) -> PortResult<RuleExecutionReceipt> {
        self.invocations.lock().unwrap().push(rule_id);
        Ok(RuleExecutionReceipt::new(
            rule_id,
            TimestampMs::new(2_001),
            0,
            0,
        ))
    }
}

#[derive(Serialize)]
struct AccessClaims<'a> {
    user_id: i64,
    username: &'a str,
    role: &'a str,
    exp: usize,
    iat: usize,
    #[serde(rename = "type")]
    token_type: &'a str,
}

fn access_token(role: &str) -> String {
    let now = chrono::Utc::now().timestamp();
    encode(
        &Header::new(Algorithm::HS256),
        &AccessClaims {
            user_id: 7,
            username: "operator",
            role,
            exp: (now + 3_600) as usize,
            iat: now as usize,
            token_type: "access",
        },
        &EncodingKey::from_secret(JWT_SECRET.as_bytes()),
    )
    .unwrap()
}

async fn application_router() -> (
    axum::Router,
    Arc<RecordingRuleExecutor>,
    Arc<MemoryAuditSink>,
) {
    let audit = Arc::new(MemoryAuditSink::new());
    let (router, executor) = application_router_with_audit(audit.clone()).await;
    (router, executor, audit)
}

async fn application_router_with_audit(
    audit: Arc<dyn AuditSink>,
) -> (axum::Router, Arc<RecordingRuleExecutor>) {
    let pool = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();
    sqlx::query(
        "CREATE TABLE rules (\
             id INTEGER PRIMARY KEY,\
             name TEXT NOT NULL,\
             description TEXT,\
             enabled INTEGER DEFAULT 1,\
             priority INTEGER DEFAULT 0,\
             cooldown_ms INTEGER DEFAULT 0,\
             trigger_config TEXT,\
             nodes_json TEXT NOT NULL DEFAULT '{}',\
             flow_json TEXT,\
             format TEXT DEFAULT 'vue-flow',\
             created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,\
             updated_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP\
         )",
    )
    .execute(&pool)
    .await
    .unwrap();
    let scheduler = Arc::new(RuleScheduler::new(
        Arc::new(MemoryRuleLiveState::new()),
        pool.clone(),
        1_000,
        std::env::temp_dir().join("aether-rule-boundary-tests"),
    ));
    let executor = Arc::new(RecordingRuleExecutor::default());
    let application = Arc::new(RuleExecutionApplication::new(
        executor.clone(),
        audit,
        SafetyPolicy,
    ));
    let authenticator =
        Arc::new(ControlAuthenticator::new(JWT_SECRET, None).expect("valid test JWT secret"));
    let state = Arc::new(
        RuleEngineState::new(pool, scheduler).with_execution_boundary(application, authenticator),
    );
    (create_rule_routes(state), executor)
}

#[derive(Default)]
struct FailCompletionAudit {
    calls: Mutex<usize>,
    records: Mutex<Vec<AuditRecord>>,
}

#[async_trait]
impl AuditSink for FailCompletionAudit {
    async fn record(&self, record: AuditRecord) -> PortResult<()> {
        let call = {
            let mut calls = self.calls.lock().unwrap();
            *calls += 1;
            *calls
        };
        if call == 2 {
            return Err(PortError::new(
                PortErrorKind::Unavailable,
                "completion audit unavailable",
            ));
        }
        self.records.lock().unwrap().push(record);
        Ok(())
    }
}

async fn execute_request(
    router: &axum::Router,
    token: Option<&str>,
    confirmed: bool,
) -> (StatusCode, serde_json::Value) {
    let mut request = Request::builder()
        .method("POST")
        .uri("/api/rules/7/execute")
        .header("content-type", "application/json")
        .header("x-request-id", "018f0000-0000-7000-8000-000000000007");
    if let Some(token) = token {
        request = request.header("authorization", format!("Bearer {token}"));
    }
    let response = router
        .clone()
        .oneshot(
            request
                .body(Body::from(
                    serde_json::to_vec(&json!({ "confirmed": confirmed })).unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let body = serde_json::from_slice(&bytes).unwrap_or_else(|_| json!({}));
    (status, body)
}

#[tokio::test]
async fn unauthenticated_or_unconfirmed_rule_execution_never_reaches_runtime() {
    let (router, executor, audit) = application_router().await;

    let (missing_auth, _) = execute_request(&router, None, true).await;
    let (missing_confirmation, _) =
        execute_request(&router, Some(&access_token("Engineer")), false).await;

    assert_eq!(missing_auth, StatusCode::FORBIDDEN);
    assert_eq!(missing_confirmation, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(executor.invocations.lock().unwrap().is_empty());
    assert_eq!(audit.records().unwrap().len(), 2);
}

#[tokio::test]
async fn authenticated_confirmed_rule_execution_uses_application_api() {
    let (router, executor, audit) = application_router().await;

    let (status, body) = execute_request(&router, Some(&access_token("Engineer")), true).await;

    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"]["rule_id"], 7);
    assert_eq!(body["data"]["actions_attempted"], 0);
    assert_eq!(
        executor.invocations.lock().unwrap().as_slice(),
        &[RuleId::new(7)]
    );
    assert_eq!(audit.records().unwrap().len(), 2);
}

#[tokio::test]
async fn completion_audit_failure_is_an_explicit_non_retryable_http_acceptance() {
    let audit = Arc::new(FailCompletionAudit::default());
    let (router, executor) = application_router_with_audit(audit.clone()).await;

    let (status, body) = execute_request(&router, Some(&access_token("Engineer")), true).await;

    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["data"]["request_id"],
        "018f0000-0000-7000-8000-000000000007"
    );
    assert_eq!(body["data"]["audit"]["status"], "incomplete");
    assert_eq!(body["data"]["audit"]["retryable"], false);
    assert_eq!(
        executor.invocations.lock().unwrap().as_slice(),
        &[RuleId::new(7)]
    );
    assert_eq!(audit.records.lock().unwrap().len(), 1);
}
