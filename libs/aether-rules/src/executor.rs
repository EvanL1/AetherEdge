//! Rule Executor - Execute Vue Flow rules with RuleFlow
//!
//! Executes rule flow by:
//! 1. Traversing nodes from start to end
//! 2. For each node: reading node-local variables, evaluating conditions
//! 3. Executing actions and following wires

use crate::error::Result;
use crate::live_state::RuleLiveState;
use crate::logger::format_conditions;
use crate::types::{
    CalculationRule, FlowCondition, Rule, RuleNode, RuleSwitchBranch, RuleValueAssignment,
    RuleVariable, RuleWires,
};
use crate::{RuleActionCommand, RuleActionCommandFacade};
use aether_calc::{CalcEngine, MemoryStateStore, StateStore};
use aether_domain::{InstanceId, PointId};
use aether_model::{ValidationConfig, validate_value};
use aether_routing::RoutingCache;
use serde::Serialize;
use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::Arc;

/// Convert dynamic point type string to static str for zero-allocation ActionResult
#[inline]
fn point_type_to_static(pt: Option<&str>, default: &'static str) -> &'static str {
    match pt {
        Some("M") | Some("measurement") => "M",
        Some("A") | Some("action") => "A",
        Some("T") | Some("telemetry") => "T",
        Some("S") | Some("status") => "S",
        Some("C") | Some("control") => "C",
        _ => default,
    }
}

/// Validate variable fields and sanitize value for write operations.
///
/// Common validation shared by `execute_rule_change`, `write_calculation_result`,
/// and `write_period_delta_result`. Returns `(instance_id, point_id, sanitized_value, point_type)`
/// or a failed `ActionResult` if validation fails.
fn validate_write_target(
    variable: &RuleVariable,
    raw_value: f64,
    default_point_type: &'static str,
    context: &str,
) -> std::result::Result<(u32, u32, f64, &'static str), ActionResult> {
    let config = ValidationConfig::default();
    let pt = point_type_to_static(variable.point_type.as_deref(), default_point_type);

    // Reject NaN/Inf/out-of-range — never silently coerce to 0.0. The old
    // sanitize_value path turned a malformed compute result into a real
    // SCADA write of 0, which is the exact failure mode this skill is
    // sealing across the codebase. Caller treats Err as "skip this action".
    let value = match validate_value(raw_value, &config) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(
                "{} skipped: value {} failed validation ({}) (variable '{}')",
                context,
                raw_value,
                e,
                variable.name
            );
            return Err(ActionResult {
                target_type: "instance",
                target_id: variable.instance.unwrap_or(0),
                point_type: pt,
                point_id: variable.point.unwrap_or(0),
                value: f64::NAN,
                success: false,
            });
        },
    };

    let instance_id = variable.instance.ok_or_else(|| {
        tracing::error!(
            "{} skipped: variable '{}' missing instance_id",
            context,
            variable.name
        );
        ActionResult {
            target_type: "instance",
            target_id: 0,
            point_type: pt,
            point_id: 0,
            value,
            success: false,
        }
    })?;

    let point = variable.point.ok_or_else(|| {
        tracing::error!(
            "{} skipped: variable '{}' missing point_id (instance_id={})",
            context,
            variable.name,
            instance_id
        );
        ActionResult {
            target_type: "instance",
            target_id: instance_id,
            point_type: pt,
            point_id: 0,
            value,
            success: false,
        }
    })?;

    Ok((instance_id, point, value, pt))
}

/// Create or reuse a cached snapshot of variable values.
///
/// Returns the cached `Arc` if `values_changed` is false, otherwise creates a new one.
fn snapshot_or_reuse(
    cache: &mut Option<Arc<HashMap<String, f64>>>,
    values: &HashMap<String, f64>,
    values_changed: bool,
) -> Arc<HashMap<String, f64>> {
    if !values_changed && let Some(snapshot) = cache.as_ref() {
        return Arc::clone(snapshot);
    }
    let snapshot = Arc::new(values.clone());
    *cache = Some(Arc::clone(&snapshot));
    snapshot
}

