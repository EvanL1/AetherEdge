//! Formula Evaluation Module
//!
//! Provides expression evaluation for combined variables in the rule engine.
//! Uses evalexpr for safe mathematical expression parsing and evaluation.
//!
//! ## Example
//!
//! ```ignore
//! let mut variables = HashMap::new();
//! variables.insert("A".to_string(), 10.0);
//! variables.insert("B".to_string(), 5.0);
//!
//! let result = evaluate_formula("A + B * 2", &variables)?;
//! assert_eq!(result, 20.0);
//! ```

use evalexpr::{ContextWithMutableVariables, HashMapContext, Value, eval_number_with_context};
use std::collections::HashMap;

/// Evaluate a mathematical formula with variable substitution
///
/// # Arguments
/// * `formula` - Mathematical expression string (e.g., "A + B * 2")
/// * `variables` - Map of variable names to their current values
///
/// # Returns
/// * `Ok(f64)` - Calculated result
/// * `Err(String)` - Error message if evaluation fails
///
/// # Supported Operations
/// * Arithmetic: `+`, `-`, `*`, `/`, `%`
/// * Power: `^`
/// * Comparison: `==`, `!=`, `<`, `<=`, `>`, `>=`
/// * Parentheses: `()`
///
/// # Example Formulas
/// * `"soc_1 + soc_2"` - Sum of two variables
/// * `"(power_in - power_out) / power_in * 100"` - Efficiency calculation
/// * `"temperature > 50"` - Condition check (returns 1.0 or 0.0)
pub fn evaluate_formula(formula: &str, variables: &HashMap<String, f64>) -> Result<f64, String> {
    let mut context = HashMapContext::new();

    // Inject all variables into the context
    for (name, value) in variables {
        context
            .set_value(name.clone(), Value::Float(*value))
            .map_err(|e| format!("Failed to set variable '{}': {}", name, e))?;
    }

    // Evaluate the expression
    eval_number_with_context(formula, &context)
        .map_err(|e| format!("Formula '{}' evaluation failed: {}", formula, e))
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
#[allow(clippy::disallowed_methods)] // Test code - unwrap is acceptable
mod tests {
    use super::*;

    fn make_vars(pairs: &[(&str, f64)]) -> HashMap<String, f64> {
        pairs.iter().map(|(k, v)| (k.to_string(), *v)).collect()
    }

    #[test]
    fn test_basic_arithmetic() {
        let vars = make_vars(&[("A", 10.0), ("B", 5.0)]);

        assert_eq!(evaluate_formula("A + B", &vars).unwrap(), 15.0);
        assert_eq!(evaluate_formula("A - B", &vars).unwrap(), 5.0);
        assert_eq!(evaluate_formula("A * B", &vars).unwrap(), 50.0);
        assert_eq!(evaluate_formula("A / B", &vars).unwrap(), 2.0);
    }

    #[test]
    fn test_operator_precedence() {
        let vars = make_vars(&[("A", 10.0), ("B", 5.0), ("C", 2.0)]);

        // Multiplication before addition
        assert_eq!(evaluate_formula("A + B * C", &vars).unwrap(), 20.0);

        // Parentheses override precedence
        assert_eq!(evaluate_formula("(A + B) * C", &vars).unwrap(), 30.0);
    }

    #[test]
    fn test_power_operator() {
        let vars = make_vars(&[("X", 2.0), ("Y", 3.0)]);

        assert_eq!(evaluate_formula("X ^ Y", &vars).unwrap(), 8.0);
        assert_eq!(evaluate_formula("X ^ 2", &vars).unwrap(), 4.0);
    }

    #[test]
    fn test_efficiency_formula() {
        let vars = make_vars(&[("power_in", 100.0), ("power_out", 85.0)]);

        let result = evaluate_formula("power_out / power_in * 100", &vars).unwrap();
        assert!((result - 85.0).abs() < 0.001);
    }

    #[test]
    fn test_complex_nested() {
        let vars = make_vars(&[("soc_1", 80.0), ("soc_2", 60.0), ("capacity", 100.0)]);

        let result = evaluate_formula("(soc_1 + soc_2) / 2 / capacity * 100", &vars).unwrap();
        assert!((result - 70.0).abs() < 0.001);
    }

    #[test]
    fn test_missing_variable() {
        let vars = make_vars(&[("A", 10.0)]);

        let result = evaluate_formula("A + B", &vars);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("evaluation failed"));
    }

    #[test]
    fn test_invalid_syntax() {
        let vars = make_vars(&[("A", 10.0)]);

        let result = evaluate_formula("A + + B", &vars);
        assert!(result.is_err());
    }

    #[test]
    fn test_division_by_zero() {
        let vars = make_vars(&[("A", 10.0), ("B", 0.0)]);

        let result = evaluate_formula("A / B", &vars);
        // evalexpr returns infinity for division by zero, not an error
        assert!(result.is_ok());
        assert!(result.unwrap().is_infinite());
    }

    #[test]
    fn test_empty_variables() {
        let vars = HashMap::new();

        // Pure constant expression should work
        assert_eq!(evaluate_formula("2 + 3", &vars).unwrap(), 5.0);
    }

    #[test]
    fn test_negative_values() {
        let vars = make_vars(&[("A", -10.0), ("B", 5.0)]);

        assert_eq!(evaluate_formula("A + B", &vars).unwrap(), -5.0);
        assert_eq!(evaluate_formula("A * B", &vars).unwrap(), -50.0);
    }

    #[test]
    fn test_floating_point_precision() {
        let vars = make_vars(&[("A", 0.1), ("B", 0.2)]);

        let result = evaluate_formula("A + B", &vars).unwrap();
        // Allow small floating point error
        assert!((result - 0.3).abs() < 0.0001);
    }
}
