use std::sync::Arc;

use aether_domain::{AlarmRuleId, AlarmSeverity, AlertId, TimestampMs};
use aether_ports::{
    AcquisitionStateWriter, AlarmRuleMutation, AlarmRuleMutationKind, AlarmRuleMutator,
    AlarmRulePatch, AlertResolutionReceipt, AlertResolver, AuditSink, CommandDispatcher,
    DeviceCommandSink, DurableOutbox, HistorySink, LiveState, LiveStateWriter, PortError,
    PortErrorKind, StateMirror,
};

#[test]
fn error_kind_exposes_recovery_semantics() {
    let unavailable = PortError::new(PortErrorKind::Unavailable, "device offline");
    let timeout = PortError::new(PortErrorKind::Timeout, "request timed out");
    let not_found = PortError::new(PortErrorKind::NotFound, "point is not commissioned");
    let rejected = PortError::new(PortErrorKind::Rejected, "interlock open");
    let permanent = PortError::new(PortErrorKind::Permanent, "invalid credentials");

    assert!(unavailable.is_retryable());
    assert!(timeout.is_retryable());
    assert!(!not_found.is_retryable());
    assert_eq!(not_found.kind(), PortErrorKind::NotFound);
    assert!(!rejected.is_retryable());
    assert!(!permanent.is_retryable());
    assert_eq!(rejected.kind(), PortErrorKind::Rejected);
    assert_eq!(rejected.message(), "interlock open");
}

#[test]
fn extension_ports_are_object_safe() {
    fn accepts_live_state(_: Option<Arc<dyn LiveState>>) {}
    fn accepts_live_state_writer(_: Option<Arc<dyn LiveStateWriter>>) {}
    fn accepts_acquisition_writer(_: Option<Arc<dyn AcquisitionStateWriter>>) {}
    fn accepts_dispatcher(_: Option<Arc<dyn CommandDispatcher>>) {}
    fn accepts_device_command_sink(_: Option<Arc<dyn DeviceCommandSink>>) {}
    fn accepts_history(_: Option<Arc<dyn HistorySink>>) {}
    fn accepts_outbox(_: Option<Arc<dyn DurableOutbox>>) {}
    fn accepts_mirror(_: Option<Arc<dyn StateMirror>>) {}
    fn accepts_audit(_: Option<Arc<dyn AuditSink>>) {}
    fn accepts_alarm_rule_mutator(_: Option<Arc<dyn AlarmRuleMutator>>) {}
    fn accepts_alert_resolver(_: Option<Arc<dyn AlertResolver>>) {}

    accepts_live_state(None);
    accepts_live_state_writer(None);
    accepts_acquisition_writer(None);
    accepts_dispatcher(None);
    accepts_device_command_sink(None);
    accepts_history(None);
    accepts_outbox(None);
    accepts_mirror(None);
    accepts_audit(None);
    accepts_alarm_rule_mutator(None);
    accepts_alert_resolver(None);
}

#[test]
fn alert_resolution_receipt_preserves_operator_visible_correlation() {
    let receipt = AlertResolutionReceipt::new(
        AlertId::new(12),
        AlarmRuleId::new(7),
        TimestampMs::new(1_720_000_000_000),
    );
    assert_eq!(receipt.alert_id(), AlertId::new(12));
    assert_eq!(receipt.rule_id(), AlarmRuleId::new(7));
    assert_eq!(receipt.resolved_at().get(), 1_720_000_000_000);
}

#[test]
fn alarm_rule_mutation_exposes_stable_target_and_kind() {
    let rule_id = AlarmRuleId::new(41);
    let patch = AlarmRulePatch::new(
        None,
        Some("high temperature".to_string()),
        Some(AlarmSeverity::new(3).expect("severity")),
        None,
        Some(90.0),
        Some(true),
        None,
    )
    .expect("valid patch");
    let update = AlarmRuleMutation::update(rule_id, patch);
    assert_eq!(update.rule_id(), Some(rule_id));
    assert_eq!(update.kind(), AlarmRuleMutationKind::Update);

    let enable = AlarmRuleMutation::set_enabled(rule_id, true);
    assert_eq!(enable.kind(), AlarmRuleMutationKind::Enable);
    assert_eq!(enable.rule_id(), Some(rule_id));
}
