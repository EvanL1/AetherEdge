//! End-to-end: the real `aether` binary's stdout under `--json` must be
//! parseable JSON with nothing else mixed in. `tracing_subscriber::fmt()`
//! defaults to stdout; if logging isn't redirected to stderr, a `--verbose`
//! run injects log lines into the same stream the `--json` envelope uses.
//! This also guards the MCP server: MCP's stdio transport reserves stdout
//! for JSON-RPC frames exclusively, so any stray log line there is fatal.

use std::process::Command;
use tempfile::TempDir;

#[test]
fn json_output_has_no_log_lines_mixed_in() {
    let exe = env!("CARGO_BIN_EXE_aether");
    let output = Command::new(exe)
        .args(["--verbose", "--json", "net", "mqtt", "status"])
        .env_remove("AETHER_UPLINK_URL")
        .output()
        .expect("failed to run aether binary");

    let stdout = String::from_utf8(output.stdout).expect("stdout was not valid UTF-8");

    // Whatever happened to the request (uplink is not running in this test,
    // so we expect a connection-refused failure), stdout must be nothing
    // but the JSON envelope. If a DEBUG/INFO log line leaked onto stdout,
    // this parse fails.
    let parsed: serde_json::Value = serde_json::from_str(&stdout).unwrap_or_else(|e| {
        panic!("stdout was not valid JSON (log lines likely mixed in): {e}\nstdout was:\n{stdout}")
    });

    assert!(
        parsed.get("success").is_some(),
        "expected a {{success, ...}} envelope, got: {parsed}"
    );
}

#[test]
fn invalid_json_validation_is_one_error_envelope_and_exits_nonzero() {
    let workspace = TempDir::new().expect("create temporary workspace");
    let config_path = workspace.path().join("config");
    let data_path = workspace.path().join("data");
    std::fs::create_dir_all(&config_path).expect("create config directory");
    std::fs::write(config_path.join("global.yaml"), "invalid: [yaml\n")
        .expect("write invalid config");

    let exe = env!("CARGO_BIN_EXE_aether");
    let output = Command::new(exe)
        .args([
            "--json",
            "--config-path",
            config_path.to_str().expect("UTF-8 config path"),
            "--db-path",
            data_path.to_str().expect("UTF-8 data path"),
            "sync",
            "--dry-run",
        ])
        .output()
        .expect("run aether validation");

    assert!(!output.status.success());
    let stdout = String::from_utf8(output.stdout).expect("stdout was not UTF-8");
    let parsed: serde_json::Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|error| panic!("expected one JSON envelope: {error}\n{stdout}"));
    assert_eq!(parsed.get("success"), Some(&serde_json::Value::Bool(false)));
}

#[test]
fn unhealthy_json_doctor_is_one_error_envelope_and_exits_nonzero() {
    let workspace = TempDir::new().expect("create temporary workspace");
    let config_path = workspace.path().join("missing-config");
    let data_path = workspace.path().join("missing-data");

    let exe = env!("CARGO_BIN_EXE_aether");
    let output = Command::new(exe)
        .args([
            "--json",
            "--config-path",
            config_path.to_str().expect("UTF-8 config path"),
            "--db-path",
            data_path.to_str().expect("UTF-8 data path"),
            "doctor",
        ])
        .output()
        .expect("run aether doctor");

    assert!(!output.status.success());
    let stdout = String::from_utf8(output.stdout).expect("stdout was not UTF-8");
    let parsed: serde_json::Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|error| panic!("expected one JSON envelope: {error}\n{stdout}"));
    assert_eq!(parsed.get("success"), Some(&serde_json::Value::Bool(false)));
}

#[test]
fn json_sync_reports_offline_atomic_desired_state_semantics() {
    let workspace = TempDir::new().expect("create temporary workspace");
    let data_path = workspace.path().join("data");
    let config_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("config.template");

    let exe = env!("CARGO_BIN_EXE_aether");
    let output = Command::new(exe)
        .args([
            "--json",
            "--config-path",
            config_path.to_str().expect("UTF-8 config path"),
            "--db-path",
            data_path.to_str().expect("UTF-8 data path"),
            "sync",
            "--confirmed",
        ])
        .output()
        .expect("run aether sync");

    assert!(
        output.status.success(),
        "sync failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).expect("stdout was not UTF-8");
    let parsed: serde_json::Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|error| panic!("expected one JSON envelope: {error}\n{stdout}"));
    assert_eq!(
        parsed.pointer("/data/desired_state_atomic"),
        Some(&serde_json::Value::Bool(true))
    );
    assert_eq!(
        parsed.pointer("/data/runtime_activation"),
        Some(&serde_json::Value::String(
            "on_next_service_start".to_string()
        ))
    );
    assert!(parsed.pointer("/data/reload").is_none());
}

#[test]
fn configuration_apply_without_confirmation_fails_before_creating_the_database() {
    let workspace = TempDir::new().expect("create temporary workspace");
    let data_path = workspace.path().join("data");
    let config_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("config.template");
    let output = Command::new(env!("CARGO_BIN_EXE_aether"))
        .args([
            "--json",
            "--config-path",
            config_path.to_str().expect("UTF-8 config path"),
            "--db-path",
            data_path.to_str().expect("UTF-8 data path"),
            "sync",
        ])
        .output()
        .expect("run unconfirmed sync");

    assert!(!output.status.success());
    assert!(!data_path.join("aether.db").exists());
    let stdout = String::from_utf8(output.stdout).expect("UTF-8 JSON output");
    let parsed: serde_json::Value = serde_json::from_str(&stdout).expect("one JSON envelope");
    assert_eq!(parsed.get("success"), Some(&serde_json::Value::Bool(false)));
    assert!(
        parsed
            .get("error")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|message| message.contains("--confirmed"))
    );
}

#[test]
fn dry_run_rejects_invalid_nested_rule_before_touching_the_real_database() {
    let workspace = TempDir::new().expect("create temporary workspace");
    let config_path = workspace.path().join("config");
    let data_path = workspace.path().join("data");
    std::fs::create_dir_all(config_path.join("io")).expect("create io config directory");
    std::fs::create_dir_all(config_path.join("automation/rules"))
        .expect("create rules config directory");
    std::fs::write(config_path.join("global.yaml"), "site_name: validation\n")
        .expect("write global config");
    std::fs::write(
        config_path.join("io/io.yaml"),
        "channels:\n  - id: 1\n    name: disabled-simulator\n    protocol: virtual\n    enabled: false\n",
    )
    .expect("write io config");
    std::fs::write(
        config_path.join("automation/automation.yaml"),
        "auto_load_instances: false\n",
    )
    .expect("write automation config");
    std::fs::write(
        config_path.join("automation/rules/invalid.json"),
        "not-json",
    )
    .expect("write invalid nested rule");

    let exe = env!("CARGO_BIN_EXE_aether");
    let output = Command::new(exe)
        .args([
            "--json",
            "--config-path",
            config_path.to_str().expect("UTF-8 config path"),
            "--db-path",
            data_path.to_str().expect("UTF-8 data path"),
            "sync",
            "--dry-run",
        ])
        .output()
        .expect("run aether dry-run");

    assert!(!output.status.success());
    assert!(
        !data_path.join("aether.db").exists(),
        "dry-run must not create or change the real database"
    );
    let stdout = String::from_utf8(output.stdout).expect("stdout was not UTF-8");
    let parsed: serde_json::Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|error| panic!("expected one JSON envelope: {error}\n{stdout}"));
    assert_eq!(parsed.get("success"), Some(&serde_json::Value::Bool(false)));
}
