use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use aether_application::{ControlApplication, SafetyPolicy};
use aether_automation::infra::application_control::{
    COMMISSIONED_RULE_ACTOR_ID, RuleActionApplication,
};
use aether_domain::{ControlCommand, TimestampMs};
use aether_ports::{
    AuditOutcome, AuditRecord, AuditSink, CommandDispatcher, CommandReceipt, PortError,
    PortErrorKind, PortResult,
};
use aether_routing::RoutingCache;
use aether_rules::{MemoryRuleLiveState, Rule, RuleExecutor, extract_rule_flow};
use async_trait::async_trait;
use serde_json::{Value, json};

#[derive(Default)]
struct RecordingAudit {
    records: Mutex<Vec<AuditRecord>>,
    unavailable: bool,
}

impl RecordingAudit {
    fn unavailable() -> Self {
        Self {
            records: Mutex::new(Vec::new()),
            unavailable: true,
        }
    }

    fn records(&self) -> Vec<AuditRecord> {
        self.records
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }
}

#[async_trait]
impl AuditSink for RecordingAudit {
    async fn record(&self, record: AuditRecord) -> PortResult<()> {
        self.records
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(record);
        if self.unavailable {
            return Err(PortError::new(
                PortErrorKind::Unavailable,
                "audit database unavailable",
            ));
        }
        Ok(())
    }
}

#[derive(Default)]
struct RecordingDispatcher {
    commands: Mutex<Vec<ControlCommand>>,
    reject: bool,
}

impl RecordingDispatcher {
    fn rejecting() -> Self {
        Self {
            commands: Mutex::new(Vec::new()),
            reject: true,
        }
    }

    fn commands(&self) -> Vec<ControlCommand> {
        self.commands
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }
}

#[async_trait]
impl CommandDispatcher for RecordingDispatcher {
    async fn dispatch(&self, command: ControlCommand) -> PortResult<CommandReceipt> {
        self.commands
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(command);
        if self.reject {
            return Err(PortError::new(
                PortErrorKind::Rejected,
                "simulated device rejection",
            ));
        }
        Ok(CommandReceipt::new(
            command.id(),
            TimestampMs::new(command.issued_at().get().saturating_add(1)),
        ))
    }
}

fn executor(dispatcher: Arc<RecordingDispatcher>, audit: Arc<RecordingAudit>) -> RuleExecutor {
    let application = Arc::new(ControlApplication::new(dispatcher, audit, SafetyPolicy));
    let action_application = Arc::new(RuleActionApplication::new(application));
    RuleExecutor::new(
        Arc::new(MemoryRuleLiveState::new()),
        Arc::new(RoutingCache::default()),
    )
    .with_action_command_facade(action_application)
}

#[derive(Default)]
struct CompletionFailingAudit {
    calls: Mutex<usize>,
    records: Mutex<Vec<AuditRecord>>,
}

#[async_trait]
impl AuditSink for CompletionFailingAudit {
    async fn record(&self, record: AuditRecord) -> PortResult<()> {
        let call = {
            let mut calls = self
                .calls
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            *calls += 1;
            *calls
        };
        if call == 2 {
            return Err(PortError::new(
                PortErrorKind::Unavailable,
                "terminal audit unavailable",
            ));
        }
        self.records
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(record);
        Ok(())
    }
}

fn executor_with_completion_audit_failure(
    dispatcher: Arc<RecordingDispatcher>,
    audit: Arc<CompletionFailingAudit>,
) -> RuleExecutor {
    let application = Arc::new(ControlApplication::new(dispatcher, audit, SafetyPolicy));
    let action_application = Arc::new(RuleActionApplication::new(application));
    RuleExecutor::new(
        Arc::new(MemoryRuleLiveState::new()),
        Arc::new(RoutingCache::default()),
    )
    .with_action_command_facade(action_application)
}

fn action_rule(id: i64, point_type: &str, value: Value) -> Rule {
    let flow = json!({
        "nodes": [
            {
                "id": "start",
                "type": "start",
                "data": { "config": { "wires": { "default": ["change"] } } }
            },
            {
                "id": "change",
                "type": "custom",
                "data": {
                    "type": "action-changeValue",
                    "config": {
                        "variables": [{
                            "name": "TARGET",
                            "type": "single",
                            "instance": 42,
                            "pointType": point_type,
                            "point": 7
                        }],
                        "rule": [{ "Variables": "TARGET", "value": value }],
                        "wires": { "default": ["end"] }
                    }
                }
            },
            { "id": "end", "type": "end" }
        ]
    });
    Rule {
        id,
        name: format!("rule-{id}"),
        description: None,
        enabled: true,
        priority: 0,
        cooldown_ms: 0,
        trigger_config: None,
        flow: extract_rule_flow(&flow).unwrap_or_else(|error| panic!("valid test rule: {error}")),
    }
}

