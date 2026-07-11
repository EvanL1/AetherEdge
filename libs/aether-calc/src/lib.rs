//! aether-calc - Unified calculation library for AetherEMS
//!
//! Provides formula evaluation with built-in functions for industrial calculations.
//!
//! # Features
//!
//! - **Expression evaluation**: Arithmetic, comparison, and logic operations
//! - **Stateful functions**: `integrate()`, `moving_avg()`, `rate_of_change()`
//! - **Stateless functions**: `scale()`, `clamp()`, `abs()`, `min()`, `max()`, `round()`, `sign()`
//!
//! # Example
//!
//! ```rust
//! use aether_calc::{CalcEngine, MemoryStateStore};
//! use std::sync::Arc;
//! use std::collections::HashMap;
//!
//! # let rt = tokio::runtime::Builder::new_current_thread()
//! #     .enable_all()
//! #     .build()
//! #     .unwrap();
//! # rt.block_on(async {
//! // Create engine with memory state store
//! let store = Arc::new(MemoryStateStore::new());
//! let engine = CalcEngine::new(store, "my_context");
//!
//! // Define variables
//! let mut vars = HashMap::new();
//! vars.insert("P".to_string(), 1000.0);  // Power in W
//! vars.insert("efficiency".to_string(), 0.95);
//!
//! // Simple expressions (sync)
//! let result = engine.evaluate_simple("P * efficiency", &vars).unwrap();
//! assert_eq!(result, 950.0);
//!
//! // Built-in functions (sync)
//! let clamped = engine.evaluate_simple("clamp(P, 0, 500)", &vars).unwrap();
//! assert_eq!(clamped, 500.0);
//!
//! // Stateful functions (async)
//! let energy = engine.evaluate("integrate(P)", &vars).await.unwrap();
//! let avg = engine.evaluate("moving_avg(P, 10)", &vars).await.unwrap();
//! # });
//! ```
//!
//! # Built-in Functions
//!
//! ## Stateful (async, require state storage)
//!
//! | Function | Signature | Description |
//! |----------|-----------|-------------|
//! | `integrate` | `integrate(var)` or `integrate(var, factor)` | Time integral, auto-calculates Δt |
//! | `moving_avg` | `moving_avg(var, window)` | Sliding window average |
//! | `rate_of_change` | `rate_of_change(var)` | Rate of change dv/dt |
//! | `period_delta` | `period_delta(var, period)` | Delta within period (daily/weekly/monthly/quarterly) |
//!
//! ## Stateless (sync)
//!
//! | Function | Signature | Description |
//! |----------|-----------|-------------|
//! | `scale` | `scale(value, factor)` | Multiply by factor |
//! | `clamp` | `clamp(value, min, max)` | Limit to range |
//! | `abs` | `abs(value)` | Absolute value |
//! | `min` | `min(a, b)` | Minimum of two |
//! | `max` | `max(a, b)` | Maximum of two |
//! | `round` | `round(value, decimals)` | Round to decimals |
//! | `sign` | `sign(value)` | Sign: -1, 0, or 1 |

pub mod builtin_functions;
pub mod error;
pub mod evaluator;
pub mod state;

// Re-exports for convenience
pub use error::{CalcError, Result};
pub use evaluator::CalcEngine;
pub use state::{MemoryStateStore, StateStore};

// Re-export stateless functions for direct use
pub use builtin_functions::{abs, clamp, max, min, round, scale, sign};
