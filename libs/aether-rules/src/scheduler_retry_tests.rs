use super::tests::{add_test_history, create_action_rule, rule_pool};
use super::*;
use crate::types::{RuleNode, RuleSwitchBranch, RuleValueAssignment, RuleVariable, RuleWires};
use crate::{MemoryRuleLiveState, RuleActionCommand, RuleExecutionContext};
use aether_domain::{CommandId, TimestampMs};
use aether_ports::{CommandReceipt, PortError, PortErrorKind, PortResult};
use async_trait::async_trait;
use serde_json::json;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

async fn wait_for_entry(entered: &tokio::sync::Notify) {
    tokio::time::timeout(Duration::from_secs(2), entered.notified())
        .await
        .expect("execution entered command facade in real time");
}

fn input_point() -> PointRef {
    PointRef {
        instance: 42,
        point_type: PointKind::Measurement,
        point: 7,
    }
}

fn input_action_rule() -> Rule {
    let mut rule = create_action_rule(91, 0);
    rule.trigger_config = Some(
        serde_json::to_string(&TriggerConfig::OnChange {
            point_refs: vec![input_point()],
            time_deadband_ms: None,
            value_deadband: None,
        })
        .expect("trigger"),
    );
    rule.flow.nodes.insert(
        "start".to_string(),
        RuleNode::Start {
            wires: RuleWires {
                default: vec!["input".to_string()],
            },
        },
    );
    rule.flow.nodes.insert(
        "input".to_string(),
        RuleNode::Switch {
            variables: vec![RuleVariable {
                name: "INPUT".to_string(),
                instance: Some(42),
                point_type: Some("measurement".to_string()),
                point: Some(7),
                formula: Vec::new(),
            }],
            rule: vec![RuleSwitchBranch {
                name: "out".to_string(),
                rule_type: "default".to_string(),
                rule: Vec::new(),
            }],
            wires: HashMap::from([("out".to_string(), vec!["action".to_string()])]),
        },
    );
    if let RuleNode::ChangeValue {
        rule: assignments, ..
    } = rule.flow.nodes.get_mut("action").expect("action")
    {
        assignments.push(RuleValueAssignment {
            variables: "TARGET".to_string(),
            value: json!(2.0),
        });
    }
    rule
}

async fn fixture<L: RuleLiveState + 'static>(
    live: Arc<L>,
    commands: Arc<dyn RuleActionCommandFacade>,
) -> (Arc<RuleScheduler>, tempfile::TempDir) {
    let rule = input_action_rule();
    let pool = rule_pool(&rule).await;
    add_test_history(&pool).await;
    let logs = tempfile::tempdir().expect("logs");
    let scheduler = Arc::new(RuleScheduler::with_state_store(
        live,
        pool,
        100,
        logs.path().to_path_buf(),
        Arc::new(MemoryStateStore::new()),
        Some(commands),
    ));
    scheduler.load_rules().await.expect("load rule");
    scheduler.rules.write().await[0]
        .onchange_state
        .last_value
        .insert(input_point().cache_key(), 1.0);
    (scheduler, logs)
}

async fn execute_mode(scheduler: &RuleScheduler, mode: &str) {
    match mode {
        "manual" => {
            if let Err(error) = scheduler.execute_rule(91).await {
                assert!(
                    matches!(&error, crate::RuleError::SchedulerError(_)),
                    "unexpected manual execution failure: {error}"
                );
            }
        },
        "watch" => scheduler
            .execute_watch_triggered(&crate::point_watch_dispatcher::WatchEvent {
                rule_ids: vec![91],
                channel_id: 1,
                point_kind: aether_domain::PointKind::Telemetry,
                point_id: 7,
                value: 99.0,
                raw: 99.0,
                timestamp_ms: 1,
            })
            .await
            .expect("watch execution"),
        _ => scheduler.tick().await.expect("tick execution"),
    }
}

struct DriftOnExecution {
    live: MemoryRuleLiveState,
    drift: AtomicBool,
}

impl RuleLiveState for DriftOnExecution {
    fn begin_execution(&self) -> RuleExecutionContext {
        if self.drift.swap(false, Ordering::SeqCst) {
            assert!(self.live.set_instance(42, 0, 7, 3.0, 2));
        }
        RuleExecutionContext::unfenced()
    }
    fn get_instance(&self, instance: u32, kind: u8, point: u32) -> Option<(f64, u64)> {
        self.live.get_instance(instance, kind, point)
    }
}

#[derive(Default)]
struct PartialCommands(AtomicUsize);

