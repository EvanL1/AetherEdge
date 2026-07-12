use std::sync::{Arc, Mutex};

use aether_application::{
    Actor, AlertResolutionApplication, ApplicationError, RequestContext, SafetyPolicy,
};
use aether_domain::{AlarmRuleId, AlertId, TimestampMs};
use aether_ports::{
    AlertResolutionReceipt, AlertResolver, AuditOutcome, AuditRecord, AuditSink, PortError,
    PortErrorKind, PortResult,
};
use async_trait::async_trait;

#[derive(Default)]
struct RecordingResolver {
    alerts: Mutex<Vec<AlertId>>,
}

#[async_trait]
impl AlertResolver for RecordingResolver {
    async fn resolve(&self, alert_id: AlertId) -> PortResult<AlertResolutionReceipt> {
        self.alerts.lock().expect("resolver lock").push(alert_id);
        Ok(AlertResolutionReceipt::new(
            alert_id,
            AlarmRuleId::new(9),
            TimestampMs::new(5_000),
        ))
    }
}

struct RecordingAudit {
    records: Mutex<Vec<AuditRecord>>,
    fail_on: Option<AuditOutcome>,
}

#[async_trait]
impl AuditSink for RecordingAudit {
    async fn record(&self, record: AuditRecord) -> PortResult<()> {
        if self.fail_on == Some(record.outcome()) {
            return Err(PortError::new(
                PortErrorKind::Unavailable,
                "alert audit unavailable",
            ));
        }
        self.records.lock().expect("audit lock").push(record);
        Ok(())
    }
}

fn context(permission: bool, confirmed: bool) -> RequestContext {
    let actor = if permission {
        Actor::new("operator").with_permission("alarm.alert.resolve")
    } else {
        Actor::new("operator")
    };
    RequestContext::new("resolve-1", actor, confirmed, TimestampMs::new(4_000))
}

#[tokio::test]
async fn denied_unconfirmed_or_unaudited_resolution_never_reaches_alarm_storage() {
    for (request_context, fail_on) in [
        (context(false, true), None),
        (context(true, false), None),
        (context(true, true), Some(AuditOutcome::Attempted)),
    ] {
        let resolver = Arc::new(RecordingResolver::default());
        let application = AlertResolutionApplication::new(
            resolver.clone(),
            Arc::new(RecordingAudit {
                records: Mutex::new(Vec::new()),
                fail_on,
            }),
            SafetyPolicy,
        );

        let result = application.resolve(&request_context, AlertId::new(3)).await;
        assert!(matches!(
            result,
            Err(ApplicationError::PermissionDenied { .. })
                | Err(ApplicationError::ConfirmationRequired { .. })
                | Err(ApplicationError::AuditUnavailable(_))
        ));
        assert!(resolver.alerts.lock().expect("resolver lock").is_empty());
    }
}

#[tokio::test]
async fn accepted_resolution_is_audited_and_never_retryable() {
    let resolver = Arc::new(RecordingResolver::default());
    let audit = Arc::new(RecordingAudit {
        records: Mutex::new(Vec::new()),
        fail_on: None,
    });
    let application =
        AlertResolutionApplication::new(resolver.clone(), audit.clone(), SafetyPolicy);

    let acceptance = application
        .resolve(&context(true, true), AlertId::new(3))
        .await
        .expect("governed alert resolution");

    assert_eq!(acceptance.alert_id(), AlertId::new(3));
    assert_eq!(acceptance.rule_id(), AlarmRuleId::new(9));
    assert!(!acceptance.is_retryable());
    assert_eq!(resolver.alerts.lock().expect("resolver lock").len(), 1);
    let outcomes = audit
        .records
        .lock()
        .expect("audit lock")
        .iter()
        .map(AuditRecord::outcome)
        .collect::<Vec<_>>();
    assert_eq!(outcomes, [AuditOutcome::Attempted, AuditOutcome::Succeeded]);
}

#[tokio::test]
async fn terminal_resolution_audit_failure_is_accepted_once_with_request_id() {
    let resolver = Arc::new(RecordingResolver::default());
    let application = AlertResolutionApplication::new(
        resolver.clone(),
        Arc::new(RecordingAudit {
            records: Mutex::new(Vec::new()),
            fail_on: Some(AuditOutcome::Succeeded),
        }),
        SafetyPolicy,
    );

    let acceptance = application
        .resolve(&context(true, true), AlertId::new(3))
        .await
        .expect("resolution already accepted");
    assert_eq!(resolver.alerts.lock().expect("resolver lock").len(), 1);
    assert!(acceptance.completion_audit().failure().is_some());
    assert_eq!(acceptance.request_id(), "resolve-1");
    assert!(!acceptance.is_retryable());
}
