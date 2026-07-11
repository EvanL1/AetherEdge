//! Python script runner for custom JSON transformations.
//!
//! Manages a persistent Python subprocess that runs user-provided transform
//! scripts. Communication uses JSON-Lines over stdin/stdout.
//!
//! The subprocess is started once and reused for all messages on a channel,
//! avoiding Python interpreter startup overhead (~100ms per fork).

use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

use super::data::{DataBatch, DataPoint, Value};
use super::error::{GatewayError, Result};
use super::quality::Quality;
use aether_model::PointType;

/// Path to the Python host script relative to the project root.
const HOST_SCRIPT: &str = "libs/aether-script-host/main.py";

// ============================================================================
// Protocol types (JSON-Lines)
// ============================================================================

#[derive(Serialize)]
struct ScriptRequest<'a> {
    id: u64,
    payload: &'a serde_json::Value,
}

#[derive(Deserialize)]
#[allow(dead_code)]
struct ScriptResponse {
    id: u64,
    #[serde(default)]
    points: Option<Vec<PluginDataPoint>>,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    status: Option<String>,
}

/// A data point returned by the Python transform script.
#[derive(Debug, Deserialize)]
struct PluginDataPoint {
    point_id: u32,
    point_type: String,
    value: serde_json::Value,
    #[serde(default = "default_quality")]
    quality: String,
}

fn default_quality() -> String {
    "good".to_string()
}

// ============================================================================
// ScriptRunner
// ============================================================================

/// Manages a persistent Python subprocess for custom JSON transformations.
pub struct ScriptRunner {
    channel_id: u32,
    script_path: String,
    inner: Mutex<ScriptRunnerInner>,
    request_counter: std::sync::atomic::AtomicU64,
}

struct ScriptRunnerInner {
    child: Option<Child>,
    stdin: Option<std::process::ChildStdin>,
    reader: Option<BufReader<std::process::ChildStdout>>,
}

impl std::fmt::Debug for ScriptRunner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ScriptRunner")
            .field("channel_id", &self.channel_id)
            .field("script_path", &self.script_path)
            .finish()
    }
}

impl ScriptRunner {
    /// Create a new script runner for the given transform script.
    pub fn new(channel_id: u32, script_path: String) -> Self {
        Self {
            channel_id,
            script_path,
            inner: Mutex::new(ScriptRunnerInner {
                child: None,
                stdin: None,
                reader: None,
            }),
            request_counter: std::sync::atomic::AtomicU64::new(1),
        }
    }

    /// Start the Python subprocess. Called lazily on first transform.
    pub fn start(&self) -> Result<()> {
        let mut inner = self
            .inner
            .lock()
            .map_err(|_| GatewayError::Internal("ScriptRunner lock poisoned".to_string()))?;

        if inner.child.is_some() {
            return Ok(());
        }

        let host_path = find_host_script()?;
        info!(
            channel_id = self.channel_id,
            script = %self.script_path,
            host = %host_path,
            "Starting Python transform subprocess"
        );

        let mut child = Command::new("python3")
            .arg(&host_path)
            .arg(&self.script_path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| GatewayError::Config(format!("Failed to start Python subprocess: {e}")))?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| GatewayError::Internal("Failed to open subprocess stdin".to_string()))?;

        let stdout = child.stdout.take().ok_or_else(|| {
            GatewayError::Internal("Failed to open subprocess stdout".to_string())
        })?;

        let mut reader = BufReader::new(stdout);

        // Wait for "ready" signal
        let mut ready_line = String::new();
        reader
            .read_line(&mut ready_line)
            .map_err(|e| GatewayError::Config(format!("Python subprocess failed to start: {e}")))?;

        let ready: ScriptResponse = serde_json::from_str(&ready_line).map_err(|e| {
            GatewayError::Config(format!(
                "Invalid ready signal from Python subprocess: {e} — got: {ready_line}"
            ))
        })?;

        if ready.status.as_deref() != Some("ready") {
            if let Some(err) = ready.error {
                return Err(GatewayError::Config(format!(
                    "Python script load error: {err}"
                )));
            }
            return Err(GatewayError::Config(
                "Python subprocess did not send ready signal".to_string(),
            ));
        }

