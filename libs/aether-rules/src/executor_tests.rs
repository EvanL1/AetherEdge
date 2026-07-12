use super::*;
use crate::MemoryRuleLiveState;
use aether_domain::TimestampMs;
use aether_ports::{CommandReceipt, PortError, PortErrorKind, PortResult};
use async_trait::async_trait;
use serde_json::json;

use crate::parser::extract_rule_flow;

/// Helper: create executor with deterministic in-process live state.
fn new_executor() -> (Arc<MemoryRuleLiveState>, RuleExecutor) {
    let live_state = Arc::new(MemoryRuleLiveState::new());
    let routing_cache = Arc::new(RoutingCache::default());
    let executor = RuleExecutor::new(Arc::clone(&live_state), routing_cache);
    (live_state, executor)
}

fn new_executor_without_action_commands() -> (Arc<MemoryRuleLiveState>, RuleExecutor) {
    let live_state = Arc::new(MemoryRuleLiveState::new());
    let routing_cache = Arc::new(RoutingCache::default());
    let executor = RuleExecutor::new(Arc::clone(&live_state), routing_cache);
    (live_state, executor)
}

struct SuccessfulActionCommands;

#[async_trait]
impl RuleActionCommandFacade for SuccessfulActionCommands {
    async fn write_action(&self, command: RuleActionCommand) -> PortResult<CommandReceipt> {
        Ok(CommandReceipt::new(
            aether_domain::CommandId::new(1),
            TimestampMs::new(command.target().point_id().get().into()),
        ))
    }
}

struct FailingActionCommands;

#[async_trait]
impl RuleActionCommandFacade for FailingActionCommands {
    async fn write_action(&self, _command: RuleActionCommand) -> PortResult<CommandReceipt> {
        Err(PortError::new(
            PortErrorKind::Unavailable,
            "simulated governed action failure",
        ))
    }
}

/// Helper: setup SOC strategy test with given battery value
async fn setup_soc_test(soc_value: &'static str) -> (Arc<MemoryRuleLiveState>, RuleExecutor, Rule) {
    let live_state = Arc::new(MemoryRuleLiveState::new());
    let soc_value = soc_value.parse::<f64>().unwrap();
    assert!(live_state.set_instance(5, 0, 3, soc_value, 1));
    let routing_cache = Arc::new(RoutingCache::default());
    let executor = RuleExecutor::new(Arc::clone(&live_state), routing_cache)
        .with_action_command_facade(Arc::new(SuccessfulActionCommands));
    let rule = create_soc_rule();
    (live_state, executor, rule)
}

/// Helper: Build simplified SOC strategy flow JSON
///
/// Logic:
/// - X1 <= 5 (low battery) → out001 → changeValue1 (pv_01:A:5=999)
/// - X1 >= 49 (medium)     → out002 → changeValue2 (diesel_gen_01:A:2=1)
/// - X1 >= 99 (high)       → out003 → changeValue3 (pv_01:A:5=78)
fn soc_strategy_json() -> serde_json::Value {
    json!({
        "nodes": [
            { "id": "start", "type": "start", "data": { "config": { "wires": { "default": ["switch1"] } } } },
            {
                "id": "switch1", "type": "custom",
                "data": {
                    "type": "function-switch",
                    "config": {
                        "variables": [{ "name": "X1", "type": "single", "instance": 5, "pointType": "measurement", "point": 3 }],
                        "rule": [
                            { "name": "out001", "type": "default", "rule": [{ "type": "variable", "variables": "X1", "operator": "<=", "value": 5 }] },
                            { "name": "out002", "type": "default", "rule": [{ "type": "variable", "variables": "X1", "operator": ">=", "value": 49 }] },
                            { "name": "out003", "type": "default", "rule": [{ "type": "variable", "variables": "X1", "operator": ">=", "value": 99 }] }
                        ],
                        "wires": { "out001": ["changeValue1"], "out002": ["changeValue2"], "out003": ["changeValue3"] }
                    }
                }
            },
            { "id": "changeValue1", "type": "custom", "data": { "type": "action-changeValue", "config": {
                "variables": [{ "name": "Y1", "type": "single", "instance": 6, "pointType": "action", "point": 5 }],
                "rule": [{ "Variables": "Y1", "value": 999 }], "wires": { "default": ["end"] }
            }}},
            { "id": "changeValue2", "type": "custom", "data": { "type": "action-changeValue", "config": {
                "variables": [{ "name": "Y2", "type": "single", "instance": 7, "pointType": "action", "point": 2 }],
                "rule": [{ "Variables": "Y2", "value": 1 }], "wires": { "default": ["end"] }
            }}},
            { "id": "changeValue3", "type": "custom", "data": { "type": "action-changeValue", "config": {
                "variables": [{ "name": "Y3", "type": "single", "instance": 6, "pointType": "action", "point": 5 }],
                "rule": [{ "Variables": "Y3", "value": 78 }], "wires": { "default": ["end"] }
            }}},
            { "id": "end", "type": "end" }
        ]
    })
}

