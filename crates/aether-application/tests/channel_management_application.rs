use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

use aether_application::{
    Actor, ApplicationError, AuditPolicy, ChannelManagementApplication, ConfirmationPolicy,
    MANAGE_CHANNEL_CAPABILITY, OperationKind, RequestContext, RiskLevel, SafetyPolicy,
    capability_catalog,
};
use aether_domain::{ChannelId, TimestampMs};
use aether_ports::{
    AuditOutcome, AuditRecord, AuditSink, ChannelDefinition, ChannelLoggingPolicy, ChannelMutation,
    ChannelMutationReceipt, ChannelMutator, ChannelParameterValue, ChannelPatch, ChannelRevision,
    ChannelRuntimeProjection, PortError, PortErrorKind, PortResult,
};
use async_trait::async_trait;

struct RecordingMutator {
    mutations: Mutex<Vec<ChannelMutation>>,
    events: Arc<Mutex<Vec<&'static str>>>,
    failure: Option<PortError>,
    projection: Option<ChannelRuntimeProjection>,
}

impl RecordingMutator {
    fn successful(events: Arc<Mutex<Vec<&'static str>>>) -> Self {
        Self {
            mutations: Mutex::new(Vec::new()),
            events,
            failure: None,
            projection: None,
        }
    }

    fn failing(events: Arc<Mutex<Vec<&'static str>>>) -> Self {
        Self {
            mutations: Mutex::new(Vec::new()),
            events,
            failure: Some(PortError::new(
                PortErrorKind::Conflict,
                "channel is referenced by an action route",
            )),
            projection: None,
        }
    }

    fn degraded(events: Arc<Mutex<Vec<&'static str>>>) -> Self {
        Self {
            mutations: Mutex::new(Vec::new()),
            events,
            failure: None,
            projection: Some(ChannelRuntimeProjection::Degraded),
        }
    }
}