/// Evaluate a formula in Reverse Polish Notation (RPN)
/// Convert a JSON token array `["X1", "+", "X2"]` to an expression string `"X1 + X2"`,
/// then evaluate via the shared `formula::evaluate_formula` engine (evalexpr).
fn evaluate_token_formula(
    tokens: &[serde_json::Value],
    values: &HashMap<String, f64>,
) -> Option<f64> {
    // Build expression string from tokens
    let expr: String = tokens
        .iter()
        .map(|t| match t {
            serde_json::Value::String(s) => Cow::Borrowed(s.as_str()),
            serde_json::Value::Number(n) => Cow::Owned(n.to_string()),
            other => Cow::Owned(other.to_string()),
        })
        .collect::<Vec<_>>()
        .join(" ");

    match crate::formula::evaluate_formula(&expr, values) {
        Ok(val) => Some(val),
        Err(e) => {
            tracing::warn!("Formula '{}' evaluation failed: {}", expr, e);
            None
        },
    }
}

/// Build an ActionResult marking that a rule action was skipped because the
/// resolved value couldn't be determined (missing variable, unsupported type).
/// Carries the variable's identity so caller and logs can attribute the skip.
fn action_skipped(variable: &RuleVariable, reason: &str) -> ActionResult {
    tracing::warn!(
        "Rule action skipped (variable '{}', instance={:?}, point={:?}): {}",
        variable.name,
        variable.instance,
        variable.point,
        reason
    );
    ActionResult {
        target_type: "instance",
        target_id: variable.instance.unwrap_or(0),
        point_type: point_type_to_static(variable.point_type.as_deref(), "A"),
        point_id: variable.point.unwrap_or(0),
        value: f64::NAN,
        success: false,
    }
}

/// Outcome of one variable-read pass for a node.
///
/// Captures both "did anything change?" (for snapshot-cache reuse) and
/// "which variables were unavailable?" (so callers can skip evaluation
/// instead of substituting 0.0 for absent readings — silently triggering
/// conditions like "current < threshold" on missing data).
#[derive(Debug, Default)]
pub(crate) struct RuleReadOutcome {
    pub values_changed: bool,
    /// Variables whose data was unavailable from SHM this cycle. Caller MUST
    /// short-circuit rule evaluation when
    /// non-empty.
    pub missing: Vec<String>,
    /// Per-point values read this pass, keyed by stable cache_key format
    /// ("M:{instance}:{point}" / "A:{instance}:{point}"). Scheduler uses
    /// this to advance OnChange last_value against the values executor
    /// actually saw.
    pub point_values: HashMap<String, f64>,
}

/// Result of executing a rule
#[derive(Debug, Clone, Serialize)]
pub struct RuleExecutionResult {
    pub rule_id: i64,
    pub success: bool,
    pub actions_executed: Vec<ActionResult>,
    pub error: Option<String>,
    pub execution_path: Vec<String>, // Node IDs visited
    /// Matched condition expression (e.g., "X1>=49" or "X1>10 && X2<50")
    pub matched_condition: Option<String>,
    /// Variable values at execution time (for logging)
    /// Arc-shared to avoid cloning the full HashMap on each node
    pub variable_values: Arc<HashMap<String, f64>>,
    /// Per-point values actually read during execution, keyed by stable
    /// point identifier ("M:{instance}:{point}" / "A:{instance}:{point}").
    /// Scheduler uses this to advance OnChange `last_value` against the
    /// values the executor really saw from the current SHM generation.
    pub point_values: Arc<HashMap<String, f64>>,
    /// Node execution details for debugging/visualization
    pub node_details: HashMap<String, NodeExecutionDetail>,
}

/// Record of an executed action
///
/// All fields are Copy types, making this struct zero-cost to clone.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct ActionResult {
    /// Target type: "instance" or "channel"
    pub target_type: &'static str,
    /// Target ID (instance_id or channel_id)
    pub target_id: u32,
    /// Point type (M/A for instance, T/S/C/A for channel)
    pub point_type: &'static str,
    /// Point ID
    pub point_id: u32,
    /// Value written (f64 for zero-allocation)
    pub value: f64,
    /// Whether the action succeeded
    pub success: bool,
}

/// Execution details for a single node (for debugging/visualization)
#[derive(Debug, Clone, Serialize)]
pub struct NodeExecutionDetail {
    /// Node type: "start", "switch", "change", "end", "calculation"
    pub node_type: &'static str,
    /// Variable values when entering this node (Arc-shared snapshot)
    pub input_values: Arc<HashMap<String, f64>>,
    /// Condition evaluation results (for Switch nodes)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub condition_results: Option<Vec<ConditionResult>>,
    /// The matched output port (for Switch nodes)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub matched_port: Option<String>,
    /// Actions executed (for ChangeValue nodes)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub actions: Option<Vec<ActionResult>>,
}