        debug!(
            channel_id = self.channel_id,
            "Python transform subprocess ready"
        );

        inner.child = Some(child);
        inner.stdin = Some(stdin);
        inner.reader = Some(reader);

        Ok(())
    }

    /// Transform a JSON payload using the Python script.
    ///
    /// Starts the subprocess lazily if not already running.
    /// On subprocess failure, attempts one restart.
    pub fn transform(&self, payload: &serde_json::Value) -> Result<DataBatch> {
        // Ensure subprocess is running
        if !self.is_running() {
            self.start()?;
        }

        match self.send_and_receive(payload) {
            Ok(batch) => Ok(batch),
            Err(e) => {
                warn!(
                    channel_id = self.channel_id,
                    error = %e,
                    "Python subprocess error, attempting restart"
                );
                self.kill();
                self.start()?;
                self.send_and_receive(payload)
            },
        }
    }

    /// Check if the subprocess is still running.
    fn is_running(&self) -> bool {
        let Ok(inner) = self.inner.lock() else {
            return false;
        };
        inner.child.is_some()
    }

    /// Send a request and read the response.
    fn send_and_receive(&self, payload: &serde_json::Value) -> Result<DataBatch> {
        let req_id = self
            .request_counter
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);

        let request = ScriptRequest {
            id: req_id,
            payload,
        };
        let mut request_json = serde_json::to_string(&request)
            .map_err(|e| GatewayError::Internal(format!("Failed to serialize request: {e}")))?;
        request_json.push('\n');

        let mut inner = self
            .inner
            .lock()
            .map_err(|_| GatewayError::Internal("ScriptRunner lock poisoned".to_string()))?;

        // Write request
        let stdin = inner
            .stdin
            .as_mut()
            .ok_or_else(|| GatewayError::Internal("Subprocess stdin not available".to_string()))?;
        stdin
            .write_all(request_json.as_bytes())
            .map_err(|e| GatewayError::Protocol(format!("Failed to write to subprocess: {e}")))?;
        stdin.flush().map_err(|e| {
            GatewayError::Protocol(format!("Failed to flush subprocess stdin: {e}"))
        })?;

        // Read response
        let reader = inner
            .reader
            .as_mut()
            .ok_or_else(|| GatewayError::Internal("Subprocess stdout not available".to_string()))?;
        let mut response_line = String::new();
        reader
            .read_line(&mut response_line)
            .map_err(|e| GatewayError::Protocol(format!("Failed to read from subprocess: {e}")))?;

        if response_line.is_empty() {
            return Err(GatewayError::Protocol(
                "Subprocess closed stdout unexpectedly".to_string(),
            ));
        }

        let response: ScriptResponse = serde_json::from_str(&response_line).map_err(|e| {
            GatewayError::InvalidResponse(format!("Invalid response from Python script: {e}"))
        })?;

        if let Some(err) = response.error {
            return Err(GatewayError::Protocol(format!(
                "Python transform error: {err}"
            )));
        }

        let plugin_points = response.points.unwrap_or_default();
        convert_plugin_points(&plugin_points)
    }

    /// Kill the subprocess (for cleanup or restart).
    fn kill(&self) {
        if let Ok(mut inner) = self.inner.lock() {
            inner.stdin = None;
            inner.reader = None;
            if let Some(mut child) = inner.child.take() {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
    }
}

impl Drop for ScriptRunner {
    fn drop(&mut self) {
        self.kill();
    }
}

// ============================================================================
// Helpers
// ============================================================================

/// Convert Python plugin data points to internal DataBatch.
fn convert_plugin_points(points: &[PluginDataPoint]) -> Result<DataBatch> {
    let mut batch = DataBatch::with_capacity(points.len());

    for p in points {
        let point_type = PointType::from_str(&p.point_type).ok_or_else(|| {
            GatewayError::DataConversion(format!("Invalid point_type: {}", p.point_type))
        })?;

        let value = convert_plugin_value(&p.value);
        let quality = parse_quality(&p.quality);

        let point = DataPoint::new(p.point_id, point_type, value).with_quality(quality);
        batch.add(point);
    }

    Ok(batch)
}

/// Convert a JSON value from the plugin to an internal Value.
fn convert_plugin_value(v: &serde_json::Value) -> Value {
    match v {
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Value::Integer(i)
            } else if let Some(f) = n.as_f64() {
                Value::Float(f)
            } else {
                Value::Null
            }
        },
        serde_json::Value::Bool(b) => Value::Bool(*b),
        serde_json::Value::String(s) => Value::String(s.clone()),
        serde_json::Value::Null => Value::Null,
        other => Value::String(other.to_string()),
    }
}

