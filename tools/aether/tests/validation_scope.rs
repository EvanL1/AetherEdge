//! `sync --dry-run` must not claim validity it never checked.
//!
//! It reported "All configurations valid!" for a mapping CSV carrying an
//! out-of-range slave id, a nonexistent function code, and junk in every
//! remaining column. The governed API rejects each of those fields
//! individually, so the file path was handing out a green light for
//! configuration that is certain to be refused when the channel is activated.

use std::process::Command;

use tempfile::TempDir;

/// Runs `aether --json sync --dry-run` against the shipped template.
fn dry_run_json() -> serde_json::Value {
    let workspace = TempDir::new().expect("workspace");
    let config_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("config.template");

    let output = Command::new(env!("CARGO_BIN_EXE_aether"))
        .env("AETHER_SHM_PATH", workspace.path().join("absent-rtdb.shm"))
        .args([
            "--json",
            "--config-path",
            config_path.to_str().expect("UTF-8 config path"),
            "--db-path",
            workspace.path().to_str().expect("UTF-8 data path"),
            "sync",
            "--dry-run",
        ])
        .output()
        .expect("run aether sync --dry-run");

    let stdout = String::from_utf8(output.stdout).expect("stdout was not UTF-8");
    assert!(
        output.status.success(),
        "dry run should succeed on the shipped template: {stdout}"
    );
    serde_json::from_str(&stdout).expect("dry run emitted JSON")
}

#[test]
fn a_dry_run_names_the_checks_it_did_not_perform() {
    let report = dry_run_json();
    let data = &report["data"];

    let unchecked = data["unchecked"]
        .as_array()
        .expect("the report states what it did not check");
    assert!(
        unchecked
            .iter()
            .any(|entry| entry.as_str() == Some("protocol_mappings")),
        "protocol mappings are not validated here and the report must say so: {data}"
    );

    let note = data["unchecked_note"]
        .as_str()
        .expect("the report explains where the missing check happens");
    assert!(
        note.contains("activat"),
        "the note must point at when the real check runs: {note}"
    );
}

#[test]
fn a_dry_run_still_reports_what_it_did_validate() {
    // Narrowing the claim must not remove the signal: structure and reference
    // errors are still caught here, and callers still need the pass/fail bit.
    let report = dry_run_json();

    assert_eq!(report["data"]["all_valid"], serde_json::Value::Bool(true));
    assert!(
        report["data"]["configs"].is_array(),
        "per-config results stay in the report"
    );
}