/// Result of evaluating a single condition branch
#[derive(Debug, Clone, Serialize)]
pub struct ConditionResult {
    /// The condition expression (e.g., "X1>=49")
    pub expression: String,
    /// Whether this condition evaluated to true
    pub result: bool,
    /// The output port name for this condition
    pub port: String,
}

/// Rule executor
///
/// Uses the authoritative SHM live-state view. Missing SHM data is missing;
/// there is no network-store fallback.
pub struct RuleExecutor<S: StateStore = MemoryStateStore> {
    live_state: Arc<dyn RuleLiveState>,
    /// State store for stateful calculation functions (integrate, moving_avg, etc.)
    state_store: Arc<S>,
    /// Governed logical-action facade installed by the composition root.
    action_commands: Option<Arc<dyn RuleActionCommandFacade>>,
}

impl RuleExecutor<MemoryStateStore> {
    /// Create with default MemoryStateStore
    pub fn new<L>(live_state: Arc<L>, _routing_cache: Arc<RoutingCache>) -> Self
    where
        L: RuleLiveState + 'static,
    {
        Self {
            live_state,
            state_store: Arc::new(MemoryStateStore::new()),
            action_commands: None,
        }
    }
}

impl<S: StateStore> RuleExecutor<S> {
    /// Create with custom state store
    pub fn with_state_store<L>(
        live_state: Arc<L>,
        _routing_cache: Arc<RoutingCache>,
        state_store: Arc<S>,
    ) -> Self
    where
        L: RuleLiveState + 'static,
    {
        Self {
            live_state,
            state_store,
            action_commands: None,
        }
    }

    /// Installs the host-owned, audited device-action command facade.
    #[must_use]
    pub fn with_action_command_facade(
        mut self,
        commands: Arc<dyn RuleActionCommandFacade>,
    ) -> Self {
        self.action_commands = Some(commands);
        self
    }

