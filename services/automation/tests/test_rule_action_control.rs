use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use aether_application::{ControlApplication, SafetyPolicy};
use aether_automation::infra::application_control::{
    COMMISSIONED_RULE_ACTOR_ID, RuleActionApplication,
};
use aether_domain::{ControlCommand, InstanceId, PointId, TimestampMs};
use aether_ports::{
    AuditOutcome, AuditRecord, AuditSink, CommandDispatcher, CommandReceipt, CommandTopologyFence,
    PortError, PortErrorKind, PortResult,
};
use aether_rules::{
    MemoryRuleLiveState, Rule, RuleActionCommand, RuleActionCommandFacade, RuleExecutor,
    RuleScheduler, extract_rule_flow,
};
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
    fences: Mutex<Vec<CommandTopologyFence>>,
    reject: bool,
}

impl RecordingDispatcher {
    fn rejecting() -> Self {
        Self {
            commands: Mutex::new(Vec::new()),
            fences: Mutex::new(Vec::new()),
            reject: true,
        }
    }

    fn commands(&self) -> Vec<ControlCommand> {
        self.commands
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    fn fences(&self) -> Vec<CommandTopologyFence> {
        self.fences
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

    async fn dispatch_fenced(
        &self,
        command: ControlCommand,
        fence: CommandTopologyFence,
    ) -> PortResult<CommandReceipt> {
        self.fences
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(fence);
        self.dispatch(command).await
    }
}

fn executor(dispatcher: Arc<RecordingDispatcher>, audit: Arc<RecordingAudit>) -> RuleExecutor {
    let application = Arc::new(ControlApplication::new(dispatcher, audit, SafetyPolicy));
    let action_application = Arc::new(RuleActionApplication::new(application));
    RuleExecutor::new(Arc::new(MemoryRuleLiveState::new()))
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
    RuleExecutor::new(Arc::new(MemoryRuleLiveState::new()))
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
async fn rule_action_application_preserves_the_execution_topology_fence() {
    let dispatcher = Arc::new(RecordingDispatcher::default());
    let audit = Arc::new(RecordingAudit::default());
    let application = Arc::new(ControlApplication::new(
        Arc::clone(&dispatcher) as Arc<dyn CommandDispatcher>,
        Arc::clone(&audit) as Arc<dyn AuditSink>,
        SafetyPolicy,
    ));
    let action_application = RuleActionApplication::new(application);
    let fence = CommandTopologyFence::new(19);
    let command = RuleActionCommand::new(InstanceId::new(42), PointId::new(7), 12.5)
        .with_topology_fence(fence);

    RuleActionCommandFacade::write_action(&action_application, command)
        .await
        .expect("fenced rule action reaches the control application");

    assert_eq!(dispatcher.fences(), vec![fence]);
    assert_eq!(dispatcher.commands().len(), 1);
    assert!(audit.records().iter().all(|record| {
        record
            .detail()
            .is_some_and(|detail| detail.contains("expected_topology_sequence=19"))
    }));
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

#[derive(Default)]
struct BlockFirstTerminalAudit {
    blocked: AtomicBool,
    entered: tokio::sync::Notify,
}

#[async_trait]
impl AuditSink for BlockFirstTerminalAudit {
    async fn record(&self, record: AuditRecord) -> PortResult<()> {
        if record.outcome() == AuditOutcome::Succeeded && !self.blocked.swap(true, Ordering::SeqCst)
        {
            self.entered.notify_one();
            std::future::pending::<()>().await;
        }
        Ok(())
    }
}

#[tokio::test]
async fn accepted_action_with_blocked_terminal_audit_is_not_resent_after_scheduler_timeout() {
    assert_accepted_action_is_not_resent(false).await;
}

#[tokio::test]
async fn input_changed_during_blocked_terminal_audit_is_not_resent_after_scheduler_timeout() {
    assert_accepted_action_is_not_resent(true).await;
}

async fn assert_accepted_action_is_not_resent(change_input_while_audit_pending: bool) {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .expect("rules database");
    common::schema::init_rules_schema(&pool)
        .await
        .expect("rules schema");
    // Priority and concurrency fix the order: the OnChange command is accepted
    // before another rule can make progress. That second rule proves the
    // deadline releases the scheduler, including on a subsequent tick.
    for (rule, priority, trigger) in [
        (
            action_rule(71, "action", json!(12.5)),
            10,
            json!({
                "type": "on_change",
                "point_refs": [{"instance": 42, "point_type": "measurement", "point": 7}]
            }),
        ),
        (
            action_rule(72, "action", json!(99.0)),
            0,
            json!({
                "type": "interval", "interval_ms": 0
            }),
        ),
    ] {
        sqlx::query(
            "INSERT INTO rules
            (id, name, nodes_json, enabled, priority, cooldown_ms, trigger_config)
            VALUES (?, ?, ?, 1, ?, 0, ?)",
        )
        .bind(rule.id)
        .bind(rule.name)
        .bind(serde_json::to_string(&rule.flow).expect("compact rule flow"))
        .bind(priority)
        .bind(trigger.to_string())
        .execute(&pool)
        .await
        .expect("stored rule");
    }
    let live_state = Arc::new(MemoryRuleLiveState::new());
    assert!(live_state.set_instance(42, 0, 7, 2.0, 1));
    let dispatcher = Arc::new(RecordingDispatcher::default());
    let audit = Arc::new(BlockFirstTerminalAudit::default());
    let application = Arc::new(ControlApplication::new(
        dispatcher.clone(),
        audit.clone(),
        SafetyPolicy,
    ));
    let action_application = Arc::new(RuleActionApplication::new(application));
    let logs = tempfile::tempdir().expect("rule logs");
    let mut scheduler = RuleScheduler::with_state_store(
        live_state.clone(),
        pool,
        100,
        logs.path().to_path_buf(),
        Arc::new(aether_calc::MemoryStateStore::new()),
        Some(action_application),
    );
    scheduler.set_max_concurrency(1);
    assert_eq!(
        scheduler.load_rules().await.expect("load database rules"),
        2
    );
    let scheduler = Arc::new(scheduler);

    let task_scheduler = scheduler.clone();
    let task = tokio::spawn(async move { task_scheduler.start().await });
    tokio::time::timeout(Duration::from_secs(1), audit.entered.notified())
        .await
        .expect("first accepted command reaches its blocked terminal audit");
    // Keep startup and initial I/O on real time; advance only once the
    // accepted command is known to be waiting on its terminal audit.
    tokio::time::pause();
    assert_eq!(
        dispatcher.commands().len(),
        1,
        "dispatch is already accepted"
    );
    if change_input_while_audit_pending {
        // The scheduler sampled 2 before execution. Input changes to 3 after
        // dispatch acceptance, while the terminal audit still blocks. Holding
        // only that old sample must not turn stable 3 into an automatic retry.
        assert!(live_state.set_instance(42, 0, 7, 3.0, 2));
    }
    tokio::time::advance(Duration::from_secs(31)).await;

    // Poll observable dispatches while SQLite's real worker commits history;
    // wall time bounds failure without advancing any additional virtual time.
    let deadline = Instant::now() + Duration::from_secs(5);
    let progressed = loop {
        let other_commands = dispatcher
            .commands()
            .iter()
            .filter(|command| command.value() == 99.0)
            .count();
        if other_commands >= 2 {
            break true;
        }
        if Instant::now() >= deadline {
            break false;
        }
        tokio::task::yield_now().await;
    };
    scheduler.stop();
    task.abort();
    let _ = task.await;
    assert!(
        progressed,
        "another rule must execute after the deadline and on the next tick"
    );
    let accepted_for_unchanged_input = dispatcher
        .commands()
        .iter()
        .filter(|command| command.value() == 12.5)
        .count();
    assert_eq!(
        accepted_for_unchanged_input, 1,
        "an accepted action whose terminal audit timed out must not be automatically replayed"
    );
}