#[async_trait]
impl RuleActionCommandFacade for PartialCommands {
    async fn write_action(&self, _command: RuleActionCommand) -> PortResult<CommandReceipt> {
        if self.0.fetch_add(1, Ordering::SeqCst).is_multiple_of(2) {
            Ok(CommandReceipt::new(CommandId::new(1), TimestampMs::new(1)))
        } else {
            Err(PortError::new(
                PortErrorKind::Rejected,
                "second action rejected",
            ))
        }
    }
}

#[tokio::test]
async fn partial_failure_holds_executor_input_in_tick_watch_and_manual() {
    for mode in ["tick", "watch", "manual"] {
        let live = Arc::new(DriftOnExecution {
            live: MemoryRuleLiveState::new(),
            drift: AtomicBool::new(true),
        });
        assert!(live.live.set_instance(42, 0, 7, 2.0, 1));
        let commands = Arc::new(PartialCommands::default());
        let (scheduler, _logs) = fixture(live, commands.clone()).await;
        execute_mode(&scheduler, mode).await;
        scheduler.tick().await.expect("stable executor input tick");
        assert_eq!(
            commands.0.load(Ordering::SeqCst),
            2,
            "{mode}: stable input read by the executor must not repeat an accepted action"
        );
    }
}

struct BlockingCommands {
    calls: AtomicUsize,
    first_entered: tokio::sync::Notify,
    first_release: tokio::sync::Notify,
    rejected_call: Option<usize>,
    hang_all: bool,
}

impl BlockingCommands {
    fn new(rejected_call: Option<usize>, hang_all: bool) -> Self {
        Self {
            calls: AtomicUsize::new(0),
            first_entered: tokio::sync::Notify::new(),
            first_release: tokio::sync::Notify::new(),
            rejected_call,
            hang_all,
        }
    }
}

#[async_trait]
impl RuleActionCommandFacade for BlockingCommands {
    async fn write_action(&self, _command: RuleActionCommand) -> PortResult<CommandReceipt> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        if call == 0 {
            self.first_entered.notify_one();
            if !self.hang_all {
                self.first_release.notified().await;
            }
        }
        if self.hang_all {
            return std::future::pending().await;
        }
        if self.rejected_call == Some(call) {
            Err(PortError::new(
                PortErrorKind::Rejected,
                "accepted first action, rejected second",
            ))
        } else {
            Ok(CommandReceipt::new(
                CommandId::new(call as u128 + 1),
                TimestampMs::new(1),
            ))
        }
    }
}

#[tokio::test]
async fn timeout_holds_input_sampled_after_timeout_in_tick_watch_and_manual() {
    for mode in ["tick", "watch", "manual"] {
        let live = Arc::new(MemoryRuleLiveState::new());
        assert!(live.set_instance(42, 0, 7, 2.0, 1));
        let commands = Arc::new(BlockingCommands::new(None, true));
        let (scheduler, _logs) = fixture(live.clone(), commands.clone()).await;
        let task_scheduler = scheduler.clone();
        let task = tokio::spawn(async move {
            execute_mode(&task_scheduler, mode).await;
        });
        wait_for_entry(&commands.first_entered).await;
        tokio::time::pause();
        assert!(live.set_instance(42, 0, 7, 3.0, 2));
        tokio::time::advance(Duration::from_secs(31)).await;
        tokio::time::resume();
        tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .expect("bounded attempt")
            .expect("task");
        let held = tokio::time::timeout(Duration::from_millis(100), scheduler.tick()).await;
        assert!(
            held.is_ok(),
            "{mode}: stable post-timeout input must remain held"
        );
        assert_eq!(commands.calls.load(Ordering::SeqCst), 1);
    }
}

