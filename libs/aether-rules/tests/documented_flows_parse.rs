//! Holds the rule-authoring guide to the parser that actually runs.
//!
//! Every JSON block in `docs/guides/writing-rules.md` is extracted and fed to
//! `extract_rule_flow`. A block that the service would reject fails this test,
//! so the guide cannot document a flow shape the runtime does not accept — the
//! failure that sent one evaluation through eight structural variants before
//! giving up, and that left the shipped pack example unloadable.

use std::path::{Path, PathBuf};

use aether_rules::extract_rule_flow;
use serde_json::Value;

const GUIDE: &str = "docs/guides/writing-rules.md";

fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Every ```json fenced block in a Markdown file, in document order.
fn json_blocks(markdown: &str) -> Vec<String> {
    let mut blocks = Vec::new();
    let mut current: Option<String> = None;
    for line in markdown.lines() {
        match current.as_mut() {
            None if line.trim_start().starts_with("```json") => current = Some(String::new()),
            None => {},
            Some(_) if line.trim_start().starts_with("```") => {
                blocks.push(current.take().unwrap_or_default());
            },
            Some(block) => {
                block.push_str(line);
                block.push('\n');
            },
        }
    }
    blocks
}

/// The flow document inside a block, whether the block is a bare flow or the
/// request body that carries one.
fn flow_of(block: &Value) -> Option<&Value> {
    if block.get("nodes").is_some() {
        return Some(block);
    }
    block
        .get("flow_json")
        .filter(|flow| flow.get("nodes").is_some())
}

#[test]
fn every_documented_flow_parses_through_the_runtime_parser() {
    let path = repository_root().join(GUIDE);
    let markdown = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display()));

    let blocks = json_blocks(&markdown);
    assert!(
        !blocks.is_empty(),
        "{GUIDE} has no JSON blocks; the extraction above is probably broken"
    );

    let mut flows_checked = 0;
    for (index, block) in blocks.iter().enumerate() {
        let parsed: Value = serde_json::from_str(block).unwrap_or_else(|error| {
            panic!(
                "{GUIDE} JSON block #{} is not valid JSON: {error}\n{block}",
                index + 1
            )
        });

        let Some(flow) = flow_of(&parsed) else {
            continue;
        };

        extract_rule_flow(flow).unwrap_or_else(|error| {
            panic!(
                "{GUIDE} JSON block #{} documents a flow the service would reject: {error}\n{block}",
                index + 1
            )
        });
        flows_checked += 1;
    }

    assert!(
        flows_checked >= 2,
        "expected the guide to document both a minimal flow and a working one, \
         found {flows_checked}"
    );
}
