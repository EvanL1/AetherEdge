//! Rule execution logger
//!
//! Provides independent log files for each rule, capturing execution details
//! including variable values, matched conditions, and action results.

use std::{
    collections::HashMap,
    fmt::Write as FmtWrite,
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use crate::types::FlowCondition;
use chrono::{Local, Utc};
use tracing::warn;

use crate::executor::{ActionResult, RuleExecutionResult};

/// Logger for individual rule execution
pub struct RuleLogger {
    rule_id: String,
    log_dir: PathBuf,
    current_date: Mutex<String>,
    current_file: Mutex<Option<File>>,
}

impl RuleLogger {
    /// Create a new RuleLogger for a specific rule
    ///
    /// Log files will be created in: `{log_root}/rules/{rule_id}/`
    /// with naming format: `{YYYYMMDD}_{rule_name}.log`
    pub fn new(log_root: &Path, rule_id: i64, _rule_name: &str) -> Self {
        let rule_id_str = rule_id.to_string();
        let rule_dir = log_root.join("rules").join(&rule_id_str);
        if let Err(e) = fs::create_dir_all(&rule_dir) {
            warn!("Log dir err {:?}: {}", rule_dir, e);
        }

        Self {
            rule_id: rule_id_str,
            log_dir: rule_dir,
            current_date: Mutex::new(String::new()),
            current_file: Mutex::new(None),
        }
    }

    /// Log rule execution with matched condition only
    ///
    /// Format: `timestamp [RULE] rule_id vars | matched_condition | action_result`
    pub fn log_execution(&self, result: &RuleExecutionResult, vars: &HashMap<String, f64>) {
        // Format variable values: "X1=50.3 X2=25.0"
        // Optimization: use fmt::Write to avoid intermediate Vec allocation
        let vars_str = if vars.is_empty() {
            "-".to_string()
        } else {
            let mut result = String::with_capacity(vars.len() * 10);
            for (i, (k, v)) in vars.iter().enumerate() {
                if i > 0 {
                    result.push(' ');
                }
                let _ = write!(result, "{}={:.1}", k, v);
            }
            result
        };

        // Matched condition or "-"
        let cond_str = result.matched_condition.as_deref().unwrap_or("-");

        // Action results: "diesel_gen_01:A:2=1 OK"
        let actions_str = format_actions(&result.actions_executed, result.error.as_deref());

        // Compose: "X1=50.3 | X1>=49 | diesel_gen_01:A:2=1 OK"
        let message = format!("{} | {} | {}", vars_str, cond_str, actions_str);
        self.write_line(&message);
    }

    fn write_line(&self, message: &str) {
        let today = Local::now().format("%Y%m%d").to_string();
        let timestamp = Utc::now().format("%Y-%m-%dT%H:%M:%S%.3fZ");

        // Check if we need to rotate the file (new day)
        let Ok(mut current_date) = self.current_date.lock() else {
            warn!("Date lock fail");
            return;
        };
        let Ok(mut file_guard) = self.current_file.lock() else {
            warn!("File lock fail");
            return;
        };

        if *current_date != today {
            // New day - open new file
            *current_date = today.clone();
            let file_path = self.log_dir.join(format!("{}_{}.log", today, self.rule_id));

            match OpenOptions::new()
                .create(true)
                .append(true)
                .open(&file_path)
            {
                Ok(file) => *file_guard = Some(file),
                Err(e) => {
                    warn!("Log open err {:?}: {}", file_path, e);
                    return;
                },
            }
        }

        // Write the log line
        if let Some(ref mut file) = *file_guard {
            let line = format!("{} [RULE] {} {}\n", timestamp, self.rule_id, message);
            if let Err(e) = file.write_all(line.as_bytes()) {
                warn!("Log write err: {}", e);
            }
        }
    }
}

/// Format action results for logging
/// Optimization: use fmt::Write to avoid intermediate Vec allocation
fn format_actions(actions: &[ActionResult], error: Option<&str>) -> String {
    if let Some(err) = error {
        return err.to_string();
    }

    if actions.is_empty() {
        return "no action".to_string();
    }

    // Pre-allocate: ~40 chars per action (instance:type:point=value status)
    let mut result = String::with_capacity(actions.len() * 40);
    for (i, a) in actions.iter().enumerate() {
        if i > 0 {
            result.push_str(", ");
        }
        let status = if a.success { "OK" } else { "FAIL" };
        // Format: "instance_id:point_type:point_id=value OK"
        let _ = write!(
            result,
            "{}:{}:{}={} {}",
            a.target_id, a.point_type, a.point_id, a.value, status
        );
    }
    result
}

/// Format conditions as expression string (e.g., "X1>=49" or "X1>10 && X2<50")
pub fn format_conditions(conditions: &[FlowCondition]) -> String {
    if conditions.is_empty() {
        return String::new();
    }

    let mut parts = Vec::new();
    let mut pending_relation: Option<&str> = None;

    for cond in conditions {
        if cond.cond_type == "relation" {
            pending_relation = cond.value.as_ref().and_then(|v| v.as_str());
            continue;
        }

        // Format: "X1>=49"
        if let Some(var) = &cond.variables {
            let op = cond.operator.as_deref().unwrap_or("==");
            let val = cond
                .value
                .as_ref()
                .map(|v| {
                    // Remove quotes from string values
                    let s = v.to_string();
                    s.trim_matches('"').to_string()
                })
                .unwrap_or_default();

            let expr = format!("{}{}{}", var, op, val);

            // Add relation if pending
            if let Some(rel) = pending_relation.take() {
                let rel_str = match rel {
                    "||" | "or" | "OR" => " || ",
                    _ => " && ",
                };
                parts.push(rel_str.to_string());
            }
            parts.push(expr);
        }
    }

    parts.concat()
}

/// Manager for multiple rule loggers
pub struct RuleLoggerManager {
    log_root: PathBuf,
    loggers: Mutex<HashMap<String, Arc<RuleLogger>>>,
}

impl RuleLoggerManager {
    /// Create a new logger manager
    pub fn new(log_root: PathBuf) -> Self {
        Self {
            log_root,
            loggers: Mutex::new(HashMap::new()),
        }
    }

    /// Get or create a logger for a specific rule
    pub fn get_logger(&self, rule_id: i64, rule_name: &str) -> Arc<RuleLogger> {
        let rule_id_str = rule_id.to_string();
        let Ok(mut loggers) = self.loggers.lock() else {
            warn!("Loggers lock fail, temp logger");
            return Arc::new(RuleLogger::new(&self.log_root, rule_id, rule_name));
        };

        if let Some(logger) = loggers.get(&rule_id_str) {
            return Arc::clone(logger);
        }

        let logger = Arc::new(RuleLogger::new(&self.log_root, rule_id, rule_name));
        loggers.insert(rule_id_str, Arc::clone(&logger));
        logger
    }
}

#[cfg(test)]
#[allow(clippy::disallowed_methods)]
mod tests {
    use super::*;

    #[test]
    fn test_format_conditions_simple() {
        let conditions = vec![FlowCondition {
            cond_type: "variable".to_string(),
            variables: Some("X1".to_string()),
            operator: Some(">=".to_string()),
            value: Some(serde_json::json!(49)),
        }];

        assert_eq!(format_conditions(&conditions), "X1>=49");
    }

    #[test]
    fn test_format_conditions_compound_and() {
        let conditions = vec![
            FlowCondition {
                cond_type: "variable".to_string(),
                variables: Some("X1".to_string()),
                operator: Some(">".to_string()),
                value: Some(serde_json::json!(10)),
            },
            FlowCondition {
                cond_type: "relation".to_string(),
                variables: None,
                operator: None,
                value: Some(serde_json::json!("&&")),
            },
            FlowCondition {
                cond_type: "variable".to_string(),
                variables: Some("X2".to_string()),
                operator: Some("<".to_string()),
                value: Some(serde_json::json!(50)),
            },
        ];

        assert_eq!(format_conditions(&conditions), "X1>10 && X2<50");
    }

    #[test]
    fn test_format_conditions_compound_or() {
        let conditions = vec![
            FlowCondition {
                cond_type: "variable".to_string(),
                variables: Some("X1".to_string()),
                operator: Some("<=".to_string()),
                value: Some(serde_json::json!(5)),
            },
            FlowCondition {
                cond_type: "relation".to_string(),
                variables: None,
                operator: None,
                value: Some(serde_json::json!("||")),
            },
            FlowCondition {
                cond_type: "variable".to_string(),
                variables: Some("X1".to_string()),
                operator: Some(">=".to_string()),
                value: Some(serde_json::json!(95)),
            },
        ];

        assert_eq!(format_conditions(&conditions), "X1<=5 || X1>=95");
    }

    #[test]
    fn test_format_conditions_empty() {
        let conditions: Vec<FlowCondition> = vec![];
        assert_eq!(format_conditions(&conditions), "");
    }

    #[test]
    fn test_format_actions_success() {
        let actions = vec![ActionResult {
            target_type: "instance",
            target_id: 5,
            point_type: "A",
            point_id: 2,
            value: 1.0,
            success: true,
        }];

        assert_eq!(format_actions(&actions, None), "5:A:2=1 OK");
    }

    #[test]
    fn test_format_actions_failure() {
        let actions = vec![ActionResult {
            target_type: "instance",
            target_id: 5,
            point_type: "A",
            point_id: 2,
            value: 1.0,
            success: false,
        }];

        assert_eq!(format_actions(&actions, None), "5:A:2=1 FAIL");
    }

    #[test]
    fn test_format_actions_with_error() {
        let actions = vec![];
        assert_eq!(format_actions(&actions, Some("read failed")), "read failed");
    }

    #[test]
    fn test_format_actions_empty() {
        let actions: Vec<ActionResult> = vec![];
        assert_eq!(format_actions(&actions, None), "no action");
    }
}
