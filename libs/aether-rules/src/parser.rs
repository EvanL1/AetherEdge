//! Vue Flow JSON Parser
//!
//! Parses frontend Vue Flow rule JSON into the simplified RuleFlow structure
//! for execution by the rule engine.
//!
//! The parser extracts only execution-necessary information from Vue Flow JSON,
//! discarding UI-only data like positions, labels, and edge styling.

use crate::types::{
    CalculationRule, FlowCondition, RuleFlow, RuleNode, RuleSwitchBranch, RuleValueAssignment,
    RuleVariable, RuleWires,
};
use serde_json::Value;
use std::collections::HashMap;

use crate::error::{Result, RuleError};

// =============================================================================
// Rule Flow Extraction (Vue Flow JSON → RuleFlow)
// =============================================================================

/// Extract rule flow topology from Vue Flow JSON
///
/// This function converts the full Vue Flow JSON (with UI information like positions,
/// labels, edges) into a minimal RuleFlow structure containing only execution-necessary
/// information.
///
/// # Arguments
/// * `full_json` - The complete Vue Flow JSON from frontend
///
/// # Returns
/// * `Ok(RuleFlow)` - Simplified topology with HashMap-based node lookup
/// * `Err(RuleError)` - If parsing fails or required fields are missing
///
/// # Example
/// ```ignore
/// let flow = extract_rule_flow(&vue_flow_json)?;
/// // flow.nodes is HashMap<String, RuleNode> for O(1) lookup
/// ```
pub fn extract_rule_flow(full_json: &Value) -> Result<RuleFlow> {
    let nodes_array = full_json
        .get("nodes")
        .and_then(|v| v.as_array())
        .ok_or_else(|| RuleError::ParseError("Missing 'nodes' array".to_string()))?;

    let mut nodes = HashMap::new();
    let mut start_node = String::new();

    for node in nodes_array {
        let node_id = node
            .get("id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| RuleError::ParseError("Node missing 'id'".to_string()))?
            .to_string();

        let node_type = node
            .get("type")
            .and_then(|v| v.as_str())
            .unwrap_or("custom");

        let data = node.get("data");

        let compact_node = match node_type {
            "start" => {
                start_node = node_id.clone();
                let wires = extract_rule_wires_default(data)?;
                RuleNode::Start { wires }
            },
            "end" => RuleNode::End,
            "custom" => {
                let inner_type = data
                    .and_then(|d| d.get("type"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("");

                match inner_type {
                    "function-switch" => extract_switch_rule_node(data)?,
                    "action-changeValue" => extract_change_value_rule_node(data)?,
                    "action-calculation" => extract_calculation_rule_node(data)?,
                    "action-periodDelta" => extract_period_delta_rule_node(data)?,
                    _ => {
                        tracing::warn!("Unknown node: {}", inner_type);
                        continue;
                    },
                }
            },
            _ => {
                tracing::warn!("Unknown top node: {}", node_type);
                continue;
            },
        };

        nodes.insert(node_id, compact_node);
    }

    if start_node.is_empty() {
        return Err(RuleError::ParseError(
            "No start node found in flow".to_string(),
        ));
    }

    Ok(RuleFlow { start_node, nodes })
}

/// Extract RuleWires from node data (for default wire)
fn extract_rule_wires_default(data: Option<&Value>) -> Result<RuleWires> {
    let default_targets = data
        .and_then(|d| d.get("config"))
        .or(data)
        .and_then(|c| c.get("wires"))
        .and_then(|w| w.get("default"))
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();

    Ok(RuleWires {
        default: default_targets,
    })
}

/// Extract function-switch node as RuleNode::Switch
fn extract_switch_rule_node(data: Option<&Value>) -> Result<RuleNode> {
    let config = data
        .and_then(|d| d.get("config"))
        .ok_or_else(|| RuleError::ParseError("Switch node missing 'config'".to_string()))?;

    // Extract variables
    let variables = extract_rule_variables(config)?;

    // Extract rules
    let rule = extract_rule_switch_branches(config)?;

    // Extract wires (HashMap for multiple outputs)
    let wires = extract_rule_wires_map(config)?;

    Ok(RuleNode::Switch {
        variables,
        rule,
        wires,
    })
}

/// Extract action-changeValue node as RuleNode::ChangeValue
fn extract_change_value_rule_node(data: Option<&Value>) -> Result<RuleNode> {
    let config = data.and_then(|d| d.get("config"));

    // Extract variables (target points)
    let variables = config
        .map(extract_rule_variables)
        .transpose()?
        .unwrap_or_default();

    // Extract value assignments
    let rule = config
        .map(extract_rule_value_assignments)
        .transpose()?
        .unwrap_or_default();

    // Extract wires (default output)
    let wires = extract_rule_wires_default(config)?;

    Ok(RuleNode::ChangeValue {
        variables,
        rule,
        wires,
    })
}

/// Extract action-calculation node as RuleNode::Calculation
fn extract_calculation_rule_node(data: Option<&Value>) -> Result<RuleNode> {
    let config = data.and_then(|d| d.get("config"));

    // Extract variables (input sources and output targets)
    let variables = config
        .map(extract_rule_variables)
        .transpose()?
        .unwrap_or_default();

    // Extract calculation rules with formulas
    let rule = config
        .map(extract_calculation_rules)
        .transpose()?
        .unwrap_or_default();

    // Extract wires (default output)
    let wires = extract_rule_wires_default(config)?;

    Ok(RuleNode::Calculation {
        variables,
        rule,
        wires,
    })
}

/// Extract action-periodDelta node as RuleNode::PeriodDelta
///
/// Vue Flow JSON format:
/// ```json
/// {
///   "type": "action-periodDelta",
///   "config": {
///     "input": { "name": "X1", "instance": 1, "pointType": "measurement", "point_id": 9 },
///     "output": { "name": "Y1", "instance": 1, "pointType": "measurement", "point_id": 101 },
///     "period": "daily",
///     "wires": { "default": ["next-node-id"] }
///   }
/// }
/// ```
fn extract_period_delta_rule_node(data: Option<&Value>) -> Result<RuleNode> {
    let config = data
        .and_then(|d| d.get("config"))
        .ok_or_else(|| RuleError::ParseError("PeriodDelta node missing 'config'".to_string()))?;

    // Extract input variable
    let input = extract_single_rule_variable(config.get("input"), "input")?;

    // Extract output variable
    let output = extract_single_rule_variable(config.get("output"), "output")?;

    // Extract period type
    let period = config
        .get("period")
        .and_then(|v| v.as_str())
        .ok_or_else(|| RuleError::ParseError("PeriodDelta node missing 'period'".to_string()))?
        .to_string();

    // Validate period type
    if !["daily", "weekly", "monthly", "quarterly"].contains(&period.as_str()) {
        return Err(RuleError::ParseError(format!(
            "Invalid period type: '{}'. Must be one of: daily, weekly, monthly, quarterly",
            period
        )));
    }

    // Extract wires (default output)
    let wires = extract_rule_wires_default(Some(config))?;

    Ok(RuleNode::PeriodDelta {
        input,
        output,
        period,
        wires,
    })
}

/// Extract a single RuleVariable from JSON value
fn extract_single_rule_variable(value: Option<&Value>, field_name: &str) -> Result<RuleVariable> {
    let var = value.ok_or_else(|| {
        RuleError::ParseError(format!("PeriodDelta node missing '{}'", field_name))
    })?;

    let name = var
        .get("name")
        .and_then(|v| v.as_str())
        .ok_or_else(|| RuleError::ParseError(format!("{} variable missing 'name'", field_name)))?
        .to_string();

    // Support both "instance" and "instance_id" as numeric ID
    let instance = var
        .get("instance")
        .or_else(|| var.get("instance_id"))
        .and_then(|v| v.as_u64())
        .and_then(|n| u32::try_from(n).ok());

    let point_type = var
        .get("pointType")
        .and_then(|v| v.as_str())
        .map(String::from);

    // Accept both "point_id" and "point"
    let point = var
        .get("point_id")
        .or_else(|| var.get("point"))
        .and_then(|v| v.as_u64())
        .and_then(|n| u32::try_from(n).ok());

    let formula = var
        .get("formula")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    Ok(RuleVariable {
        name,
        instance,
        point_type,
        point,
        formula,
    })
}

/// Extract compact variables from config
fn extract_rule_variables(config: &Value) -> Result<Vec<RuleVariable>> {
    let vars_arr = match config.get("variables").and_then(|v| v.as_array()) {
        Some(arr) => arr,
        None => return Ok(vec![]),
    };

    let mut variables = Vec::new();
    for var in vars_arr {
        let name = var
            .get("name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| RuleError::ParseError("Variable missing 'name'".to_string()))?
            .to_string();

        // Support both "instance" and "instance_id" as numeric ID
        let instance = var
            .get("instance")
            .or_else(|| var.get("instance_id"))
            .and_then(|v| v.as_u64())
            .and_then(|n| u32::try_from(n).ok());

        let point_type = var
            .get("pointType")
            .and_then(|v| v.as_str())
            .map(String::from);

        // Accept both "point_id" (parser tests) and "point" (frontend/executor tests)
        let point = var
            .get("point_id")
            .or_else(|| var.get("point"))
            .and_then(|v| v.as_u64())
            .and_then(|n| u32::try_from(n).ok());

        let formula = var
            .get("formula")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();

        variables.push(RuleVariable {
            name,
            instance,
            point_type,
            point,
            formula,
        });
    }

    Ok(variables)
}

/// Extract compact switch rules from config
fn extract_rule_switch_branches(config: &Value) -> Result<Vec<RuleSwitchBranch>> {
    let rules_arr = match config.get("rule").and_then(|v| v.as_array()) {
        Some(arr) => arr,
        None => return Ok(vec![]),
    };

    let mut rules = Vec::new();
    for rule in rules_arr {
        let name = rule
            .get("name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| RuleError::ParseError("Rule missing 'name'".to_string()))?
            .to_string();

        let rule_type = rule
            .get("type")
            .and_then(|v| v.as_str())
            .unwrap_or("default")
            .to_string();

        let conditions = extract_flow_conditions(rule)?;

        rules.push(RuleSwitchBranch {
            name,
            rule_type,
            rule: conditions,
        });
    }

    Ok(rules)
}

/// Extract compact conditions from a rule
fn extract_flow_conditions(rule: &Value) -> Result<Vec<FlowCondition>> {
    let rule_arr = match rule.get("rule").and_then(|v| v.as_array()) {
        Some(arr) => arr,
        None => return Ok(vec![]),
    };

    let mut conditions = Vec::new();
    for item in rule_arr {
        let cond_type = item
            .get("type")
            .and_then(|v| v.as_str())
            .unwrap_or("variable")
            .to_string();

        let variables = item
            .get("variables")
            .and_then(|v| v.as_str())
            .map(String::from);
        let operator = item
            .get("operator")
            .and_then(|v| v.as_str())
            .map(String::from);
        let value = item.get("value").cloned();

        conditions.push(FlowCondition {
            cond_type,
            variables,
            operator,
            value,
        });
    }

    Ok(conditions)
}

/// Extract compact value assignments from config
fn extract_rule_value_assignments(config: &Value) -> Result<Vec<RuleValueAssignment>> {
    let rules_arr = match config.get("rule").and_then(|v| v.as_array()) {
        Some(arr) => arr,
        None => return Ok(vec![]),
    };

    let mut assignments = Vec::new();
    for rule in rules_arr {
        let variables = rule
            .get("Variables")
            .and_then(|v| v.as_str())
            .ok_or_else(|| RuleError::ParseError("Assignment missing 'Variables'".to_string()))?
            .to_string();

        let value = rule
            .get("value")
            .cloned()
            .ok_or_else(|| RuleError::ParseError("Assignment missing 'value'".to_string()))?;

        assignments.push(RuleValueAssignment { variables, value });
    }

    Ok(assignments)
}

/// Extract calculation rules from config
fn extract_calculation_rules(config: &Value) -> Result<Vec<CalculationRule>> {
    let rules_arr = match config.get("rule").and_then(|v| v.as_array()) {
        Some(arr) => arr,
        None => return Ok(vec![]),
    };

    let mut rules = Vec::new();
    for rule in rules_arr {
        let output = rule
            .get("output")
            .and_then(|v| v.as_str())
            .ok_or_else(|| RuleError::ParseError("Calculation rule missing 'output'".to_string()))?
            .to_string();

        let formula = rule
            .get("formula")
            .and_then(|v| v.as_str())
            .ok_or_else(|| RuleError::ParseError("Calculation rule missing 'formula'".to_string()))?
            .to_string();

        rules.push(CalculationRule { output, formula });
    }

    Ok(rules)
}

/// Extract wires as HashMap for multiple outputs (used by switch nodes)
fn extract_rule_wires_map(config: &Value) -> Result<HashMap<String, Vec<String>>> {
    let wires_obj = match config.get("wires").and_then(|v| v.as_object()) {
        Some(obj) => obj,
        None => return Ok(HashMap::new()),
    };

    let mut wires_map = HashMap::new();
    for (key, value) in wires_obj {
        let targets: Vec<String> = value
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();
        wires_map.insert(key.clone(), targets);
    }

    Ok(wires_map)
}

/// The two `rules` table flow columns, produced together.
///
/// Named fields (rather than a `(String, String)` tuple) so call sites
/// cannot silently swap the editor document and the execution topology
/// when binding SQL parameters.
#[derive(Debug, Clone)]
pub struct FlowColumns {
    /// Original Vue Flow editor document → `rules.flow_json`.
    pub flow_json: String,
    /// Compact execution topology → `rules.nodes_json`.
    pub nodes_json: String,
}

/// Serialize a Vue Flow document into both `rules` table flow columns.
///
/// Invariant: `flow_json` (editor document) and `nodes_json` (execution
/// topology) must always be written together — a row where they diverge
/// silently executes a different rule than the editor shows. Every SQL
/// site that writes either column must take both values from here instead
/// of serializing them itself. Current writers: `repository::upsert_rule`,
/// automation `update_rule`, `aether sync`.
pub fn flow_column_values(flow: &Value) -> Result<FlowColumns> {
    let compact = extract_rule_flow(flow)?;
    Ok(FlowColumns {
        flow_json: serde_json::to_string(flow)?,
        nodes_json: serde_json::to_string(&compact)?,
    })
}

#[cfg(test)]
#[allow(clippy::disallowed_methods)] // Test code - unwrap is acceptable
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_flow_column_values_produces_both_columns_together() {
        let flow = json!({
            "nodes": [
                {
                    "id": "start",
                    "type": "start",
                    "position": { "x": 0, "y": 0 },
                    "data": { "config": { "wires": { "default": ["end"] } } }
                },
                { "id": "end", "type": "end", "position": { "x": 100, "y": 0 } }
            ],
            "edges": []
        });

        let cols = flow_column_values(&flow).unwrap();

        // nodes_json must be exactly what extract_rule_flow produces — the
        // two columns cannot diverge when produced through this function.
        // Compare as parsed Values: RuleFlow.nodes is a HashMap, so the
        // serialized key order is not deterministic.
        let actual_nodes: Value = serde_json::from_str(&cols.nodes_json).unwrap();
        let expected_nodes: Value =
            serde_json::to_value(extract_rule_flow(&flow).unwrap()).unwrap();
        assert_eq!(actual_nodes, expected_nodes);

        // flow_json must round-trip to the original editor document.
        let round_trip: Value = serde_json::from_str(&cols.flow_json).unwrap();
        assert_eq!(round_trip, flow);

        // Invalid flow (missing nodes) must fail as a unit — no partial output.
        assert!(flow_column_values(&json!({"edges": []})).is_err());
    }

    #[test]
    fn test_extract_rule_flow() {
        // Test Vue Flow JSON similar to user's example
        let flow = json!({
            "id": "rule-001",
            "name": "Test Rule",
            "nodes": [
                {
                    "id": "start",
                    "type": "start",
                    "position": { "x": 0, "y": 0 },
                    "data": {
                        "config": {
                            "wires": { "default": ["node-1"] }
                        }
                    }
                },
                {
                    "id": "node-1",
                    "type": "custom",
                    "position": { "x": 100, "y": 100 },
                    "data": {
                        "type": "function-switch",
                        "label": "Check Value",
                        "config": {
                            "variables": [
                                {
                                    "name": "X1",
                                    "type": "single",
                                    "instance": 1,
                                    "pointType": "measurement",
                                    "point_id": 3
                                }
                            ],
                            "rule": [
                                {
                                    "name": "out001",
                                    "type": "default",
                                    "rule": [
                                        {
                                            "type": "variable",
                                            "variables": "X1",
                                            "operator": "<=",
                                            "value": 5
                                        }
                                    ]
                                }
                            ],
                            "wires": {
                                "out001": ["node-2"],
                                "out002": ["end"]
                            }
                        }
                    }
                },
                {
                    "id": "node-2",
                    "type": "custom",
                    "position": { "x": 200, "y": 200 },
                    "data": {
                        "type": "action-changeValue",
                        "config": {
                            "variables": [
                                {
                                    "name": "Y1",
                                    "type": "single",
                                    "instance": 2,
                                    "pointType": "action",
                                    "point_id": 5
                                }
                            ],
                            "rule": [
                                { "Variables": "Y1", "value": 999 }
                            ],
                            "wires": { "default": ["end"] }
                        }
                    }
                },
                {
                    "id": "end",
                    "type": "end",
                    "position": { "x": 300, "y": 300 }
                }
            ],
            "edges": [
                { "id": "e1", "source": "start", "target": "node-1" }
            ],
            "metadata": { "exportedAt": "2024-01-01" }
        });

        let compact = extract_rule_flow(&flow).unwrap();

        // Verify start node
        assert_eq!(compact.start_node, "start");
        assert_eq!(compact.nodes.len(), 4);

        // Verify start node structure
        match compact.nodes.get("start").unwrap() {
            RuleNode::Start { wires } => {
                assert_eq!(wires.default, vec!["node-1"]);
            },
            _ => panic!("Expected Start node"),
        }

        // Verify switch node structure
        match compact.nodes.get("node-1").unwrap() {
            RuleNode::Switch {
                variables,
                rule,
                wires,
            } => {
                assert_eq!(variables.len(), 1);
                assert_eq!(variables[0].name, "X1");
                // Note: instance is now Option<u32>, parsed from JSON
                assert_eq!(variables[0].instance, Some(1));
                assert_eq!(variables[0].point_type, Some("measurement".to_string()));
                assert_eq!(variables[0].point, Some(3));

                assert_eq!(rule.len(), 1);
                assert_eq!(rule[0].name, "out001");

                assert_eq!(wires.get("out001").unwrap(), &vec!["node-2"]);
                assert_eq!(wires.get("out002").unwrap(), &vec!["end"]);
            },
            _ => panic!("Expected Switch node"),
        }

        // Verify changeValue node structure
        match compact.nodes.get("node-2").unwrap() {
            RuleNode::ChangeValue {
                variables,
                rule,
                wires,
            } => {
                assert_eq!(variables.len(), 1);
                assert_eq!(variables[0].name, "Y1");
                assert_eq!(variables[0].point_type, Some("action".to_string()));

                assert_eq!(rule.len(), 1);
                assert_eq!(rule[0].variables, "Y1");
                assert_eq!(rule[0].value, json!(999));

                assert_eq!(wires.default, vec!["end"]);
            },
            _ => panic!("Expected ChangeValue node"),
        }

        // Verify end node
        assert!(matches!(compact.nodes.get("end").unwrap(), RuleNode::End));
    }

    #[test]
    fn test_compact_flow_serialization() {
        // Test that RuleFlow can be serialized to the expected JSON format
        let flow = json!({
            "nodes": [
                {
                    "id": "start",
                    "type": "start",
                    "data": {
                        "config": {
                            "wires": { "default": ["end"] }
                        }
                    }
                },
                {
                    "id": "end",
                    "type": "end"
                }
            ]
        });

        let compact = extract_rule_flow(&flow).unwrap();
        let serialized = serde_json::to_string(&compact).unwrap();
        let deserialized: RuleFlow = serde_json::from_str(&serialized).unwrap();

        assert_eq!(deserialized.start_node, "start");
        assert_eq!(deserialized.nodes.len(), 2);
    }
}