#[async_trait]
impl ChannelMutator for RecordingMutator {
    async fn mutate(&self, mutation: ChannelMutation) -> PortResult<ChannelMutationReceipt> {
        let kind = mutation.kind();
        let channel_id = mutation.channel_id().unwrap_or_else(|| ChannelId::new(101));
        let resulting_revision = mutation
            .expected_revision()
            .map_or(ChannelRevision::new(1), |revision| {
                revision.checked_next().expect("test revision has capacity")
            });
        let desired_enabled = match &mutation {
            ChannelMutation::Create { definition } => definition.enabled(),
            ChannelMutation::SetEnabled { enabled, .. } => *enabled,
            ChannelMutation::Delete { .. } => false,
            ChannelMutation::Update { .. } => true,
        };
        let projection = self.projection.unwrap_or(match kind {
            aether_ports::ChannelMutationKind::Create
            | aether_ports::ChannelMutationKind::Disable => ChannelRuntimeProjection::Stopped,
            aether_ports::ChannelMutationKind::Update => ChannelRuntimeProjection::Active,
            aether_ports::ChannelMutationKind::Delete => ChannelRuntimeProjection::Removed,
            aether_ports::ChannelMutationKind::Enable => {
                ChannelRuntimeProjection::ActivationPending
            },
        });
        self.events.lock().expect("event lock").push("mutate");
        self.mutations.lock().expect("mutation lock").push(mutation);
        if let Some(failure) = &self.failure {
            return Err(failure.clone());
        }
        Ok(ChannelMutationReceipt::new(
            channel_id,
            kind,
            resulting_revision,
            desired_enabled,
            projection,
        ))
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

fn context(permission: bool, confirmed: bool) -> RequestContext {
    let actor = if permission {
        Actor::new("commissioner").with_permission("io.channel.manage")
    } else {
        Actor::new("commissioner")
    };
    RequestContext::new(
        "channel-management-1",
        actor,
        confirmed,
        TimestampMs::new(2_000),
    )
}

fn definition() -> ChannelDefinition {
    let parameters = BTreeMap::from([(
        "never-audit-parameter-key".to_string(),
        ChannelParameterValue::String("never-audit-me".to_string()),
    )]);
    ChannelDefinition::new(
        Some(ChannelId::new(7)),
        "packaging-plc",
        "modbus_tcp",
        parameters,
    )
    .with_description("primary packaging controller")
    .with_logging(
        ChannelLoggingPolicy::default()
            .with_enabled(true)
            .with_level("never-audit-log-level")
            .with_file("/var/log/never-audit-channel-file.log"),
    )
}

fn mutations() -> Vec<ChannelMutation> {
    vec![
        ChannelMutation::create(definition()),
        ChannelMutation::update_with_revision(
            ChannelId::new(7),
            ChannelRevision::new(3),
            ChannelPatch::new()
                .with_name("packaging-plc-2")
                .with_parameters(BTreeMap::from([(
                    "also-never-audit-parameter-key".to_string(),
                    ChannelParameterValue::String("also-secret".to_string()),
                )]))
                .with_logging(
                    ChannelLoggingPolicy::default()
                        .with_enabled(true)
                        .with_level("also-never-audit-log-level")
                        .with_file("/var/log/also-never-audit.log"),
                ),
        ),
        ChannelMutation::delete_with_revision(ChannelId::new(7), ChannelRevision::new(3)),
        ChannelMutation::enable_with_revision(ChannelId::new(7), ChannelRevision::new(3)),
        ChannelMutation::disable_with_revision(ChannelId::new(7), ChannelRevision::new(3)),
    ]
}

#[test]
fn channel_management_is_high_risk_confirmed_audited_and_non_idempotent() {
    assert_eq!(MANAGE_CHANNEL_CAPABILITY.name(), "io.channel.manage");
    assert_eq!(MANAGE_CHANNEL_CAPABILITY.kind(), OperationKind::Command);
    assert_eq!(MANAGE_CHANNEL_CAPABILITY.risk(), RiskLevel::High);
    assert_eq!(
        MANAGE_CHANNEL_CAPABILITY.required_permission(),
        "io.channel.manage"
    );
    assert_eq!(
        MANAGE_CHANNEL_CAPABILITY.confirmation(),
        ConfirmationPolicy::Always
    );
    assert_eq!(
        MANAGE_CHANNEL_CAPABILITY.audit_policy(),
        AuditPolicy::Required
    );
    assert!(!MANAGE_CHANNEL_CAPABILITY.is_idempotent());
    assert!(capability_catalog().contains(&MANAGE_CHANNEL_CAPABILITY));
}

#[tokio::test]
async fn every_channel_mutation_is_attempt_audited_before_the_port_and_terminal_audited() {
    for mutation in mutations() {
        let expected_kind = mutation.kind();
        let expected_revision = mutation
            .expected_revision()
            .map_or(ChannelRevision::new(1), |revision| {
                revision.checked_next().expect("test revision has capacity")
            });
        let events = Arc::new(Mutex::new(Vec::new()));
        let mutator = Arc::new(RecordingMutator::successful(Arc::clone(&events)));
        let audit = Arc::new(RecordingAudit::successful(Arc::clone(&events)));
        let application =
            ChannelManagementApplication::new(mutator.clone(), audit.clone(), SafetyPolicy);

        let acceptance = application
            .mutate(&context(true, true), mutation)
            .await
            .expect("governed channel mutation succeeds");

        assert_eq!(acceptance.kind(), expected_kind);
        assert_eq!(acceptance.channel_id(), ChannelId::new(7));
        assert_eq!(acceptance.resulting_revision(), expected_revision);
        assert_eq!(
            acceptance.reconciliation_required(),
            matches!(
                acceptance.runtime_projection(),
                ChannelRuntimeProjection::ActivationPending | ChannelRuntimeProjection::Degraded
            )
        );
        assert_eq!(acceptance.request_id(), "channel-management-1");
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
                .all(|record| record.capability() == "io.channel.manage")
        );
        assert!(records.iter().all(|record| {
            !record
                .detail()
                .unwrap_or_default()
                .contains("never-audit-me")
        }));
        assert!(
            records
                .iter()
                .all(|record| !record.detail().unwrap_or_default().contains("also-secret"))
        );
        assert!(records.iter().all(|record| {
            let detail = record.detail().unwrap_or_default();
            !detail.contains("never-audit-channel-file")
                && !detail.contains("also-never-audit")
                && !detail.contains("never-audit-parameter-key")
                && !detail.contains("never-audit-log-level")
                && !detail.contains("modbus_tcp")
                && !detail.contains("parameter_sha256")
                && !detail.contains("logging_enabled")
                && !detail.contains("logging_level")
                && !detail.contains("logging_file")
        }));
        if expected_kind == aether_ports::ChannelMutationKind::Update {
            assert!(
                records
                    .iter()
                    .filter_map(AuditRecord::detail)
                    .all(|detail| { detail.contains("changed_fields=name,parameters,logging") })
            );
        } else if expected_kind == aether_ports::ChannelMutationKind::Create {
            assert!(
                records
                    .iter()
                    .filter_map(AuditRecord::detail)
                    .all(|detail| detail.contains(
                        "changed_fields=name,description,protocol,parameters,logging,enabled"
                    ))
            );
        }
    }
}

#[tokio::test]
async fn rejected_protocol_text_is_digest_only_and_audit_detail_is_bounded() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let mutator = Arc::new(RecordingMutator::successful(Arc::clone(&events)));
    let audit = Arc::new(RecordingAudit::successful(Arc::clone(&events)));
    let application =
        ChannelManagementApplication::new(mutator.clone(), audit.clone(), SafetyPolicy);
    let attacker_text = "unauthenticated-protocol-payload".repeat(32_768);
    let mutation = ChannelMutation::create(ChannelDefinition::new(
        None,
        "bounded-audit",
        attacker_text.clone(),
        BTreeMap::new(),
    ));