fn create_soc_rule() -> Rule {
    let flow_json = soc_strategy_json();
    let rule_flow = extract_rule_flow(&flow_json).unwrap();
    Rule {
        id: 1,
        name: "SOC Strategy".to_string(),
        description: None,
        enabled: true,
        priority: 0,
        cooldown_ms: 0,
        trigger_config: None,
        flow: rule_flow,
    }
}

// =========================================================================
// Condition Evaluation Tests
// =========================================================================

#[tokio::test]
async fn test_evaluate_flow_condition() {
    let (_live_state, executor) = new_executor();

    let mut values = HashMap::new();
    values.insert("X1".to_string(), 100.0);
    values.insert("X2".to_string(), 50.0);

    // X1 > X2 (100 > 50 = true)
    let condition = FlowCondition {
        cond_type: "variable".to_string(),
        variables: Some("X1".to_string()),
        operator: Some(">".to_string()),
        value: Some(json!("X2")),
    };
    assert!(executor.evaluate_flow_condition(&condition, &values));

    // X1 <= 100 (true)
    let condition2 = FlowCondition {
        cond_type: "variable".to_string(),
        variables: Some("X1".to_string()),
        operator: Some("<=".to_string()),
        value: Some(json!(100)),
    };
    assert!(executor.evaluate_flow_condition(&condition2, &values));

    // X2 >= 60 (50 >= 60 = false)
    let condition3 = FlowCondition {
        cond_type: "variable".to_string(),
        variables: Some("X2".to_string()),
        operator: Some(">=".to_string()),
        value: Some(json!(60)),
    };
    assert!(!executor.evaluate_flow_condition(&condition3, &values));
}

#[tokio::test]
async fn test_evaluate_flow_conditions_with_logic() {
    let (_live_state, executor) = new_executor();

    let mut values = HashMap::new();
    values.insert("X1".to_string(), 100.0);
    values.insert("X2".to_string(), 50.0);

    // X1 == 100 && X2 < 60 (true AND true = true)
    let conditions = vec![
        FlowCondition {
            cond_type: "variable".to_string(),
            variables: Some("X1".to_string()),
            operator: Some("==".to_string()),
            value: Some(json!(100)),
        },
        FlowCondition {
            cond_type: "relation".to_string(),
            variables: None,
            operator: None,
            value: Some(json!("&&")),
        },
        FlowCondition {
            cond_type: "variable".to_string(),
            variables: Some("X2".to_string()),
            operator: Some("<".to_string()),
            value: Some(json!(60)),
        },
    ];
    assert!(executor.evaluate_flow_conditions(&conditions, &values));

    // X1 > 200 || X2 == 50 (false OR true = true)
    let conditions2 = vec![
        FlowCondition {
            cond_type: "variable".to_string(),
            variables: Some("X1".to_string()),
            operator: Some(">".to_string()),
            value: Some(json!(200)),
        },
        FlowCondition {
            cond_type: "relation".to_string(),
            variables: None,
            operator: None,
            value: Some(json!("||")),
        },
        FlowCondition {
            cond_type: "variable".to_string(),
            variables: Some("X2".to_string()),
            operator: Some("==".to_string()),
            value: Some(json!(50)),
        },
    ];
    assert!(executor.evaluate_flow_conditions(&conditions2, &values));
}