#[tokio::test]
async fn production_rule_actions_use_control_application_without_legacy_dispatch() {
    let dispatcher = Arc::new(RecordingDispatcher::default());
    let audit = Arc::new(RecordingAudit::default());
    let executor = executor(Arc::clone(&dispatcher), Arc::clone(&audit));
    let rule = action_rule(1, "action", json!(12.5));

    let first = executor
        .execute(&rule)
        .await
        .unwrap_or_else(|error| panic!("first execution: {error}"));
    let second = executor
        .execute(&rule)
        .await
        .unwrap_or_else(|error| panic!("second execution: {error}"));

    assert!(first.actions_executed[0].success);
    assert!(second.actions_executed[0].success);
    assert!(first.success);
    assert!(second.success);
    let commands = dispatcher.commands();
    assert_eq!(commands.len(), 2);
    assert_ne!(commands[0].id(), commands[1].id());

    let records = audit.records();
    assert_eq!(records.len(), 4);
    assert_eq!(
        records.iter().map(AuditRecord::outcome).collect::<Vec<_>>(),
        vec![
            AuditOutcome::Attempted,
            AuditOutcome::Succeeded,
            AuditOutcome::Attempted,
            AuditOutcome::Succeeded,
        ]
    );
    assert!(
        records
            .iter()
            .all(|record| record.actor_id() == COMMISSIONED_RULE_ACTOR_ID)
    );

    let mut request_ids_by_attempt = HashMap::new();
    for record in &records {
        request_ids_by_attempt
            .entry(record.request_id())
            .or_insert_with(Vec::new)
            .push(record.outcome());
    }
    assert_eq!(request_ids_by_attempt.len(), 2);
    for (request_id, outcomes) in request_ids_by_attempt {
        assert_eq!(
            outcomes,
            vec![AuditOutcome::Attempted, AuditOutcome::Succeeded]
        );
        let request_uuid = uuid::Uuid::parse_str(request_id)
            .unwrap_or_else(|error| panic!("request id must be a UUID: {error}"));
        assert!(
            commands
                .iter()
                .any(|command| command.id().get() == request_uuid.as_u128())
        );
    }
}

#[tokio::test]
async fn unavailable_audit_fails_closed_before_device_dispatch() {
    let dispatcher = Arc::new(RecordingDispatcher::default());
    let audit = Arc::new(RecordingAudit::unavailable());
    let executor = executor(Arc::clone(&dispatcher), Arc::clone(&audit));

    let result = executor
        .execute(&action_rule(2, "action", json!(1.0)))
        .await
        .unwrap_or_else(|error| panic!("rule traversal: {error}"));

    assert!(!result.actions_executed[0].success);
    assert!(!result.success);
    assert_eq!(
        result.error.as_deref(),
        Some("1 of 1 attempted rule actions failed")
    );
    assert!(dispatcher.commands().is_empty());
    assert_eq!(audit.records()[0].outcome(), AuditOutcome::Attempted);
}

#[tokio::test]
async fn accepted_rule_action_stays_successful_when_only_terminal_audit_fails() {
    let dispatcher = Arc::new(RecordingDispatcher::default());
    let audit = Arc::new(CompletionFailingAudit::default());
    let executor =
        executor_with_completion_audit_failure(Arc::clone(&dispatcher), Arc::clone(&audit));

    let result = executor
        .execute(&action_rule(4, "action", json!(1.0)))
        .await
        .unwrap_or_else(|error| panic!("rule traversal: {error}"));

    assert!(result.success);
    assert!(result.actions_executed[0].success);
    assert_eq!(dispatcher.commands().len(), 1);
    let records = audit
        .records
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].outcome(), AuditOutcome::Attempted);
}

#[tokio::test]
async fn device_failure_emits_attempted_and_failed_audit() {
    let dispatcher = Arc::new(RecordingDispatcher::rejecting());
    let audit = Arc::new(RecordingAudit::default());
    let executor = executor(Arc::clone(&dispatcher), Arc::clone(&audit));

    let result = executor
        .execute(&action_rule(3, "action", json!(1.0)))
        .await
        .unwrap_or_else(|error| panic!("rule traversal: {error}"));

    assert!(!result.actions_executed[0].success);
    assert!(!result.success);
    assert_eq!(
        result.error.as_deref(),
        Some("1 of 1 attempted rule actions failed")
    );
    assert_eq!(dispatcher.commands().len(), 1);
    assert_eq!(
        audit
            .records()
            .iter()
            .map(AuditRecord::outcome)
            .collect::<Vec<_>>(),
        vec![AuditOutcome::Attempted, AuditOutcome::Failed]
    );
}

#[tokio::test]
async fn command_measurement_and_non_finite_action_targets_fail_closed() {
    let dispatcher = Arc::new(RecordingDispatcher::default());
    let audit = Arc::new(RecordingAudit::default());
    let executor = executor(Arc::clone(&dispatcher), Arc::clone(&audit));

    for rule in [
        action_rule(4, "control", json!(1.0)),
        action_rule(5, "measurement", json!(1.0)),
        action_rule(6, "action", json!("NaN")),
    ] {
        let result = executor
            .execute(&rule)
            .await
            .unwrap_or_else(|error| panic!("rule traversal: {error}"));
        assert!(
            result.actions_executed.iter().all(|action| !action.success),
            "invalid target must not produce a successful action"
        );
    }

    assert!(dispatcher.commands().is_empty());
    assert!(audit.records().is_empty());
}
