//! Rule execution type definitions
//!
//! Core types for rule parsing and execution:
//! - Rule: execution structure with compact flow topology
//! - RuleFlow: simplified flow topology for execution
//! - RuleNode: node variants (Start, End, Switch, ChangeValue, Calculation)
//! - Supporting types for variables, conditions, and assignments

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// ============================================================================
// Rule Execution Structure
// ============================================================================

/// Rule - execution structure with compact flow topology
/// This is the internal representation used by the execution engine
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Rule {
    /// Unique identifier
    pub id: i64,

    /// Rule name
    pub name: String,

    /// Optional description
    pub description: Option<String>,

    /// Whether the rule is enabled
    #[serde(default = "default_enabled")]
    pub enabled: bool,

    /// Execution priority (higher = earlier)
    #[serde(default)]
    pub priority: u32,

    /// Cooldown period in milliseconds (legacy, use trigger_config for new rules)
    #[serde(default)]
    pub cooldown_ms: u64,

    /// Trigger configuration (JSON string)
    ///
    /// Supported formats:
    /// - `{"type": "interval", "interval_ms": 1000}` - Execute at fixed intervals
    ///
    /// If not set, falls back to `cooldown_ms` as interval.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger_config: Option<String>,

    /// Rule flow topology (nodes with local variables)
    pub flow: RuleFlow,
}

fn default_enabled() -> bool {
    true
}

// ============================================================================
// Compact Flow Structures (Vue Flow → Simplified Topology)
// ============================================================================

/// Rule flow topology - simplified structure for execution
/// Extracted from full Vue Flow JSON, discarding UI-only information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuleFlow {
    /// ID of the start node
    pub start_node: String,

    /// Nodes indexed by ID for O(1) lookup
    pub nodes: HashMap<String, RuleNode>,
}

/// Rule node - execution-only node structure
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum RuleNode {
    /// Start node - entry point
    #[serde(rename = "start")]
    Start {
        /// Output wires
        wires: RuleWires,
    },

    /// End node - termination point
    #[serde(rename = "end")]
    End,

    /// Switch node - conditional branching (function-switch)
    #[serde(rename = "function-switch")]
    Switch {
        /// Node-local variable definitions
        variables: Vec<RuleVariable>,
        /// Condition rules
        rule: Vec<RuleSwitchBranch>,
        /// Output wires (keyed by output name, e.g., "out001")
        wires: HashMap<String, Vec<String>>,
    },

    /// Change value action node
    #[serde(rename = "action-changeValue")]
    ChangeValue {
        /// Node-local variable definitions (target points)
        variables: Vec<RuleVariable>,
        /// Value assignments
        rule: Vec<RuleValueAssignment>,
        /// Output wires
        wires: RuleWires,
    },

    /// Calculation action node - formula evaluation
    #[serde(rename = "action-calculation")]
    Calculation {
        /// Input variables (also serve as output targets)
        variables: Vec<RuleVariable>,
        /// Calculation rules with formulas
        rule: Vec<CalculationRule>,
        /// Output wires
        wires: RuleWires,
    },

    /// Period delta action node - calculates value change within a period
    ///
    /// Tracks cumulative values (like energy meters) and calculates the delta
    /// within a specified period (daily, weekly, monthly, quarterly).
    /// Handles counter resets and period rollovers automatically.
    #[serde(rename = "action-periodDelta")]
    PeriodDelta {
        /// Input variable (source cumulative value, e.g., total charge energy)
        input: RuleVariable,
        /// Output variable (period delta result, e.g., daily charge energy)
        output: RuleVariable,
        /// Period type: "daily", "weekly", "monthly", "quarterly"
        period: String,
        /// Output wires
        wires: RuleWires,
    },
}

/// Rule wires - output connections
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RuleWires {
    /// Default output connections
    #[serde(default)]
    pub default: Vec<String>,
}

/// Rule variable definition (matches Vue Flow format)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuleVariable {
    /// Variable name (e.g., "X1")
    pub name: String,

    /// Instance ID (numeric), supports both "instance" and "instance_id" in JSON
    #[serde(alias = "instance_id", skip_serializing_if = "Option::is_none")]
    pub instance: Option<u32>,

    /// Point type: "measurement" or "action"
    #[serde(rename = "pointType", skip_serializing_if = "Option::is_none")]
    pub point_type: Option<String>,

    /// Point ID (numeric)
    #[serde(rename = "point_id", skip_serializing_if = "Option::is_none")]
    pub point: Option<u32>,

    /// Formula tokens (for combined type, if non-empty this is a calculated variable)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub formula: Vec<serde_json::Value>,
}

/// Rule switch branch (condition branch)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuleSwitchBranch {
    /// Output port name (e.g., "out001")
    pub name: String,

    /// Rule type (currently only "default")
    #[serde(rename = "type")]
    pub rule_type: String,

    /// Conditions
    pub rule: Vec<FlowCondition>,
}

/// Flow condition (used in RuleFlow)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FlowCondition {
    /// Condition type: "variable" or "relation"
    #[serde(rename = "type")]
    pub cond_type: String,

    /// Variable name (for variable type)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub variables: Option<String>,

    /// Comparison operator: "<=", ">=", "==", "!=", "<", ">"
    #[serde(skip_serializing_if = "Option::is_none")]
    pub operator: Option<String>,

    /// Comparison value (number or variable name)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<serde_json::Value>,
}

/// Rule value assignment
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuleValueAssignment {
    /// Target variable name
    #[serde(rename = "Variables")]
    pub variables: String,

    /// Value to assign (number or variable name)
    pub value: serde_json::Value,
}

/// Calculation rule for formula evaluation
///
/// Used by `action-calculation` nodes to compute values using evalexpr expressions.
/// Supports arithmetic (+, -, *, /), comparison (>, <, >=, <=, ==), logical (&&, ||),
/// and conditional expressions (if(cond, then, else)).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CalculationRule {
    /// Output variable name (must reference a variable in the node)
    pub output: String,

    /// Formula expression (evalexpr syntax, e.g., "a + b * 2")
    pub formula: String,
}