#[tokio::test]
async fn test_evaluate_rule_switch() {
    let (_live_state, executor) = new_executor();

    let mut values = HashMap::new();
    values.insert("X1".to_string(), 10.0);

    let rules = vec![
        RuleSwitchBranch {
            name: "out001".to_string(),
            rule_type: "default".to_string(),
            rule: vec![FlowCondition {
                cond_type: "variable".to_string(),
                variables: Some("X1".to_string()),
                operator: Some("<=".to_string()),
                value: Some(json!(5)),
            }],
        },
        RuleSwitchBranch {
            name: "out002".to_string(),
            rule_type: "default".to_string(),
            rule: vec![FlowCondition {
                cond_type: "variable".to_string(),
                variables: Some("X1".to_string()),
                operator: Some(">".to_string()),
                value: Some(json!(5)),
            }],
        },
    ];

    let mut wires = HashMap::new();
    wires.insert("out001".to_string(), vec!["node-low".to_string()]);
    wires.insert("out002".to_string(), vec!["node-high".to_string()]);

    // X1=10 > 5, should match out002
    let (next, port, condition) =
        executor.evaluate_rule_switch_with_details(&rules, &wires, &values);
    assert_eq!(next, Some("node-high"));
    assert_eq!(port, Some("out002".to_string()));
    assert_eq!(condition, Some("X1>5".to_string()));
}

// =========================================================================
// SOC Strategy Tests (using setup_soc_test helper)
// =========================================================================

#[tokio::test]
async fn test_soc_strategy_low_battery() {
    // SOC = 3.5 → should match out001 (X1 <= 5)
    let (_live_state, executor, rule) = setup_soc_test("3.5").await;
    let result = executor.execute(&rule).await.unwrap();

    assert!(result.success, "Execution should succeed");
    assert!(
        result.execution_path.contains(&"changeValue1".to_string()),
        "Should execute changeValue1 for low battery. Path: {:?}",
        result.execution_path
    );
    assert_eq!(result.actions_executed.len(), 1);
    assert_eq!(result.actions_executed[0].value, 999.0);
}

#[tokio::test]
async fn failed_attempted_action_makes_rule_execution_fail() {
    let live_state = Arc::new(MemoryRuleLiveState::new());
    assert!(live_state.set_instance(5, 0, 3, 3.5, 1));
    let executor = RuleExecutor::new(Arc::clone(&live_state), Arc::new(RoutingCache::default()))
        .with_action_command_facade(Arc::new(FailingActionCommands));

    let result = executor
        .execute(&create_soc_rule())
        .await
        .unwrap_or_else(|error| panic!("execute test rule: {error}"));

    assert_eq!(result.actions_executed.len(), 1);
    assert!(!result.actions_executed[0].success);
    assert!(
        !result.success,
        "failed action must fail the rule execution"
    );
    assert_eq!(
        result.error.as_deref(),
        Some("1 of 1 attempted rule actions failed")
    );
}

#[tokio::test]
async fn no_action_rule_still_completes_successfully() {
    let (_live_state, executor) = new_executor();
    let rule = Rule {
        id: 99,
        name: "no-action".to_string(),
        description: None,
        enabled: true,
        priority: 0,
        cooldown_ms: 0,
        trigger_config: None,
        flow: crate::types::RuleFlow {
            start_node: "start".to_string(),
            nodes: HashMap::from([
                (
                    "start".to_string(),
                    RuleNode::Start {
                        wires: RuleWires {
                            default: vec!["end".to_string()],
                        },
                    },
                ),
                ("end".to_string(), RuleNode::End),
            ]),
        },
    };

    let result = executor
        .execute(&rule)
        .await
        .unwrap_or_else(|error| panic!("execute test rule: {error}"));

    assert!(result.success);
    assert!(result.actions_executed.is_empty());
    assert!(result.error.is_none());
}