    let result = application.mutate(&context(false, true), mutation).await;

    assert!(matches!(
        result,
        Err(ApplicationError::PermissionDenied { .. })
    ));
    assert!(mutator.mutations.lock().expect("mutation lock").is_empty());
    let records = audit.records.lock().expect("record lock");
    let detail = records[0].detail().expect("rejected audit detail");
    assert!(detail.len() < 1_024, "audit detail must remain bounded");
    assert!(!detail.contains(&attacker_text));
    assert!(detail.contains("protocol_sha256="));
    assert!(detail.contains("protocol_bytes="));
}

#[tokio::test]
async fn denied_or_unconfirmed_mutation_is_rejected_and_never_reaches_the_port() {
    for request_context in [context(false, true), context(true, false)] {
        let events = Arc::new(Mutex::new(Vec::new()));
        let mutator = Arc::new(RecordingMutator::successful(Arc::clone(&events)));
        let audit = Arc::new(RecordingAudit::successful(Arc::clone(&events)));
        let application =
            ChannelManagementApplication::new(mutator.clone(), audit.clone(), SafetyPolicy);

        let result = application
            .mutate(
                &request_context,
                ChannelMutation::enable_with_revision(ChannelId::new(7), ChannelRevision::new(3)),
            )
            .await;

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
    let application = ChannelManagementApplication::new(mutator.clone(), audit, SafetyPolicy);

    let result = application
        .mutate(
            &context(true, true),
            ChannelMutation::enable_with_revision(ChannelId::new(7), ChannelRevision::new(3)),
        )
        .await;

    assert!(matches!(result, Err(ApplicationError::AuditUnavailable(_))));
    assert!(mutator.mutations.lock().expect("mutation lock").is_empty());
    assert!(events.lock().expect("event lock").is_empty());
}

#[tokio::test]
async fn port_failure_is_returned_and_failed_audited() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let mutator = Arc::new(RecordingMutator::failing(Arc::clone(&events)));
    let audit = Arc::new(RecordingAudit::successful(Arc::clone(&events)));
    let application = ChannelManagementApplication::new(mutator, audit, SafetyPolicy);

    let result = application
        .mutate(
            &context(true, true),
            ChannelMutation::delete_with_revision(ChannelId::new(7), ChannelRevision::new(3)),
        )
        .await;

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
    let application = ChannelManagementApplication::new(mutator.clone(), audit, SafetyPolicy);

    let acceptance = application
        .mutate(
            &context(true, true),
            ChannelMutation::enable_with_revision(ChannelId::new(7), ChannelRevision::new(3)),
        )
        .await
        .expect("accepted non-idempotent mutations are not retryable errors");

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

#[tokio::test]
async fn committed_desired_state_with_degraded_runtime_is_accepted_for_reconciliation() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let mutator = Arc::new(RecordingMutator::degraded(Arc::clone(&events)));
    let audit = Arc::new(RecordingAudit::successful(Arc::clone(&events)));
    let application = ChannelManagementApplication::new(mutator, audit, SafetyPolicy);

    let acceptance = application
        .mutate(
            &context(true, true),
            ChannelMutation::enable_with_revision(ChannelId::new(7), ChannelRevision::new(3)),
        )
        .await
        .expect("committed desired state is accepted even when projection is degraded");

    assert_eq!(
        acceptance.runtime_projection(),
        ChannelRuntimeProjection::Degraded
    );
    assert!(acceptance.reconciliation_required());
    assert!(!acceptance.is_retryable());
}

