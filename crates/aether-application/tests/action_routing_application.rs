use std::sync::{Arc, Mutex};

use aether_application::{
    ActionRoutingApplication, Actor, ApplicationError, AuditPolicy, ConfirmationPolicy,
    MANAGE_ROUTING_CAPABILITY, OperationKind, RequestContext, RiskLevel, SafetyPolicy,
    capability_catalog,
};
use aether_domain::{
    ChannelCommandAddress, ChannelId, InstanceId, PointId, PointKind, TimestampMs,
};
use aether_ports::{
    ActionRoute, ActionRouteKey, ActionRoutingMutation, ActionRoutingMutationReceipt, AuditOutcome,
    AuditRecord, AuditSink, AutomationActionRoutingMutator, PortError, PortErrorKind, PortResult,
};
use async_trait::async_trait;

struct RecordingMutator {
    mutations: Mutex<Vec<ActionRoutingMutation>>,
    events: Arc<Mutex<Vec<&'static str>>>,
    failure: Option<PortError>,
}

impl RecordingMutator {
    fn successful(events: Arc<Mutex<Vec<&'static str>>>) -> Self {
        Self {
            mutations: Mutex::new(Vec::new()),
            events,
            failure: None,
        }
    }

    fn failing(events: Arc<Mutex<Vec<&'static str>>>) -> Self {
        Self {
            mutations: Mutex::new(Vec::new()),
            events,
            failure: Some(PortError::new(
                PortErrorKind::Conflict,
                "route changed concurrently",
            )),
        }
    }
}

#[async_trait]
impl AutomationActionRoutingMutator for RecordingMutator {
    async fn mutate(
        &self,
        mutation: ActionRoutingMutation,
    ) -> PortResult<ActionRoutingMutationReceipt> {
        let kind = mutation.kind();
        let target = mutation.target();
        self.events.lock().expect("event lock").push("mutate");
        self.mutations.lock().expect("mutation lock").push(mutation);
        if let Some(failure) = &self.failure {
            return Err(failure.clone());
        }
        Ok(ActionRoutingMutationReceipt::new(kind, target, 1))
    }
}

struct RecordingAudit {
    records: Mutex<Vec<AuditRecord>>,
    calls: Mutex<usize>,
    fail_on_call: Option<usize>,
    events: Arc<Mutex<Vec<&'static str>>>,
}

impl RecordingAudit {
    fn successful(events: Arc<Mutex<Vec<&'static str>>>) -> Self {
        Self {
            records: Mutex::new(Vec::new()),
            calls: Mutex::new(0),
            fail_on_call: None,
            events,
        }
    }

    fn failing_on(events: Arc<Mutex<Vec<&'static str>>>, call: usize) -> Self {
        Self {
            records: Mutex::new(Vec::new()),
            calls: Mutex::new(0),
            fail_on_call: Some(call),
            events,
        }
    }
}

#[async_trait]
impl AuditSink for RecordingAudit {
    async fn record(&self, record: AuditRecord) -> PortResult<()> {
        let call = {
            let mut calls = self.calls.lock().expect("call lock");
            *calls += 1;
            *calls
        };
        if self.fail_on_call == Some(call) {
            return Err(PortError::new(
                PortErrorKind::Unavailable,
                "audit sink unavailable",
            ));
        }
        let event = match record.outcome() {
            AuditOutcome::Rejected => "audit.rejected",
            AuditOutcome::Attempted => "audit.attempted",
            AuditOutcome::Succeeded => "audit.succeeded",
            AuditOutcome::Failed => "audit.failed",
        };
        self.events.lock().expect("event lock").push(event);
        self.records.lock().expect("record lock").push(record);
        Ok(())
    }
}

fn route_key() -> ActionRouteKey {
    ActionRouteKey::new(InstanceId::new(7), PointId::new(11))
}

fn route() -> ActionRoute {
    let destination =
        ChannelCommandAddress::new(ChannelId::new(3), PointKind::Action, PointId::new(19))
            .expect("command-owned destination");
    ActionRoute::new(route_key(), destination, true)
}

fn mutation() -> ActionRoutingMutation {
    ActionRoutingMutation::upsert(route())
}

fn context(permission: bool, confirmed: bool) -> RequestContext {
    let actor = if permission {
        Actor::new("operator").with_permission("automation.routing.manage")
    } else {
        Actor::new("operator")
    };
    RequestContext::new(
        "action-routing-1",
        actor,
        confirmed,
        TimestampMs::new(2_000),
    )
}

#[test]
fn action_routing_management_is_a_high_risk_audited_non_idempotent_command() {
    assert_eq!(
        MANAGE_ROUTING_CAPABILITY.name(),
        "automation.routing.manage"
    );
    assert_eq!(MANAGE_ROUTING_CAPABILITY.kind(), OperationKind::Command);
    assert_eq!(MANAGE_ROUTING_CAPABILITY.risk(), RiskLevel::High);
    assert_eq!(
        MANAGE_ROUTING_CAPABILITY.required_permission(),
        "automation.routing.manage"
    );
    assert_eq!(
        MANAGE_ROUTING_CAPABILITY.confirmation(),
        ConfirmationPolicy::Always
    );
    assert_eq!(
        MANAGE_ROUTING_CAPABILITY.audit_policy(),
        AuditPolicy::Required
    );
    assert!(!MANAGE_ROUTING_CAPABILITY.is_idempotent());
    assert!(capability_catalog().contains(&MANAGE_ROUTING_CAPABILITY));
}

