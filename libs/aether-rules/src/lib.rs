//! Aether Rules - Rule Engine Library
//!
//! A Vue Flow-based rule engine for AetherEMS providing:
//! - Rule parsing from Vue Flow JSON format
//! - Rule execution with condition evaluation and action dispatch
//! - Rule scheduling with interval-based triggers
//! - SQLite persistence for rule storage
//!
//! # Architecture
//!
//! ```text
//! ┌─────────────┐     ┌──────────────┐     ┌─────────────┐
//! │  Scheduler  │────▶│   Executor   │────▶│ Live state  │
//! │  +PointWatch│     │  (evaluate)  │     │   port      │
//! └─────────────┘     └──────────────┘     └─────────────┘
//!        │                   │
//!        ▼
//! ┌─────────────┐
//! │ Repository  │
//! │  (SQLite)   │
//! └─────────────┘
//! ```

mod error;
pub(crate) mod formula;
#[cfg(unix)]
mod live_state;
pub mod parser;
mod repository;
pub mod types;

// Rule engine runtime (executor/scheduler/logger) is Unix-only while its
// PointWatch subscription adapter uses POSIX mmap facilities.
// Windows builds only need the parser for `aether sync` (remote management CLI).
#[cfg(unix)]
mod action_command;
#[cfg(unix)]
mod executor;
#[cfg(unix)]
pub mod logger;
#[cfg(unix)]
pub mod point_watch_dispatcher;
#[cfg(unix)]
mod scheduler;

// Re-export public API
pub use error::{Result, RuleError};
pub use parser::{FlowColumns, extract_rule_flow, flow_column_values};
pub use repository::{
    delete_rule, get_rule, get_rule_for_execution, list_rules, list_rules_paginated,
    load_all_rules, load_enabled_rules, set_rule_enabled, upsert_rule,
};

#[cfg(unix)]
pub use action_command::{RuleActionCommand, RuleActionCommandFacade};
#[cfg(unix)]
pub use executor::{ActionResult, RuleExecutionResult, RuleExecutor};
#[cfg(unix)]
pub use live_state::{MemoryRuleLiveState, RuleExecutionContext, RuleLiveState};
#[cfg(unix)]
pub use logger::{RuleLogger, RuleLoggerManager, format_conditions};
#[cfg(unix)]
pub use point_watch_dispatcher::{
    MeasurementRouteBinding, PointWatchDispatcher, PointWatchHint, RuleSubscriptionInfo, WatchEvent,
};
#[cfg(unix)]
pub use scheduler::{
    DEFAULT_TICK_MS, OnChangeState, PointKind, PointRef, RuleScheduler, SchedulerStatus,
    TriggerConfig, ValueDeadband, should_trigger_onchange,
};

// Re-export rule types for convenience
pub use types::{
    CalculationRule, FlowCondition, Rule, RuleFlow, RuleNode, RuleSwitchBranch,
    RuleValueAssignment, RuleVariable, RuleWires,
};