#[tokio::test]
async fn test_soc_strategy_boundary_5() {
    // SOC = 5.0 → should match out001 (X1 <= 5)
    let (_live_state, executor, rule) = setup_soc_test("5.0").await;
    let result = executor.execute(&rule).await.unwrap();

    assert!(result.success);
    assert!(result.execution_path.contains(&"changeValue1".to_string()));
}

#[tokio::test]
async fn test_soc_strategy_medium_battery() {
    // SOC = 50.0 → should match out002 (X1 >= 49)
    let (_live_state, executor, rule) = setup_soc_test("50.0").await;
    let result = executor.execute(&rule).await.unwrap();

    assert!(result.success);
    assert!(
        result.execution_path.contains(&"changeValue2".to_string()),
        "Should execute changeValue2 for medium battery. Path: {:?}",
        result.execution_path
    );
    assert_eq!(result.actions_executed.len(), 1);
    assert_eq!(result.actions_executed[0].value, 1.0);
}

#[tokio::test]
async fn test_soc_strategy_high_battery() {
    // SOC = 99.5 → out002 (>=49) matches before out003 (>=99) due to condition order
    let (_live_state, executor, rule) = setup_soc_test("99.5").await;
    let result = executor.execute(&rule).await.unwrap();

    assert!(result.success);
    assert!(
        result.execution_path.contains(&"changeValue2".to_string()),
        "Due to condition order, out002 matches first. Path: {:?}",
        result.execution_path
    );
}

#[tokio::test]
async fn test_soc_strategy_no_match() {
    // SOC = 25.0 → no match (5 < 25 < 49)
    let (_live_state, executor, rule) = setup_soc_test("25.0").await;
    let result = executor.execute(&rule).await.unwrap();

    assert!(!result.success);
    assert!(result.error.is_some());
    assert!(result.error.unwrap().contains("No matching switch rule"));
}

// =========================================================================
// Variable Reading Tests
// =========================================================================

#[tokio::test]
async fn test_read_rule_variables_with_name_index() {
    let (live_state, executor) = new_executor();
    assert!(live_state.set_instance(100, 0, 1, 42.5, 1));

    let variables = vec![RuleVariable {
        name: "X1".to_string(),
        instance: Some(100),
        point_type: Some("measurement".to_string()),
        point: Some(1),
        formula: vec![],
    }];

    let mut values = HashMap::new();
    let outcome = executor
        .read_rule_variables(&variables, &mut values)
        .await
        .unwrap();
    assert!(outcome.missing.is_empty(), "no variables should be missing");

    assert_eq!(values.get("X1"), Some(&42.5));
}

#[tokio::test]
async fn test_read_variables_uses_live_state() {
    let (live_state, executor) = new_executor();
    assert!(live_state.set_instance(10, 0, 1, 55.5, 1));

    let variables = vec![RuleVariable {
        name: "DIRECT".to_string(),
        instance: Some(10),
        point_type: Some("measurement".to_string()),
        point: Some(1),
        formula: vec![],
    }];

    let mut values = HashMap::new();
    let outcome = executor
        .read_rule_variables(&variables, &mut values)
        .await
        .unwrap();
    assert!(outcome.missing.is_empty(), "no variables should be missing");

    assert_eq!(values.get("DIRECT"), Some(&55.5));
}

