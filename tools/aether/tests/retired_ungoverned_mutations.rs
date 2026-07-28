//! Prevents CLI-only compatibility mutations from bypassing the governed
//! application capability, revision, confirmation, and audit contracts.

use std::path::{Path, PathBuf};
use std::process::Command;

fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

#[test]
fn cli_rejects_retired_ungoverned_mutation_subcommands() {
    let retired: &[&[&str]] = &[
        &["channels", "write"],
        &["channels", "reload"],
        &["channels", "points", "add"],
        &["channels", "points", "update"],
        &["channels", "points", "remove"],
        &["channels", "points", "batch"],
        &["models", "instances", "create"],
        &["models", "instances", "update"],
        &["models", "instances", "delete"],
        &["routing", "create"],
        &["routing", "batch"],
        &["routing", "delete-instance"],
        &["routing", "delete-channel"],
        &["templates", "list"],
        &["models", "products", "available"],
        &["templates", "get"],
        &["templates", "snapshot"],
        &["templates", "apply"],
        &["templates", "delete"],
        &["net", "mqtt", "config-set"],
        &["net", "mqtt", "reconnect"],
        &["net", "mqtt", "disconnect"],
        &["net", "cert", "upload"],
        &["net", "cert", "delete"],
        &["setup"],
        &["services", "status"],
        &["logs", "list"],
        &["doctor"],
        &["top"],
        &["shm", "get", "ch:1:T:1"],
        &["shm", "info"],
        &["shm", "watch", "ch:1:T:1"],
        &["shm", "top"],
    ];

    for arguments in retired {
        let output = Command::new(env!("CARGO_BIN_EXE_aether"))
            .args(*arguments)
            .output()
            .expect("run aether CLI");
        assert!(
            !output.status.success(),
            "retired command accepted: {arguments:?}"
        );
        let stderr = String::from_utf8(output.stderr).expect("CLI stderr is UTF-8");
        assert!(
            stderr.contains("unrecognized subcommand"),
            "retired command did not fail at clap boundary: {arguments:?}: {stderr}"
        );
    }
}

#[test]
fn source_and_reference_docs_keep_ungoverned_wrappers_retired() {
    let root = repository_root();
    for (relative, forbidden) in [
        ("tools/aether/src/channels.rs", "fn write_point"),
        ("tools/aether/src/channels.rs", "fn points_batch"),
        ("tools/aether/src/models/client.rs", "fn create_instance"),
        ("tools/aether/src/models/client.rs", "fn update_instance"),
        ("tools/aether/src/models/client.rs", "fn delete_instance"),
        ("tools/aether/src/routing.rs", "fn create_routing"),
        ("tools/aether/src/routing.rs", "fn batch_routing"),
        ("tools/aether/src/net.rs", "fn mqtt_config_set"),
        ("tools/aether/src/net.rs", "fn cert_upload"),
        ("tools/aether/src/mcp.rs", "fn channels_write"),
        ("tools/aether/src/mcp.rs", "legacy_write_test_router"),
    ] {
        let source = std::fs::read_to_string(root.join(relative))
            .unwrap_or_else(|error| panic!("failed to read {relative}: {error}"));
        assert!(
            !source.contains(forbidden),
            "{relative} restored retired compatibility symbol {forbidden}"
        );
    }

    let reference =
        std::fs::read_to_string(root.join("docs/reference/cli.md")).expect("read CLI reference");
    for heading in [
        "### channels write",
        "### models instances create",
        "### routing create",
        "### templates apply",
        "### net mqtt config-set",
        "### net cert upload",
    ] {
        assert!(
            !reference.contains(heading),
            "CLI reference restored {heading}"
        );
    }
}