    /// Execute a rule with RuleFlow
    pub async fn execute(&self, rule: &Rule) -> Result<RuleExecutionResult> {
        let mut result = RuleExecutionResult {
            rule_id: rule.id,
            success: false,
            actions_executed: vec![],
            error: None,
            execution_path: vec![],
            matched_condition: None,
            variable_values: Arc::new(HashMap::new()),
            point_values: Arc::new(HashMap::new()),
            node_details: HashMap::new(),
        };

        // Execute from start node, accumulating variable values along the path
        let mut values: HashMap<String, f64> = HashMap::new();
        // Mirror of `values` keyed by point identifier (M:{inst}:{point} or
        // A:{inst}:{point}) so scheduler can advance OnChange last_value
        // against what the executor actually saw from SHM.
        let mut point_values: HashMap<String, f64> = HashMap::new();
        let mut current_id = rule.flow.start_node.as_str();
        let max_iterations = 100; // Prevent infinite loops
        let mut iterations = 0;

        let mut values_snapshot: Option<Arc<HashMap<String, f64>>> = None;

        loop {
            iterations += 1;
            if iterations > max_iterations {
                result.error = Some("Execution exceeded maximum iterations".to_string());
                return Ok(result);
            }

            result.execution_path.push(current_id.to_string());

            let node = match rule.flow.nodes.get(current_id) {
                Some(n) => n,
                None => {
                    result.error = Some(format!("Node not found: {}", current_id));
                    return Ok(result);
                },
            };

            match node {
                RuleNode::End => {
                    // Save final variable values and aggregate all attempted
                    // action outcomes. Reaching End means traversal completed,
                    // but it must not erase a failed device command.
                    // point_values is the executor's authoritative "what we
                    // actually read" view, surfaced to scheduler for OnChange
                    // last_value advancement so deadband matches reality.
                    result.variable_values = Arc::new(std::mem::take(&mut values));
                    result.point_values = Arc::new(std::mem::take(&mut point_values));
                    let failed_actions = result
                        .actions_executed
                        .iter()
                        .filter(|action| !action.success)
                        .count();
                    if failed_actions == 0 {
                        result.success = true;
                    } else {
                        result.success = false;
                        result.error = Some(format!(
                            "{} of {} attempted rule actions failed",
                            failed_actions,
                            result.actions_executed.len()
                        ));
                    }
                    break;
                },
                RuleNode::Start { wires } => {
                    current_id = match wires.default.first() {
                        Some(next) => next.as_str(),
                        None => {
                            result.error = Some("Start node has no output wire".to_string());
                            return Ok(result);
                        },
                    };
                },
                RuleNode::Switch {
                    variables,
                    rule: rules,
                    wires,
                } => {
                    // Read node-local variables. Missing variables → skip this
                    // cycle so absent data can never trigger threshold rules
                    // (a 0.0 fallback would fire "X1 < 5" on missing readings).
                    let outcome = match self.read_rule_variables(variables, &mut values).await {
                        Ok(o) => o,
                        Err(e) => {
                            result.error = Some(format!("Failed to read variables: {}", e));
                            result.variable_values = Arc::new(std::mem::take(&mut values));
                            result.point_values = Arc::new(std::mem::take(&mut point_values));
                            return Ok(result);
                        },
                    };
                    point_values.extend(outcome.point_values);
                    if !outcome.missing.is_empty() {
                        result.error = Some(format!(
                            "Rule cycle skipped: variables unavailable: {}",
                            outcome.missing.join(", ")
                        ));
                        result.variable_values = Arc::new(std::mem::take(&mut values));
                        result.point_values = Arc::new(std::mem::take(&mut point_values));
                        return Ok(result);
                    }
                    let values_changed = outcome.values_changed;

                    // Snapshot values when entering this node (reuse cache if nothing changed)
                    let snapshot = snapshot_or_reuse(&mut values_snapshot, &values, values_changed);
                    result.variable_values = Arc::clone(&snapshot);

                    // Evaluate all conditions for debugging/visualization
                    let condition_results = self.evaluate_all_conditions(rules, &values);

                    // Evaluate switch rules to determine next node and capture matched condition
                    let (next_node, matched_port, matched_cond) =
                        self.evaluate_rule_switch_with_details(rules, wires, &values);
                    result.matched_condition = matched_cond;

                    // Record node execution detail (reuse Arc snapshot)
                    result.node_details.insert(
                        current_id.to_string(),
                        NodeExecutionDetail {
                            node_type: "switch",
                            input_values: snapshot,
                            condition_results: Some(condition_results),
                            matched_port,
                            actions: None,
                        },
                    );

                    match next_node {
                        Some(next) => current_id = next,
                        None => {
                            result.error = Some("No matching switch rule".to_string());
                            return Ok(result);
                        },
                    }
                },
                RuleNode::ChangeValue {
                    variables,
                    rule: assignments,
                    wires,
                } => {
                    // Read target variables. Skip the cycle on missing data —
                    // a 0.0 fallback would write meaningless action values.
                    let outcome = match self.read_rule_variables(variables, &mut values).await {
                        Ok(o) => o,
                        Err(e) => {
                            result.error = Some(format!("Failed to read variables: {}", e));
                            return Ok(result);
                        },
                    };
                    point_values.extend(outcome.point_values);
                    if !outcome.missing.is_empty() {
                        result.error = Some(format!(
                            "Rule cycle skipped: variables unavailable: {}",
                            outcome.missing.join(", ")
                        ));
                        return Ok(result);
                    }
                    let values_changed = outcome.values_changed;

                    // Snapshot values when entering this node (before executing actions)
                    let input_snapshot =
                        snapshot_or_reuse(&mut values_snapshot, &values, values_changed);
                    result.variable_values = Arc::clone(&input_snapshot);

                    // Execute value assignments and collect actions for this node
                    let mut node_actions = Vec::new();
                    for assignment in assignments {
                        let variable = variables.iter().find(|v| v.name == assignment.variables);
                        if let Some(var) = variable {
                            let executed = self.execute_rule_change(var, assignment, &values).await;
                            node_actions.push(executed);
                            result.actions_executed.push(executed);
                        }
                    }

                    // Record node execution detail
                    result.node_details.insert(
                        current_id.to_string(),
                        NodeExecutionDetail {
                            node_type: "change",
                            input_values: input_snapshot,
                            condition_results: None,
                            matched_port: None,
                            actions: Some(node_actions),
                        },
                    );

                    current_id = match wires.default.first() {
                        Some(next) => next.as_str(),
                        None => {
                            result.error = Some("ChangeValue node has no output wire".to_string());
                            return Ok(result);
                        },
                    };
                },
                RuleNode::Calculation {
                    variables,
                    rule: calculations,
                    wires,
                } => match self
                    .handle_calculation_node(
                        current_id,
                        variables,
                        calculations,
                        wires,
                        &mut values,
                        &mut point_values,
                        &mut values_snapshot,
                        &mut result,
                        rule.id,
                    )
                    .await
                {
                    Some(next) => current_id = next,
                    None => return Ok(result),
                },
                RuleNode::PeriodDelta {
                    input,
                    output,
                    period,
                    wires,
                } => match self
                    .handle_period_delta_node(
                        current_id,
                        input,
                        output,
                        period,
                        wires,
                        &mut values,
                        &mut point_values,
                        &mut values_snapshot,
                        &mut result,
                        rule.id,
                    )
                    .await
                {
                    Some(next) => current_id = next,
                    None => return Ok(result),
                },
            }
        }

        Ok(result)
    }