/// Locking the contract: measurement variables that have never been written
/// (SHM slot absent) MUST appear in `outcome.missing` so callers can skip
/// the cycle. Substituting 0.0 would silently fire conditions like
/// "current < 5A" on devices that never reported.
#[tokio::test]
async fn test_missing_measurement_appears_in_outcome_missing() {
    let (_live_state, executor) = new_executor();

    let variables = vec![RuleVariable {
        name: "GHOST".to_string(),
        instance: Some(999),
        point_type: Some("measurement".to_string()),
        point: Some(1),
        formula: vec![],
    }];

    let mut values = HashMap::new();
    let outcome = executor
        .read_rule_variables(&variables, &mut values)
        .await
        .unwrap();

    assert_eq!(outcome.missing, vec!["GHOST".to_string()]);
    assert!(
        !values.contains_key("GHOST"),
        "missing measurement must NOT be inserted with a 0.0 fallback — \
         that would silently trigger threshold conditions"
    );
}

#[tokio::test]
async fn test_non_finite_measurement_appears_in_outcome_missing() {
    let (live_state, executor) = new_executor();
    assert!(live_state.set_instance(5, 0, 3, f64::NAN, 1));

    let variables = vec![RuleVariable {
        name: "BAD".to_string(),
        instance: Some(5),
        point_type: Some("measurement".to_string()),
        point: Some(3),
        formula: vec![],
    }];

    let mut values = HashMap::new();
    let outcome = executor
        .read_rule_variables(&variables, &mut values)
        .await
        .unwrap();

    assert_eq!(outcome.missing, vec!["BAD".to_string()]);
    assert!(
        !values.contains_key("BAD"),
        "NaN/Inf measurements must be treated as missing, not valid rule input"
    );
}

/// Action variables (write targets) that are not yet written are NOT missing —
/// "never written" is the normal initial state for an action point. Caller
/// proceeds to evaluate/write. Locks the asymmetric semantics with measurement.
#[tokio::test]
async fn test_unset_action_var_is_not_treated_as_missing() {
    let (_live_state, executor) = new_executor();

    let variables = vec![RuleVariable {
        name: "Y_TARGET".to_string(),
        instance: Some(999),
        point_type: Some("action".to_string()),
        point: Some(1),
        formula: vec![],
    }];

    let mut values = HashMap::new();
    let outcome = executor
        .read_rule_variables(&variables, &mut values)
        .await
        .unwrap();

    assert!(
        outcome.missing.is_empty(),
        "unwritten action point is NOT a missing-variable error"
    );
    assert!(
        !values.contains_key("Y_TARGET"),
        "still don't fabricate a 0.0 — values stays empty until something writes"
    );
}

/// Locks the contract: assignment.value referencing an unknown variable name
/// must NOT silently write 0 — it produces ActionResult { success: false }
/// with NaN value, so the rule's failure is observable in logs/UI.
#[tokio::test]
async fn test_assignment_unknown_variable_does_not_write_zero() {
    let (_live_state, executor) = new_executor();
    // Action target exists but the variable referenced by the assignment
    // ("PHANTOM") was never read into `values`.
    let target = RuleVariable {
        name: "Y".to_string(),
        instance: Some(42),
        point_type: Some("action".to_string()),
        point: Some(7),
        formula: vec![],
    };
    let assignment = RuleValueAssignment {
        variables: "Y".to_string(),
        value: json!("PHANTOM"),
    };
    let values = HashMap::new(); // PHANTOM not present, no numeric literal

    let result = executor
        .execute_rule_change(&target, &assignment, &values)
        .await;
    assert!(!result.success, "missing-var assignment must not succeed");
    assert!(
        result.value.is_nan(),
        "skipped action carries NaN, not 0.0 — caller can distinguish"
    );

    // The rejected assignment never reaches the governed command facade.
}

/// Numeric literals encoded as strings (some frontends do this) still resolve.
#[tokio::test]
async fn test_assignment_numeric_string_literal_resolves() {
    let (_live_state, executor) = new_executor();
    let target = RuleVariable {
        name: "Y".to_string(),
        instance: Some(1),
        point_type: Some("action".to_string()),
        point: Some(1),
        formula: vec![],
    };
    let assignment = RuleValueAssignment {
        variables: "Y".to_string(),
        value: json!("3.125"),
    };
    let values = HashMap::new();

    let result = executor
        .execute_rule_change(&target, &assignment, &values)
        .await;
    assert_eq!(
        result.value, 3.125,
        "numeric-literal string must parse, not be looked up as variable"
    );
}

