use std::sync::{Arc, Mutex};

use aether_application::{
    Actor, ApplicationError, ControlApplication, EdgeApplication, OperationKind, RequestContext,
    RiskLevel, SafetyPolicy, capability_catalog,
};
use aether_domain::{
    CommandId, ControlCommand, InstanceId, PointAddress, PointId, PointKind, PointQuality,
    PointSample, TimestampMs,
};
use aether_ports::{
    AuditOutcome, AuditRecord, AuditSink, CommandDispatcher, CommandReceipt, LiveState, PortError,
    PortErrorKind, PortResult,
};
use async_trait::async_trait;

struct StubLiveState {
    sample: PointSample,
}

#[async_trait]
impl LiveState for StubLiveState {
    async fn read(&self, address: PointAddress) -> PortResult<Option<PointSample>> {
        Ok((address == self.sample.address()).then_some(self.sample))
    }

    async fn read_many(&self, addresses: &[PointAddress]) -> PortResult<Vec<Option<PointSample>>> {
        Ok(addresses
            .iter()
            .map(|address| (*address == self.sample.address()).then_some(self.sample))
            .collect())
    }
}

#[derive(Default)]
struct RecordingDispatcher {
    commands: Mutex<Vec<ControlCommand>>,
}

#[async_trait]
impl CommandDispatcher for RecordingDispatcher {
    async fn dispatch(&self, command: ControlCommand) -> PortResult<CommandReceipt> {
        self.commands.lock().unwrap().push(command);
        Ok(CommandReceipt::new(
            command.id(),
            TimestampMs::new(command.issued_at().get() + 1),
        ))
    }
}

#[derive(Default)]
struct RecordingAudit {
    records: Mutex<Vec<AuditRecord>>,
    fail: bool,
}

impl RecordingAudit {
    fn failing() -> Self {
        Self {
            records: Mutex::new(Vec::new()),
            fail: true,
        }
    }
}

#[async_trait]
impl AuditSink for RecordingAudit {
    async fn record(&self, record: AuditRecord) -> PortResult<()> {
        if self.fail {
            return Err(PortError::new(
                PortErrorKind::Unavailable,
                "audit sink offline",
            ));
        }
        self.records.lock().unwrap().push(record);
        Ok(())
    }
}

fn action_address() -> PointAddress {
    PointAddress::new(InstanceId::new(8), PointKind::Action, PointId::new(2))
}

fn sample() -> PointSample {
    PointSample::new(
        action_address(),
        12.0,
        TimestampMs::new(1_000),
        PointQuality::Good,
    )
}

fn application(
    dispatcher: Arc<RecordingDispatcher>,
    audit: Arc<RecordingAudit>,
) -> EdgeApplication {
    EdgeApplication::new(
        Arc::new(StubLiveState { sample: sample() }),
        dispatcher,
        audit,
        SafetyPolicy,
    )
}

fn control_application(
    dispatcher: Arc<RecordingDispatcher>,
    audit: Arc<RecordingAudit>,
) -> ControlApplication {
    ControlApplication::new(dispatcher, audit, SafetyPolicy)
}

fn context(actor: Actor, confirmed: bool) -> RequestContext {
    RequestContext::new("request-1", actor, confirmed, TimestampMs::new(2_000))
}

#[test]
fn capability_catalog_is_machine_discoverable_and_classifies_control_as_high_risk() {
    let write = capability_catalog()
        .iter()
        .find(|descriptor| descriptor.name() == "device.write_point")
        .expect("write capability is registered");

    assert_eq!(write.kind(), OperationKind::Command);
    assert_eq!(write.risk(), RiskLevel::High);
    assert_eq!(write.required_permission(), "device.control");
    assert!(write.requires_confirmation());
    assert!(!write.is_idempotent());
}