#[tokio::test]
async fn stale_manual_and_automatic_success_cannot_clear_a_newer_retry_hold() {
    for (old_mode, new_mode) in [
        ("manual", "tick"),
        ("manual", "watch"),
        ("tick", "manual"),
        ("watch", "manual"),
    ] {
        for initially_held in [true, false] {
            let live = Arc::new(MemoryRuleLiveState::new());
            assert!(live.set_instance(42, 0, 7, 2.0, 1));
            let commands = Arc::new(BlockingCommands::new(Some(2), false));
            let (scheduler, _logs) = fixture(live.clone(), commands.clone()).await;
            if initially_held {
                scheduler.rules.write().await[0].set_retry_hold(
                    true,
                    &HashMap::from([(input_point().cache_key(), Some(2.0))]),
                );
                if old_mode != "manual" {
                    assert!(live.set_instance(42, 0, 7, 3.0, 2));
                }
            }
            let task_scheduler = scheduler.clone();
            let old = tokio::spawn(async move {
                execute_mode(&task_scheduler, old_mode).await;
            });
            wait_for_entry(&commands.first_entered).await;
            let new_input = if initially_held && old_mode != "manual" {
                4.0
            } else {
                3.0
            };
            assert!(live.set_instance(42, 0, 7, new_input, 3));
            execute_mode(&scheduler, new_mode).await;
            // Metadata reload must preserve the newer hold's identity.
            sqlx::query("UPDATE rules SET name = 'renamed' WHERE id = 91")
                .execute(&scheduler.pool)
                .await
                .expect("rename");
            scheduler.reload_rules().await.expect("metadata reload");
            commands.first_release.notify_one();
            old.await.expect("old completion");
            let rules = scheduler.rules.read().await;
            assert!(
                rules[0].retry_hold.is_some(),
                "{old_mode}->{new_mode}: stale success cleared new hold"
            );
            assert_eq!(
                rules[0].onchange_state.last_value[&input_point().cache_key()],
                1.0,
                "stale completion must not consume an uncertain input"
            );
            drop(rules);
            scheduler.tick().await.expect("stable held input");
            assert_eq!(
                commands.calls.load(Ordering::SeqCst),
                4,
                "{old_mode}->{new_mode}: stable input must not replay after stale completion"
            );
        }
    }
}

#[tokio::test]
async fn execution_completed_under_an_old_definition_cannot_mutate_reloaded_rule() {
    for mode in ["tick", "watch", "manual"] {
        for edit in ["enabled", "trigger", "flow", "metadata"] {
            let live = Arc::new(MemoryRuleLiveState::new());
            assert!(live.set_instance(42, 0, 7, 2.0, 1));
            let commands = Arc::new(BlockingCommands::new(Some(1), false));
            let (scheduler, _logs) = fixture(live, commands.clone()).await;
            let task_scheduler = scheduler.clone();
            let old = tokio::spawn(async move {
                execute_mode(&task_scheduler, mode).await;
            });
            wait_for_entry(&commands.first_entered).await;
            let mut edited = input_action_rule();
            match edit {
                "enabled" => edited.enabled = false,
                "trigger" => {
                    edited.trigger_config =
                        Some(r#"{"type":"interval","interval_ms":100}"#.to_string())
                },
                "flow" => {
                    edited.flow.nodes.remove("input");
                },
                _ => edited.name = "renamed".to_string(),
            }
            sqlx::query("UPDATE rules SET name = ?, enabled = ?, trigger_config = ?, nodes_json = ? WHERE id = 91")
                .bind(&edited.name).bind(edited.enabled).bind(&edited.trigger_config)
                .bind(serde_json::to_string(&edited.flow).expect("flow"))
                .execute(&scheduler.pool).await.expect("edit rule");
            scheduler.reload_rules().await.expect("reload edited rule");
            commands.first_release.notify_one();
            old.await.expect("old completion");
            let rules = scheduler.rules.read().await;
            if edit == "enabled" {
                assert!(
                    rules.is_empty(),
                    "disabled rule stays absent after old completion"
                );
                continue;
            }
            if edit == "metadata" {
                assert!(
                    rules[0].retry_hold.is_some(),
                    "metadata reload preserves completion authority"
                );
            } else {
                assert!(
                    rules[0].retry_hold.is_none(),
                    "{mode}/{edit}: stale definition installed a hold"
                );
                assert!(
                    rules[0].last_execution.is_none(),
                    "stale definition advanced execution time"
                );
                assert!(rules[0].last_cooldown_start.is_none());
                assert_eq!(
                    rules[0].onchange_state.last_value[&input_point().cache_key()],
                    1.0
                );
            }
        }
    }
}

struct CompletionGate {
    entered: tokio::sync::Notify,
    release: tokio::sync::Notify,
}

struct TwoPartialFailures {
    calls: AtomicUsize,
    failures: AtomicUsize,
    gates: [CompletionGate; 2],
}

#[async_trait]
impl RuleActionCommandFacade for TwoPartialFailures {
    async fn write_action(&self, command: RuleActionCommand) -> PortResult<CommandReceipt> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if command.value() == 1.0 {
            return Ok(CommandReceipt::new(CommandId::new(1), TimestampMs::new(1)));
        }
        let failure = self.failures.fetch_add(1, Ordering::SeqCst);
        if let Some(gate) = self.gates.get(failure) {
            gate.entered.notify_one();
            gate.release.notified().await;
        }
        Err(PortError::new(
            PortErrorKind::Rejected,
            "second action rejected after acceptance",
        ))
    }
}