#[tokio::test]
async fn test_action_without_governed_command_facade_fails_closed() {
    let (_live_state, executor) = new_executor_without_action_commands();
    let target = RuleVariable {
        name: "Y".to_string(),
        instance: Some(42),
        point_type: Some("action".to_string()),
        point: Some(7),
        formula: vec![],
    };
    let assignment = RuleValueAssignment {
        variables: "Y".to_string(),
        value: json!(12.5),
    };
    let values = HashMap::new();

    let result = executor
        .execute_rule_change(&target, &assignment, &values)
        .await;

    assert!(
        !result.success,
        "rule action must fail when the governed command facade is unavailable"
    );
}

// =========================================================================
// Token Formula Tests (infix format from frontend)
// =========================================================================

#[test]
fn test_formula_simple_addition() {
    // Frontend sends: ["X1", "+", "X2"]
    let formula = vec![json!("X1"), json!("+"), json!("X2")];
    let mut values = HashMap::new();
    values.insert("X1".to_string(), 10.0);
    values.insert("X2".to_string(), 20.0);
    assert_eq!(evaluate_token_formula(&formula, &values), Some(30.0));
}

#[test]
fn test_formula_precedence() {
    // X1 + X2 * 2 = 10 + 20*2 = 50
    let formula = vec![json!("X1"), json!("+"), json!("X2"), json!("*"), json!(2)];
    let mut values = HashMap::new();
    values.insert("X1".to_string(), 10.0);
    values.insert("X2".to_string(), 20.0);
    assert_eq!(evaluate_token_formula(&formula, &values), Some(50.0));
}

#[test]
fn test_formula_all_operators() {
    // a + b - c * d / e = 10 + 5 - 6*4/2 = 15 - 12 = 3
    let formula = vec![
        json!("a"),
        json!("+"),
        json!("b"),
        json!("-"),
        json!("c"),
        json!("*"),
        json!("d"),
        json!("/"),
        json!("e"),
    ];
    let mut values = HashMap::new();
    for (k, v) in [("a", 10.0), ("b", 5.0), ("c", 6.0), ("d", 4.0), ("e", 2.0)] {
        values.insert(k.to_string(), v);
    }
    assert_eq!(evaluate_token_formula(&formula, &values), Some(3.0));
}

#[test]
fn test_formula_division_by_zero() {
    let formula = vec![json!("X1"), json!("/"), json!(0)];
    let mut values = HashMap::new();
    values.insert("X1".to_string(), 10.0);
    // evalexpr returns infinity for division by zero
    let result = evaluate_token_formula(&formula, &values);
    assert!(result.is_some());
    assert!(result.unwrap().is_infinite());
}

#[test]
fn test_formula_undefined_variable() {
    let formula = vec![json!("X1"), json!("+"), json!("UNDEFINED")];
    let mut values = HashMap::new();
    values.insert("X1".to_string(), 10.0);
    assert_eq!(evaluate_token_formula(&formula, &values), None);
}

#[test]
fn test_formula_numeric_literals() {
    // 5 + 3 * 2 = 11
    let formula = vec![json!(5), json!("+"), json!(3), json!("*"), json!(2)];
    assert_eq!(
        evaluate_token_formula(&formula, &HashMap::new()),
        Some(11.0)
    );
}

#[test]
fn test_formula_float_precision() {
    let formula = vec![json!(1.5), json!("+"), json!(2.5)];
    assert_eq!(evaluate_token_formula(&formula, &HashMap::new()), Some(4.0));
}

#[test]
fn test_formula_single_variable() {
    let formula = vec![json!("X1")];
    let mut values = HashMap::new();
    values.insert("X1".to_string(), 42.0);
    assert_eq!(evaluate_token_formula(&formula, &values), Some(42.0));
}