#[tokio::test]
async fn authorized_mutation_is_attempt_audited_before_the_port_and_terminal_audited() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let mutator = Arc::new(RecordingMutator::successful(Arc::clone(&events)));
    let audit = Arc::new(RecordingAudit::successful(Arc::clone(&events)));
    let application = ActionRoutingApplication::new(mutator.clone(), audit.clone(), SafetyPolicy);

    let acceptance = application
        .mutate(&context(true, true), mutation())
        .await
        .expect("governed action-route mutation succeeds");

    assert_eq!(acceptance.kind(), mutation().kind());
    assert_eq!(acceptance.route_key(), Some(route_key()));
    assert_eq!(acceptance.affected_routes(), 1);
    assert_eq!(acceptance.request_id(), "action-routing-1");
    assert!(acceptance.completion_audit().is_recorded());
    assert!(!acceptance.is_retryable());
    assert_eq!(
        *events.lock().expect("event lock"),
        vec!["audit.attempted", "mutate", "audit.succeeded"]
    );
    assert_eq!(mutator.mutations.lock().expect("mutation lock").len(), 1);
    let records = audit.records.lock().expect("record lock");
    assert!(
        records
            .iter()
            .all(|record| record.capability() == "automation.routing.manage")
    );
    assert!(
        records[0]
            .detail()
            .expect("audit detail")
            .contains("instance_id=7; action_id=11")
    );
}

#[tokio::test]
async fn denied_or_unconfirmed_mutation_is_rejected_and_never_reaches_the_port() {
    for request_context in [context(false, true), context(true, false)] {
        let events = Arc::new(Mutex::new(Vec::new()));
        let mutator = Arc::new(RecordingMutator::successful(Arc::clone(&events)));
        let audit = Arc::new(RecordingAudit::successful(Arc::clone(&events)));
        let application =
            ActionRoutingApplication::new(mutator.clone(), audit.clone(), SafetyPolicy);

        let result = application.mutate(&request_context, mutation()).await;

        assert!(matches!(
            result,
            Err(ApplicationError::PermissionDenied { .. })
                | Err(ApplicationError::ConfirmationRequired { .. })
        ));
        assert!(mutator.mutations.lock().expect("mutation lock").is_empty());
        assert_eq!(*events.lock().expect("event lock"), vec!["audit.rejected"]);
        assert_eq!(
            audit.records.lock().expect("record lock")[0].outcome(),
            AuditOutcome::Rejected
        );
    }
}

#[tokio::test]
async fn attempted_audit_must_succeed_before_the_mutation_port_runs() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let mutator = Arc::new(RecordingMutator::successful(Arc::clone(&events)));
    let audit = Arc::new(RecordingAudit::failing_on(Arc::clone(&events), 1));
    let application = ActionRoutingApplication::new(mutator.clone(), audit, SafetyPolicy);

    let result = application.mutate(&context(true, true), mutation()).await;

    assert!(matches!(result, Err(ApplicationError::AuditUnavailable(_))));
    assert!(mutator.mutations.lock().expect("mutation lock").is_empty());
    assert!(events.lock().expect("event lock").is_empty());
}

#[tokio::test]
async fn port_failure_is_returned_as_typed_application_error_and_failed_audited() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let mutator = Arc::new(RecordingMutator::failing(Arc::clone(&events)));
    let audit = Arc::new(RecordingAudit::successful(Arc::clone(&events)));
    let application = ActionRoutingApplication::new(mutator, audit, SafetyPolicy);

    let result = application.mutate(&context(true, true), mutation()).await;

    match result {
        Err(ApplicationError::Port(error)) => {
            assert_eq!(error.kind(), PortErrorKind::Conflict);
        },
        other => panic!("expected typed port error, got {other:?}"),
    }
    assert_eq!(
        *events.lock().expect("event lock"),
        vec!["audit.attempted", "mutate", "audit.failed"]
    );
}

#[tokio::test]
async fn terminal_audit_failure_returns_non_retryable_accepted_outcome() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let mutator = Arc::new(RecordingMutator::successful(Arc::clone(&events)));
    let audit = Arc::new(RecordingAudit::failing_on(Arc::clone(&events), 2));
    let application = ActionRoutingApplication::new(mutator.clone(), audit, SafetyPolicy);

    let acceptance = application
        .mutate(&context(true, true), mutation())
        .await
        .expect("an accepted non-idempotent mutation must not be reported retryable");

    assert!(!acceptance.completion_audit().is_recorded());
    assert_eq!(
        acceptance
            .completion_audit()
            .failure()
            .expect("terminal audit failure")
            .kind(),
        PortErrorKind::Unavailable
    );
    assert!(!acceptance.is_retryable());
    assert_eq!(mutator.mutations.lock().expect("mutation lock").len(), 1);
    assert_eq!(
        *events.lock().expect("event lock"),
        vec!["audit.attempted", "mutate"]
    );
}
