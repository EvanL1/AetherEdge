use std::sync::Arc;

use aether_ports::{
    AuditSink, CommandDispatcher, DurableOutbox, HistorySink, LiveState, LiveStateWriter,
    PortError, PortErrorKind, StateMirror,
};

#[test]
fn error_kind_exposes_recovery_semantics() {
    let unavailable = PortError::new(PortErrorKind::Unavailable, "device offline");
    let timeout = PortError::new(PortErrorKind::Timeout, "request timed out");
    let rejected = PortError::new(PortErrorKind::Rejected, "interlock open");
    let permanent = PortError::new(PortErrorKind::Permanent, "invalid credentials");

    assert!(unavailable.is_retryable());
    assert!(timeout.is_retryable());
    assert!(!rejected.is_retryable());
    assert!(!permanent.is_retryable());
    assert_eq!(rejected.kind(), PortErrorKind::Rejected);
    assert_eq!(rejected.message(), "interlock open");
}

#[test]
fn extension_ports_are_object_safe() {
    fn accepts_live_state(_: Option<Arc<dyn LiveState>>) {}
    fn accepts_live_state_writer(_: Option<Arc<dyn LiveStateWriter>>) {}
    fn accepts_dispatcher(_: Option<Arc<dyn CommandDispatcher>>) {}
    fn accepts_history(_: Option<Arc<dyn HistorySink>>) {}
    fn accepts_outbox(_: Option<Arc<dyn DurableOutbox>>) {}
    fn accepts_mirror(_: Option<Arc<dyn StateMirror>>) {}
    fn accepts_audit(_: Option<Arc<dyn AuditSink>>) {}

    accepts_live_state(None);
    accepts_live_state_writer(None);
    accepts_dispatcher(None);
    accepts_history(None);
    accepts_outbox(None);
    accepts_mirror(None);
    accepts_audit(None);
}
