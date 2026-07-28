//! Deterministic expression calculation owned by the rule engine.

mod builtin_functions;
mod error;
mod evaluator;
mod state;

pub(crate) use evaluator::CalcEngine;
pub(crate) use state::CalculationState;