#[tokio::test]
async fn interleaved_partial_failures_hold_current_input_in_both_completion_orders() {
    for (old_mode, new_mode) in [
        ("manual", "tick"),
        ("manual", "watch"),
        ("tick", "manual"),
        ("watch", "manual"),
    ] {
        for old_finishes_first in [true, false] {
            let live = Arc::new(MemoryRuleLiveState::new());
            assert!(live.set_instance(42, 0, 7, 2.0, 1));
            let commands = Arc::new(TwoPartialFailures {
                calls: AtomicUsize::new(0),
                failures: AtomicUsize::new(0),
                gates: std::array::from_fn(|_| CompletionGate {
                    entered: tokio::sync::Notify::new(),
                    release: tokio::sync::Notify::new(),
                }),
            });
            let (scheduler, _logs) = fixture(live.clone(), commands.clone()).await;
            let old_scheduler = scheduler.clone();
            let old = tokio::spawn(async move {
                execute_mode(&old_scheduler, old_mode).await;
            });
            wait_for_entry(&commands.gates[0].entered).await;
            assert!(live.set_instance(42, 0, 7, 3.0, 2));
            let new_scheduler = scheduler.clone();
            let new = tokio::spawn(async move {
                execute_mode(&new_scheduler, new_mode).await;
            });
            wait_for_entry(&commands.gates[1].entered).await;
            if old_finishes_first {
                commands.gates[0].release.notify_one();
                old.await.expect("old partial completion");
                commands.gates[1].release.notify_one();
                new.await.expect("new partial completion");
            } else {
                commands.gates[1].release.notify_one();
                new.await.expect("new partial completion");
                commands.gates[0].release.notify_one();
                old.await.expect("old partial completion");
            }
            scheduler
                .tick()
                .await
                .expect("stable input after both partial failures");
            assert_eq!(
                commands.calls.load(Ordering::SeqCst),
                4,
                "{old_mode}->{new_mode}, old first={old_finishes_first}: stable input repeated an accepted action"
            );
            assert_eq!(
                scheduler.rules.read().await[0].onchange_state.last_value
                    [&input_point().cache_key()],
                1.0
            );
        }
    }
}

#[tokio::test]
async fn unrelated_reload_keeps_uncertain_completion_when_rule_order_changes() {
    for mode in ["tick", "watch", "manual"] {
        let live = Arc::new(MemoryRuleLiveState::new());
        assert!(live.set_instance(42, 0, 7, 2.0, 1));
        let commands = Arc::new(BlockingCommands::new(Some(1), false));
        let (scheduler, _logs) = fixture(live, commands.clone()).await;
        let task_scheduler = scheduler.clone();
        let old = tokio::spawn(async move {
            execute_mode(&task_scheduler, mode).await;
        });
        wait_for_entry(&commands.first_entered).await;
        let mut unrelated = input_action_rule();
        unrelated.id = 92;
        unrelated.priority = 200;
        unrelated.flow.start_node = "start".to_string();
        unrelated.flow.nodes = HashMap::from([
            (
                "start".to_string(),
                RuleNode::Start {
                    wires: RuleWires {
                        default: vec!["end".to_string()],
                    },
                },
            ),
            ("end".to_string(), RuleNode::End),
        ]);
        sqlx::query("INSERT INTO rules (id, name, enabled, priority, cooldown_ms, trigger_config, nodes_json) VALUES (?, ?, ?, ?, ?, ?, ?)")
            .bind(unrelated.id).bind(&unrelated.name).bind(true).bind(unrelated.priority)
            .bind(0).bind(&unrelated.trigger_config)
            .bind(serde_json::to_string(&unrelated.flow).expect("unrelated flow"))
            .execute(&scheduler.pool).await.expect("insert unrelated rule");
        scheduler
            .reload_rules()
            .await
            .expect("reload reordered rules");
        assert_eq!(
            scheduler.rules.read().await[1].rule.id,
            91,
            "target moved after higher priority insertion"
        );
        commands.first_release.notify_one();
        old.await.expect("old partial completion");
        scheduler
            .tick()
            .await
            .expect("stable input after unrelated reload");
        assert_eq!(
            commands.calls.load(Ordering::SeqCst),
            2,
            "{mode}: unrelated reload lost an accepted command's hold"
        );
    }
}