    /// Handle Calculation node: evaluate formulas and write results
    #[allow(clippy::too_many_arguments)]
    async fn handle_calculation_node<'a>(
        &self,
        node_id: &str,
        variables: &[RuleVariable],
        calculations: &[CalculationRule],
        wires: &'a RuleWires,
        values: &mut HashMap<String, f64>,
        point_values: &mut HashMap<String, f64>,
        snapshot_cache: &mut Option<Arc<HashMap<String, f64>>>,
        result: &mut RuleExecutionResult,
        rule_id: i64,
    ) -> Option<&'a str> {
        let outcome = match self.read_rule_variables(variables, values).await {
            Ok(o) => o,
            Err(e) => {
                result.error = Some(format!("Failed to read variables: {}", e));
                return None;
            },
        };
        point_values.extend(outcome.point_values);
        if !outcome.missing.is_empty() {
            result.error = Some(format!(
                "Calculation skipped: variables unavailable: {}",
                outcome.missing.join(", ")
            ));
            return None;
        }
        let values_changed = outcome.values_changed;

        let input_snapshot = snapshot_or_reuse(snapshot_cache, values, values_changed);
        result.variable_values = Arc::clone(&input_snapshot);

        let calc_engine =
            CalcEngine::new(Arc::clone(&self.state_store), format!("rule_{}", rule_id));
        let mut node_actions = Vec::new();

        for calc in calculations {
            let calc_result = match calc_engine.evaluate(&calc.formula, values).await {
                Ok(v) => v,
                Err(e) => {
                    result.error = Some(format!("Calc '{}' error: {}", calc.formula, e));
                    return None;
                },
            };

            if let Some(var) = variables.iter().find(|v| v.name == calc.output) {
                let action = self.write_calculation_result(var, calc_result, calc).await;
                node_actions.push(action);
                result.actions_executed.push(action);
            }

            values.insert(calc.output.clone(), calc_result);
        }

        *snapshot_cache = None; // Values modified; invalidate cache
        result.node_details.insert(
            node_id.to_string(),
            NodeExecutionDetail {
                node_type: "calculation",
                input_values: input_snapshot,
                condition_results: None,
                matched_port: None,
                actions: Some(node_actions),
            },
        );

        match wires.default.first() {
            Some(next) => Some(next.as_str()),
            None => {
                result.error = Some("Calculation node has no output wire".to_string());
                None
            },
        }
    }

    /// Handle PeriodDelta node: calculate period delta and write result
    #[allow(clippy::too_many_arguments)]
    async fn handle_period_delta_node<'a>(
        &self,
        node_id: &str,
        input: &RuleVariable,
        output: &RuleVariable,
        period: &str,
        wires: &'a RuleWires,
        values: &mut HashMap<String, f64>,
        point_values: &mut HashMap<String, f64>,
        snapshot_cache: &mut Option<Arc<HashMap<String, f64>>>,
        result: &mut RuleExecutionResult,
        rule_id: i64,
    ) -> Option<&'a str> {
        let input_vars = vec![input.clone()];
        let outcome = match self.read_rule_variables(&input_vars, values).await {
            Ok(o) => o,
            Err(e) => {
                result.error = Some(format!("Failed to read input variable: {}", e));
                return None;
            },
        };
        point_values.extend(outcome.point_values);
        if !outcome.missing.is_empty() {
            result.error = Some(format!(
                "PeriodDelta skipped: input variable unavailable: {}",
                outcome.missing.join(", ")
            ));
            return None;
        }
        let values_changed = outcome.values_changed;

        let input_snapshot = snapshot_or_reuse(snapshot_cache, values, values_changed);
        result.variable_values = Arc::clone(&input_snapshot);

        let input_value = match values.get(&input.name).copied() {
            Some(v) if v.is_finite() => v,
            Some(v) => {
                result.error = Some(format!(
                    "PeriodDelta skipped: input variable '{}' is non-finite ({})",
                    input.name, v
                ));
                return None;
            },
            None => {
                result.error = Some(format!(
                    "PeriodDelta skipped: input variable '{}' unavailable after read",
                    input.name
                ));
                return None;
            },
        };
        let calc_engine =
            CalcEngine::new(Arc::clone(&self.state_store), format!("rule_{}", rule_id));

        let state_key = format!(
            "{}:{}:{}",
            rule_id,
            input.instance.unwrap_or(0),
            input.point.unwrap_or(0)
        );
        let delta = match calc_engine
            .builtin()
            .period_delta(&state_key, input_value, period)
            .await
        {
            Ok(v) => v,
            Err(e) => {
                result.error = Some(format!("period_delta error: {}", e));
                return None;
            },
        };

        let action = self.write_period_delta_result(output, delta, period).await;
        result.actions_executed.push(action);

        values.insert(output.name.clone(), delta);
        *snapshot_cache = None; // Invalidate cache

        result.node_details.insert(
            node_id.to_string(),
            NodeExecutionDetail {
                node_type: "periodDelta",
                input_values: input_snapshot,
                condition_results: None,
                matched_port: None,
                actions: Some(vec![action]),
            },
        );

        match wires.default.first() {
            Some(next) => Some(next.as_str()),
            None => {
                result.error = Some("PeriodDelta node has no output wire".to_string());
                None
            },
        }
    }

    /// Read variables from the authoritative live-state view.
    ///
    /// Production supplies a SHM-backed implementation. A missing or
    /// non-finite Measurement makes the rule ineligible for this cycle;
    /// an unwritten Action target remains optional because rules may write it.
    async fn read_rule_variables(
        &self,
        variables: &[RuleVariable],
        values: &mut HashMap<String, f64>,
    ) -> Result<RuleReadOutcome> {
        let mut outcome = RuleReadOutcome::default();

        for var in variables {
            // Skip formula variables in Phase 1 - calculated in Phase 2 after base variables
            if !var.formula.is_empty() {
                continue;
            }

            let var_name = var.name.clone();

            // Get instance ID (supports both "instance" and "instance_id" via serde alias)
            let instance_id = match var.instance {
                Some(id) => id,
                None => {
                    return Err(crate::error::RuleError::ExecutionError(format!(
                        "Variable '{}' is missing instance_id",
                        var_name
                    )));
                },
            };

            let point_type = var.point_type.as_deref().unwrap_or("measurement");
            let point = var.point.ok_or_else(|| {
                crate::error::RuleError::ExecutionError(format!(
                    "Variable '{}' is missing point_id",
                    var_name
                ))
            })?;

            let is_action = point_type == "action";
            let instance_type = u8::from(is_action);
            match self
                .live_state
                .get_instance(instance_id, instance_type, point)
            {
                Some((value, _timestamp_ms)) if value.is_finite() => {
                    let point_key = format!(
                        "{}:{}:{}",
                        if is_action { 'A' } else { 'M' },
                        instance_id,
                        point
                    );
                    outcome.point_values.insert(point_key, value);
                    outcome.values_changed |= values
                        .insert(var_name, value)
                        .is_none_or(|previous| previous.total_cmp(&value).is_ne());
                },
                Some((value, _)) if is_action => {
                    tracing::trace!(
                        "Action variable {} from SHM is non-finite ({}) — leaving unset",
                        var_name,
                        value
                    );
                },
                Some((value, _)) => {
                    tracing::warn!(
                        "Measurement variable {} from SHM is non-finite ({}) — skipping rule",
                        var_name,
                        value
                    );
                    outcome.missing.push(var_name);
                },
                None if is_action => {
                    tracing::trace!(
                        "Action variable {} has no SHM value yet — leaving unset",
                        var_name
                    );
                },
                None => {
                    tracing::warn!(
                        "Measurement variable {} is absent from SHM — skipping rule",
                        var_name
                    );
                    outcome.missing.push(var_name);
                },
            }
        }

        // ★ Phase 2: Calculate formula variables (depend on base variables)
        // Formula variables use RPN (Reverse Polish Notation) like ["X1", "X2", "+", 2, "*"]
        for var in variables {
            if var.formula.is_empty() {
                continue; // Skip non-formula variables (already handled above)
            }

            let var_name = var.name.clone();
            match evaluate_token_formula(&var.formula, values) {
                Some(result) => {
                    outcome.values_changed |= values.insert(var_name, result) != Some(result);
                },
                None => {
                    tracing::warn!(
                        "Formula variable '{}' could not be evaluated — rule will be skipped",
                        var_name
                    );
                    outcome.missing.push(var_name);
                },
            }
        }

        Ok(outcome)
    }

    /// Evaluate compact switch rules and return the next node ID with matched condition and port
    ///
    /// Returns: (next_node_id, matched_port, matched_condition_expression)
    fn evaluate_rule_switch_with_details<'a>(
        &self,
        rules: &[RuleSwitchBranch],
        wires: &'a HashMap<String, Vec<String>>,
        values: &HashMap<String, f64>,
    ) -> (Option<&'a str>, Option<String>, Option<String>) {
        for rule in rules {
            if self.evaluate_flow_conditions(&rule.rule, values) {
                // Format the matched condition expression
                let condition_str = format_conditions(&rule.rule);

                // Find the wire target for this rule's output
                if let Some(targets) = wires.get(&rule.name)
                    && let Some(target) = targets.first()
                {
                    return (
                        Some(target.as_str()),
                        Some(rule.name.clone()),
                        Some(condition_str),
                    );
                }
            }
        }
        (None, None, None)
    }

    /// Evaluate all switch conditions and return results for each branch
    ///
    /// This is used for debugging/visualization to show which conditions matched/failed
    fn evaluate_all_conditions(
        &self,
        rules: &[RuleSwitchBranch],
        values: &HashMap<String, f64>,
    ) -> Vec<ConditionResult> {
        rules
            .iter()
            .map(|rule| {
                let result = self.evaluate_flow_conditions(&rule.rule, values);
                let expression = format_conditions(&rule.rule);
                ConditionResult {
                    expression,
                    result,
                    port: rule.name.clone(),
                }
            })
            .collect()
    }

    /// Evaluate compact conditions
    fn evaluate_flow_conditions(
        &self,
        conditions: &[FlowCondition],
        values: &HashMap<String, f64>,
    ) -> bool {
        if conditions.is_empty() {
            return true;
        }

        let mut result = true;
        let mut pending_relation: Option<&str> = None;

        for cond in conditions {
            if cond.cond_type == "relation" {
                // Store relation for next condition
                pending_relation = cond.value.as_ref().and_then(|v| v.as_str());
                continue;
            }

            // Evaluate variable condition
            let cond_result = self.evaluate_flow_condition(cond, values);

            // Combine with previous result
            match pending_relation {
                Some("||") | Some("or") | Some("OR") => {
                    result = result || cond_result;
                },
                _ => {
                    // Default to AND
                    result = result && cond_result;
                },
            }
            pending_relation = None;
        }

        result
    }

    /// Evaluate a single compact condition
    fn evaluate_flow_condition(&self, cond: &FlowCondition, values: &HashMap<String, f64>) -> bool {
        let var_name = match &cond.variables {
            Some(name) => name,
            None => return false,
        };

        let operator = cond.operator.as_deref().unwrap_or("==");

        // Variable must exist in values, otherwise condition fails
        let left = match values.get(var_name) {
            Some(&v) => v,
            None => {
                tracing::warn!(
                    "Variable '{}' not found in values, condition fails",
                    var_name
                );
                return false;
            },
        };
        let right = match &cond.value {
            Some(v) => {
                if let Some(n) = v.as_f64() {
                    n
                } else if let Some(n) = v.as_i64() {
                    n as f64
                } else if let Some(s) = v.as_str() {
                    // Could be a variable reference - must exist if referenced
                    match values.get(s) {
                        Some(&v) => v,
                        None => match s.parse::<f64>() {
                            Ok(n) => n,
                            Err(_) => {
                                tracing::warn!("Variable '{}' not found and not a number", s);
                                return false;
                            },
                        },
                    }
                } else {
                    0.0
                }
            },
            None => 0.0,
        };

        match operator {
            "==" | "eq" => (left - right).abs() < f64::EPSILON,
            "!=" | "ne" => (left - right).abs() >= f64::EPSILON,
            ">" | "gt" => left > right,
            "<" | "lt" => left < right,
            ">=" | "gte" => left >= right,
            "<=" | "lte" => left <= right,
            _ => false,
        }
    }

    /// Execute a compact value change action
    async fn execute_rule_change(
        &self,
        variable: &RuleVariable,
        assignment: &RuleValueAssignment,
        values: &HashMap<String, f64>,
    ) -> ActionResult {
        // Resolve the value to write. No 0.0 fallback — silently writing 0
        // when an assignment references a missing variable would corrupt
        // control commands (e.g. "set Y = X1" when X1 is unavailable).
        let raw_value: f64 = if let Some(n) = assignment.value.as_f64() {
            n
        } else if let Some(n) = assignment.value.as_i64() {
            n as f64
        } else if let Some(s) = assignment.value.as_str() {
            // String form is either (a) a numeric literal or (b) a variable name.
            if let Ok(n) = s.parse::<f64>() {
                n
            } else if let Some(&v) = values.get(s) {
                v
            } else {
                tracing::warn!(
                    "Rule action skipped: assignment value '{}' is neither a numeric \
                     literal nor a known variable",
                    s
                );
                return action_skipped(variable, "unknown variable in assignment");
            }
        } else {
            tracing::warn!(
                "Rule action skipped: assignment value type unsupported: {:?}",
                assignment.value
            );
            return action_skipped(variable, "unsupported assignment value type");
        };

        let (instance_id, point, value, pt) =
            match validate_write_target(variable, raw_value, "A", "Rule action") {
                Ok(v) => v,
                Err(action) => return action,
            };
        self.write_to_point(instance_id, point, value, pt, "Rule action")
            .await
    }

    /// Write a validated value to the appropriate point (M or A) and build ActionResult
    async fn write_to_point(
        &self,
        instance_id: u32,
        point: u32,
        value: f64,
        pt: &'static str,
        context: &str,
    ) -> ActionResult {
        let success = match pt {
            "M" => self
                .write_measurement_point(instance_id, point, value)
                .await
                .is_ok(),
            "A" => {
                self.write_action_point(instance_id, point, value, context)
                    .await
            },
            _ => {
                tracing::warn!("Unknown point type '{}' for {}", pt, context);
                false
            },
        };

        ActionResult {
            target_type: "instance",
            target_id: instance_id,
            point_type: pt,
            point_id: point,
            value,
            success,
        }
    }

    async fn write_action_point(
        &self,
        instance_id: u32,
        point: u32,
        value: f64,
        context: &str,
    ) -> bool {
        let Some(commands) = self.action_commands.as_ref() else {
            tracing::error!(
                "{} dispatch failed: governed action command facade not configured for instance_id={}, point_id={}",
                context,
                instance_id,
                point
            );
            return false;
        };
        let command =
            RuleActionCommand::new(InstanceId::new(instance_id), PointId::new(point), value);
        match commands.write_action(command).await {
            Ok(_) => true,
            Err(error) => {
                tracing::error!(
                    "{} governed action failed for instance_id={}, point_id={}: {}",
                    context,
                    instance_id,
                    point,
                    error
                );
                false
            },
        }
    }

    /// Write calculation result to instance point (M or A)
    async fn write_calculation_result(
        &self,
        variable: &RuleVariable,
        raw_value: f64,
        calc: &CalculationRule,
    ) -> ActionResult {
        let (instance_id, point, value, pt) = match validate_write_target(
            variable,
            raw_value,
            "M",
            &format!("Calc '{}'", calc.output),
        ) {
            Ok(v) => v,
            Err(action) => return action,
        };
        self.write_to_point(
            instance_id,
            point,
            value,
            pt,
            &format!("Calc '{}'", calc.output),
        )
        .await
    }

    /// Write period delta result to instance point (always measurement type)
    async fn write_period_delta_result(
        &self,
        variable: &RuleVariable,
        raw_value: f64,
        period: &str,
    ) -> ActionResult {
        let (instance_id, point, value, _pt) = match validate_write_target(
            variable,
            raw_value,
            "M",
            &format!("PeriodDelta({})", period),
        ) {
            Ok(v) => v,
            Err(action) => return action,
        };
        tracing::debug!(
            "PeriodDelta write: inst:{}:M:{} = {} (period={})",
            instance_id,
            point,
            value,
            period
        );
        self.write_to_point(
            instance_id,
            point,
            value,
            "M",
            &format!("PeriodDelta({})", period),
        )
        .await
    }

    /// Reject derived Measurement writes until automation owns a dedicated SHM
    /// plane for them. Writing into IO-owned T/S slots would violate the
    /// single-writer contract, and a network-store fallback would create a
    /// second live-state authority.
    async fn write_measurement_point(
        &self,
        instance_id: u32,
        point: u32,
        value: f64,
    ) -> Result<()> {
        Err(crate::error::RuleError::ExecutionError(format!(
            "derived Measurement write rejected: inst:{}:M:{}={} has no automation-owned SHM plane",
            instance_id, point, value
        )))
    }
}

#[cfg(test)]
#[allow(clippy::disallowed_methods)]
#[path = "executor_tests.rs"]
mod tests;
