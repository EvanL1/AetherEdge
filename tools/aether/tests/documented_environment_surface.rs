//! Keeps the configuration reference level with the environment the services
//! actually read.
//!
//! An operator cannot discover a variable that exists only in a `env::var` call
//! somewhere in the workspace. This walks the production sources, collects every
//! variable name they read, and requires each one to appear in the reference —
//! so adding a knob without documenting it fails here rather than in a
//! deployment that cannot be configured.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

const REFERENCE: &str = "docs/reference/configuration.md";

/// Variables the reference deliberately omits, with the reason.
const NOT_OPERATOR_FACING: &[(&str, &str)] = &[
    (
        "AETHER_JSON",
        "CLI output mode; documented as the --json flag",
    ),
    (
        "SKIP_VALIDATION",
        "development escape hatch, not a deployment knob",
    ),
    ("HOSTNAME", "provided by the operating system"),
    (
        "AETHER_TEST_PG_DSN",
        "opts a developer into the ignored PostgreSQL integration tests; never read by a running service",
    ),
    (
        "AETHER_TEST_TSDB_DSN",
        "opts a developer into the ignored TimescaleDB integration tests; never read by a running service",
    ),
    ("CARGO_TARGET_TMPDIR", "provided by cargo during tests"),
];

fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Rust sources under the production directories of every workspace member.
fn production_sources(root: &Path) -> Vec<PathBuf> {
    let mut sources = Vec::new();
    for area in ["services", "libs", "crates", "tools"] {
        let Ok(members) = std::fs::read_dir(root.join(area)) else {
            continue;
        };
        for member in members.filter_map(Result::ok) {
            collect_rust_files(&member.path().join("src"), &mut sources);
        }
    }
    sources
}

fn collect_rust_files(directory: &Path, sources: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        if path.is_dir() {
            collect_rust_files(&path, sources);
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            sources.push(path);
        }
    }
}

/// Every `SCREAMING_SNAKE` name passed to `env::var` or `env_or`.
fn variables_read_by(source: &str) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    for marker in ["env::var(\"", "env_or(\""] {
        let mut rest = source;
        while let Some(start) = rest.find(marker) {
            rest = &rest[start + marker.len()..];
            let Some(end) = rest.find('"') else { break };
            let name = &rest[..end];
            if !name.is_empty()
                && name.chars().all(|character| {
                    character.is_ascii_uppercase() || "0123456789_".contains(character)
                })
                && name.chars().next().is_some_and(|c| c.is_ascii_uppercase())
            {
                names.insert(name.to_string());
            }
        }
    }
    names
}

#[test]
fn every_environment_variable_the_services_read_is_documented() {
    let root = repository_root();
    let reference = std::fs::read_to_string(root.join(REFERENCE))
        .unwrap_or_else(|error| panic!("failed to read {REFERENCE}: {error}"));

    let exempt: BTreeSet<&str> = NOT_OPERATOR_FACING.iter().map(|(name, _)| *name).collect();

    let mut undocumented = Vec::new();
    for path in production_sources(&root) {
        let Ok(source) = std::fs::read_to_string(&path) else {
            continue;
        };
        for name in variables_read_by(&source) {
            if exempt.contains(name.as_str()) || reference.contains(&format!("`{name}`")) {
                continue;
            }
            undocumented.push(name);
        }
    }
    undocumented.sort();
    undocumented.dedup();

    assert!(
        undocumented.is_empty(),
        "these environment variables are read by the services but absent from {REFERENCE}:\n  {}\n\n\
         Document them there, or add them to NOT_OPERATOR_FACING in this test with the reason.",
        undocumented.join("\n  ")
    );
}