#[tokio::test]
async fn reading_live_state_requires_the_declared_permission() {
    let dispatcher = Arc::new(RecordingDispatcher::default());
    let audit = Arc::new(RecordingAudit::default());
    let application = application(dispatcher, audit);

    let denied = application
        .read_point(&context(Actor::new("reader"), false), action_address())
        .await;
    assert!(matches!(
        denied,
        Err(ApplicationError::PermissionDenied { .. })
    ));

    let allowed = application
        .read_point(
            &context(Actor::new("reader").with_permission("device.read"), false),
            action_address(),
        )
        .await
        .expect("authorized read succeeds");
    assert_eq!(allowed, Some(sample()));
}

#[tokio::test]
async fn high_risk_control_requires_confirmation_and_audits_rejection() {
    let dispatcher = Arc::new(RecordingDispatcher::default());
    let audit = Arc::new(RecordingAudit::default());
    let application = application(Arc::clone(&dispatcher), Arc::clone(&audit));

    let result = application
        .write_point(
            &context(
                Actor::new("operator").with_permission("device.control"),
                false,
            ),
            CommandId::new(17),
            action_address(),
            30.0,
        )
        .await;

    assert!(matches!(
        result,
        Err(ApplicationError::ConfirmationRequired { .. })
    ));
    assert!(dispatcher.commands.lock().unwrap().is_empty());
    let records = audit.records.lock().unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].outcome(), AuditOutcome::Rejected);
}

#[tokio::test]
async fn confirmed_control_is_audited_before_and_after_dispatch() {
    let dispatcher = Arc::new(RecordingDispatcher::default());
    let audit = Arc::new(RecordingAudit::default());
    let application = application(Arc::clone(&dispatcher), Arc::clone(&audit));

    let receipt = application
        .write_point(
            &context(
                Actor::new("operator").with_permission("device.control"),
                true,
            ),
            CommandId::new(18),
            action_address(),
            31.0,
        )
        .await
        .expect("confirmed control succeeds");

    assert_eq!(receipt.command_id(), CommandId::new(18));
    let commands = dispatcher.commands.lock().unwrap();
    assert_eq!(commands.len(), 1);
    assert_eq!(commands[0].issued_at(), TimestampMs::new(2_000));
    assert_eq!(commands[0].expires_at(), TimestampMs::new(7_000));
    drop(commands);
    let outcomes: Vec<_> = audit
        .records
        .lock()
        .unwrap()
        .iter()
        .map(AuditRecord::outcome)
        .collect();
    assert_eq!(
        outcomes,
        vec![AuditOutcome::Attempted, AuditOutcome::Succeeded]
    );
}

#[tokio::test]
async fn unavailable_mandatory_audit_fails_closed_before_device_dispatch() {
    let dispatcher = Arc::new(RecordingDispatcher::default());
    let application = application(Arc::clone(&dispatcher), Arc::new(RecordingAudit::failing()));

    let result = application
        .write_point(
            &context(
                Actor::new("operator").with_permission("device.control"),
                true,
            ),
            CommandId::new(19),
            action_address(),
            32.0,
        )
        .await;

    assert!(matches!(result, Err(ApplicationError::AuditUnavailable(_))));
    assert!(dispatcher.commands.lock().unwrap().is_empty());
}

#[tokio::test]
async fn control_facade_authorizes_audits_and_dispatches_without_live_state() {
    let dispatcher = Arc::new(RecordingDispatcher::default());
    let audit = Arc::new(RecordingAudit::default());
    let application = control_application(Arc::clone(&dispatcher), Arc::clone(&audit));

    application
        .write_point(
            &context(
                Actor::new("local:operator").with_permission("device.control"),
                true,
            ),
            CommandId::new(20),
            action_address(),
            33.0,
        )
        .await
        .expect("control-only application dispatches without a live-state port");

    assert_eq!(dispatcher.commands.lock().unwrap().len(), 1);
    assert_eq!(audit.records.lock().unwrap().len(), 2);
}
