//! Guards the HTTP/CLI/MCP runtime contract against reviving a nonexistent
//! automation measurement-write capability. A legacy frontend wrapper may
//! still contain an old URL, but it is outside this edge-kernel change and is
//! not evidence of a service contract: automation's router remains authoritative.

use std::path::{Path, PathBuf};
use std::process::Command;

fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn read_repository_file(relative_path: &str) -> String {
    let path = repository_root().join(relative_path);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display()))
}

#[test]
fn cli_rejects_the_retired_instance_measurement_subcommand() {
    let output = Command::new(env!("CARGO_BIN_EXE_aether"))
        .args([
            "models",
            "instances",
            "measurement",
            "3",
            "--point-id",
            "101",
            "--value",
            "1.0",
        ])
        .output()
        .expect("run aether CLI");

    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).expect("CLI stderr is UTF-8");
    assert!(
        stderr.contains("unrecognized subcommand") || stderr.contains("unexpected argument"),
        "retired command reached runtime instead of being rejected by clap: {stderr}"
    );
}

#[test]
fn source_and_reference_docs_do_not_publish_the_retired_write_route() {
    let model_client = read_repository_file("tools/aether/src/models/client.rs");
    assert!(
        !model_client.contains("/api/instances/{}/measurement"),
        "ModelClient still calls the nonexistent automation route"
    );

    let http_reference = read_repository_file("docs/reference/http-api.md");
    assert!(
        !http_reference.contains("POST | `/api/instances/{id}/measurement`")
            && !http_reference.contains("POST /api/instances/{id}/measurement"),
        "HTTP reference still advertises the nonexistent automation route"
    );

    let automation_router = read_repository_file("services/automation/src/routes.rs");
    assert!(
        !automation_router.contains("\"/api/instances/{id}/measurement\""),
        "automation unexpectedly acquired an instance-measurement write route"
    );

    let legacy_api_reference = read_repository_file("docs/API_REFERENCE.md");
    assert!(
        !legacy_api_reference.contains("POST /api/instances/{id}/measurement"),
        "historical API reference still advertises the nonexistent automation route"
    );

    let mcp_reference = read_repository_file("docs/reference/mcp-tools.md");
    assert!(
        !mcp_reference.contains("### `models_instances_measurement`"),
        "MCP reference still advertises the retired tool"
    );
}
