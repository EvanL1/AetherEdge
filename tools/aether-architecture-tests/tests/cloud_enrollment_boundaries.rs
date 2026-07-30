use std::fs;
use std::path::{Path, PathBuf};

const CLI_FORBIDDEN_IDENTITY_CAPABILITIES: &[&str] = &[
    "ClaimedGatewayIdentitySource",
    "ClaimedGatewayIdentity",
    "FileClaimedGatewayIdentitySource",
    "GatewayPrivateKeySeed",
    "GatewaySessionAuthenticator",
    "rumqttc",
];

#[test]
fn enrollment_cli_cannot_read_private_identity_or_own_a_cloudlink_session() {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("architecture-test crate must be under the workspace");
    let cli_source = workspace.join("tools/aether/src");
    let mut violations = Vec::new();

    for source in rust_sources(&cli_source) {
        let text = fs::read_to_string(&source)
            .unwrap_or_else(|error| panic!("failed to read {}: {error}", source.display()));
        for capability in CLI_FORBIDDEN_IDENTITY_CAPABILITIES {
            if text.contains(capability) {
                violations.push(format!(
                    "{} references uplink-only capability {capability}",
                    source.strip_prefix(workspace).unwrap_or(&source).display()
                ));
            }
        }
    }

    assert!(
        violations.is_empty(),
        "the one-shot enrollment CLI may write enrollment state but must not read private identity or own CloudLink:\n{}",
        violations.join("\n")
    );
}

fn rust_sources(root: &Path) -> Vec<PathBuf> {
    let mut pending = vec![root.to_path_buf()];
    let mut sources = Vec::new();
    while let Some(directory) = pending.pop() {
        let entries = fs::read_dir(&directory)
            .unwrap_or_else(|error| panic!("failed to read {}: {error}", directory.display()));
        for entry in entries {
            let path = entry.expect("source directory entry").path();
            let file_type = fs::symlink_metadata(&path)
                .unwrap_or_else(|error| panic!("failed to inspect {}: {error}", path.display()))
                .file_type();
            assert!(
                !file_type.is_symlink(),
                "CLI source tree must not contain symlinks: {}",
                path.display()
            );
            if file_type.is_dir() {
                pending.push(path);
            } else if path.extension().is_some_and(|extension| extension == "rs") {
                sources.push(path);
            }
        }
    }
    sources
}