/// Parse a quality string from the plugin to internal Quality.
fn parse_quality(s: &str) -> Quality {
    if s.eq_ignore_ascii_case("good") {
        Quality::Good
    } else if s.eq_ignore_ascii_case("bad") {
        Quality::Bad
    } else if s.eq_ignore_ascii_case("uncertain") {
        Quality::Uncertain
    } else if s.eq_ignore_ascii_case("invalid") {
        Quality::Invalid
    } else {
        Quality::Good
    }
}

/// Find the host script path, searching relative to the executable.
fn find_host_script() -> Result<String> {
    // Try relative to current working directory
    if Path::new(HOST_SCRIPT).exists() {
        return Ok(HOST_SCRIPT.to_string());
    }

    // Try relative to executable
    if let Ok(exe) = std::env::current_exe()
        && let Some(exe_dir) = exe.parent()
    {
        // Walk up to find project root (look for Cargo.toml)
        let mut dir = exe_dir;
        for _ in 0..5 {
            let candidate = dir.join(HOST_SCRIPT);
            if candidate.exists() {
                return Ok(candidate.to_string_lossy().to_string());
            }
            if let Some(parent) = dir.parent() {
                dir = parent;
            } else {
                break;
            }
        }
    }

    // Try common installation paths
    let install_paths = [
        "/etc/aether/script-host/main.py",
        "/usr/local/share/aether/script-host/main.py",
    ];
    for path in &install_paths {
        if Path::new(path).exists() {
            return Ok((*path).to_string());
        }
    }

    Err(GatewayError::Config(format!(
        "Cannot find Python host script: {HOST_SCRIPT}"
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_convert_plugin_value_float() {
        let v = convert_plugin_value(&json!(3.25));
        assert_eq!(v.as_f64(), Some(3.25));
    }

    #[test]
    fn test_convert_plugin_value_int() {
        let v = convert_plugin_value(&json!(42));
        assert_eq!(v.as_i64(), Some(42));
    }

    #[test]
    fn test_convert_plugin_value_bool() {
        let v = convert_plugin_value(&json!(true));
        assert_eq!(v.as_bool(), Some(true));
    }

    #[test]
    fn test_convert_plugin_value_string() {
        let v = convert_plugin_value(&json!("hello"));
        assert_eq!(v.as_string(), Some("hello"));
    }

    #[test]
    fn test_parse_quality() {
        assert_eq!(parse_quality("good"), Quality::Good);
        assert_eq!(parse_quality("Good"), Quality::Good);
        assert_eq!(parse_quality("bad"), Quality::Bad);
        assert_eq!(parse_quality("uncertain"), Quality::Uncertain);
        assert_eq!(parse_quality("invalid"), Quality::Invalid);
        assert_eq!(parse_quality("unknown"), Quality::Good); // default
    }

    #[test]
    fn test_convert_plugin_points() {
        let points = vec![
            PluginDataPoint {
                point_id: 101,
                point_type: "T".to_string(),
                value: json!(42.5),
                quality: "good".to_string(),
            },
            PluginDataPoint {
                point_id: 110,
                point_type: "S".to_string(),
                value: json!(true),
                quality: "bad".to_string(),
            },
        ];

        let batch = convert_plugin_points(&points).unwrap();
        assert_eq!(batch.len(), 2);

        let p1 = batch.iter().next().unwrap();
        assert_eq!(p1.id, 101);
        assert_eq!(p1.point_type, PointType::Telemetry);
        assert_eq!(p1.quality, Quality::Good);
    }
}