#[tokio::test]
async fn invalid_common_channel_data_is_rejected_without_port_side_effects() {
    let invalid_mutations = vec![
        ChannelMutation::create(ChannelDefinition::new(
            None,
            "   ",
            "modbus_tcp",
            BTreeMap::new(),
        )),
        ChannelMutation::create(ChannelDefinition::new(
            None,
            "valid-name",
            "\t",
            BTreeMap::new(),
        )),
        ChannelMutation::enable(ChannelId::new(10_000)),
        ChannelMutation::update(ChannelId::new(7), ChannelPatch::new()),
        ChannelMutation::update(ChannelId::new(7), ChannelPatch::new().with_name("\n\t")),
        ChannelMutation::update(ChannelId::new(7), ChannelPatch::new().with_protocol("  ")),
        ChannelMutation::create(ChannelDefinition::new(
            None,
            "valid-name",
            "virtual",
            BTreeMap::from([(
                "nested".to_string(),
                ChannelParameterValue::Object(BTreeMap::from([(
                    "array".to_string(),
                    ChannelParameterValue::Array(vec![ChannelParameterValue::Float(f64::NAN)]),
                )])),
            )]),
        )),
        ChannelMutation::create(ChannelDefinition::new(
            Some(ChannelId::new(10_000)),
            "valid-name",
            "virtual",
            BTreeMap::new(),
        )),
    ];

    for mutation in invalid_mutations {
        let events = Arc::new(Mutex::new(Vec::new()));
        let mutator = Arc::new(RecordingMutator::successful(Arc::clone(&events)));
        let audit = Arc::new(RecordingAudit::successful(Arc::clone(&events)));
        let application = ChannelManagementApplication::new(mutator.clone(), audit, SafetyPolicy);

        let result = application.mutate(&context(true, true), mutation).await;

        assert!(matches!(
            result,
            Err(ApplicationError::InvalidChannelMutation(_))
        ));
        assert!(mutator.mutations.lock().expect("mutation lock").is_empty());
        assert_eq!(*events.lock().expect("event lock"), vec!["audit.rejected"]);
    }
}

#[tokio::test]
async fn every_explicit_zero_revision_is_rejected_without_port_side_effects() {
    let zero = ChannelRevision::new(0);
    let invalid_mutations = [
        ChannelMutation::update_with_revision(
            ChannelId::new(7),
            zero,
            ChannelPatch::new().with_name("valid"),
        ),
        ChannelMutation::delete_with_revision(ChannelId::new(7), zero),
        ChannelMutation::enable_with_revision(ChannelId::new(7), zero),
        ChannelMutation::disable_with_revision(ChannelId::new(7), zero),
    ];

    for mutation in invalid_mutations {
        let events = Arc::new(Mutex::new(Vec::new()));
        let mutator = Arc::new(RecordingMutator::successful(Arc::clone(&events)));
        let audit = Arc::new(RecordingAudit::successful(Arc::clone(&events)));
        let application = ChannelManagementApplication::new(mutator.clone(), audit, SafetyPolicy);

        let result = application.mutate(&context(true, true), mutation).await;

        assert!(matches!(
            result,
            Err(ApplicationError::InvalidChannelMutation(reason))
                if reason == "expected_revision must be at least 1"
        ));
        assert!(mutator.mutations.lock().expect("mutation lock").is_empty());
        assert_eq!(*events.lock().expect("event lock"), vec!["audit.rejected"]);
    }
}
