//! Documentation resources served over MCP `resources/*`.
//!
//! Generic kernel documentation remains embedded so it matches the binary.
//! Domain knowledge is read only from manifest-validated active Pack roots at
//! startup; an inactive Pack contributes no resource URI or content.

use std::collections::BTreeSet;
use std::path::PathBuf;

use aether_pack::ActivePackSet;
use thiserror::Error;

#[derive(Debug, Clone)]
pub(crate) struct DocResource {
    pub uri: String,
    pub body: String,
}

struct EmbeddedDocResource {
    uri: &'static str,
    body: &'static str,
}

const KERNEL_DOC_RESOURCES: &[EmbeddedDocResource] = &[
    EmbeddedDocResource {
        uri: "aether://docs/concepts/architecture",
        body: include_str!("../../../docs/concepts/architecture.md"),
    },
    EmbeddedDocResource {
        uri: "aether://docs/concepts/data-model",
        body: include_str!("../../../docs/concepts/data-model.md"),
    },
    EmbeddedDocResource {
        uri: "aether://docs/concepts/shared-memory",
        body: include_str!("../../../docs/concepts/shared-memory.md"),
    },
    EmbeddedDocResource {
        uri: "aether://docs/concepts/rule-engine",
        body: include_str!("../../../docs/concepts/rule-engine.md"),
    },
    EmbeddedDocResource {
        uri: "aether://docs/concepts/data-flow",
        body: include_str!("../../../docs/concepts/data-flow.md"),
    },
    EmbeddedDocResource {
        uri: "aether://docs/guides/ai-assistants",
        body: include_str!("../../../docs/guides/ai-assistants.md"),
    },
    EmbeddedDocResource {
        uri: "aether://docs/reference/mcp-tools",
        body: include_str!("../../../docs/reference/mcp-tools.md"),
    },
];

const MAX_KNOWLEDGE_BYTES: usize = 1024 * 1024;

#[derive(Debug, Error)]
pub(crate) enum DocResourceError {
    #[error("cannot enumerate knowledge for active Pack {pack} at {path}: {source}")]
    DirectoryRead {
        pack: String,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("active Pack {pack} knowledge path {path} escapes {root}")]
    EscapedKnowledge {
        pack: String,
        path: PathBuf,
        root: PathBuf,
    },
    #[error("active Pack {pack} knowledge symlink is forbidden: {path}")]
    SymlinkKnowledge { pack: String, path: PathBuf },
    #[error("active Pack {pack} knowledge must be a regular file: {path}")]
    NonRegularKnowledge { pack: String, path: PathBuf },
    #[error("active Pack {pack} knowledge exceeds {limit} bytes ({size} bytes): {path}")]
    KnowledgeTooLarge {
        pack: String,
        path: PathBuf,
        size: u64,
        limit: usize,
    },
    #[error("cannot read active Pack {pack} knowledge {path}: {source}")]
    KnowledgeRead {
        pack: String,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("active Pack {pack} knowledge filename is not a valid URI segment: {path}")]
    InvalidKnowledgeName { pack: String, path: PathBuf },
    #[error("active Pack {pack} knowledge {path} lacks title/description frontmatter")]
    InvalidKnowledgeMetadata { pack: String, path: PathBuf },
    #[error("MCP documentation URI {uri} is duplicated")]
    DuplicateUri { uri: String },
}

pub(crate) fn doc_resources(
    active_packs: &ActivePackSet,
) -> Result<Vec<DocResource>, DocResourceError> {
    let mut resources = KERNEL_DOC_RESOURCES
        .iter()
        .map(|resource| DocResource {
            uri: resource.uri.to_string(),
            body: resource.body.to_string(),
        })
        .collect::<Vec<_>>();
    let mut uris = resources
        .iter()
        .map(|resource| resource.uri.clone())
        .collect::<BTreeSet<_>>();

    for pack in active_packs.iter() {
        let Some(knowledge_directory) = pack.asset_directory("knowledge") else {
            continue;
        };
        let canonical_directory =
            std::fs::canonicalize(&knowledge_directory).map_err(|source| {
                DocResourceError::DirectoryRead {
                    pack: pack.id().to_string(),
                    path: knowledge_directory.clone(),
                    source,
                }
            })?;
        let entries = std::fs::read_dir(&canonical_directory).map_err(|source| {
            DocResourceError::DirectoryRead {
                pack: pack.id().to_string(),
                path: canonical_directory.clone(),
                source,
            }
        })?;
        let mut paths = entries
            .map(|entry| entry.map(|value| value.path()))
            .collect::<std::io::Result<Vec<_>>>()
            .map_err(|source| DocResourceError::DirectoryRead {
                pack: pack.id().to_string(),
                path: canonical_directory.clone(),
                source,
            })?;
        paths.sort_unstable();

        for path in paths {
            let Some(file_name) = path.file_name().and_then(|value| value.to_str()) else {
                return Err(DocResourceError::InvalidKnowledgeName {
                    pack: pack.id().to_string(),
                    path,
                });
            };
            if PathBuf::from(file_name)
                .extension()
                .and_then(|extension| extension.to_str())
                != Some("md")
            {
                continue;
            }
            let metadata = std::fs::symlink_metadata(&path).map_err(|source| {
                DocResourceError::KnowledgeRead {
                    pack: pack.id().to_string(),
                    path: path.clone(),
                    source,
                }
            })?;
            if metadata.file_type().is_symlink() {
                return Err(DocResourceError::SymlinkKnowledge {
                    pack: pack.id().to_string(),
                    path,
                });
            }
            if !metadata.file_type().is_file() {
                return Err(DocResourceError::NonRegularKnowledge {
                    pack: pack.id().to_string(),
                    path,
                });
            }
            if metadata.len() > MAX_KNOWLEDGE_BYTES as u64 {
                return Err(DocResourceError::KnowledgeTooLarge {
                    pack: pack.id().to_string(),
                    path,
                    size: metadata.len(),
                    limit: MAX_KNOWLEDGE_BYTES,
                });
            }
            let resolved =
                std::fs::canonicalize(&path).map_err(|source| DocResourceError::KnowledgeRead {
                    pack: pack.id().to_string(),
                    path: path.clone(),
                    source,
                })?;
            if !resolved.starts_with(&canonical_directory) || !resolved.starts_with(pack.root()) {
                return Err(DocResourceError::EscapedKnowledge {
                    pack: pack.id().to_string(),
                    path: resolved,
                    root: canonical_directory,
                });
            }
            let Some(stem) = PathBuf::from(file_name)
                .file_stem()
                .and_then(|value| value.to_str())
                .map(str::to_string)
            else {
                return Err(DocResourceError::InvalidKnowledgeName {
                    pack: pack.id().to_string(),
                    path: resolved,
                });
            };
            if !valid_uri_segment(&stem) {
                return Err(DocResourceError::InvalidKnowledgeName {
                    pack: pack.id().to_string(),
                    path: resolved,
                });
            }
            let body = std::fs::read_to_string(&resolved).map_err(|source| {
                DocResourceError::KnowledgeRead {
                    pack: pack.id().to_string(),
                    path: resolved.clone(),
                    source,
                }
            })?;
            if frontmatter_field(&body, "title").is_none()
                || frontmatter_field(&body, "description").is_none()
            {
                return Err(DocResourceError::InvalidKnowledgeMetadata {
                    pack: pack.id().to_string(),
                    path: resolved,
                });
            }
            let uri = format!("aether://packs/{}/knowledge/{stem}", pack.id());
            if !uris.insert(uri.clone()) {
                return Err(DocResourceError::DuplicateUri { uri });
            }
            resources.push(DocResource { uri, body });
        }
    }
    Ok(resources)
}

fn valid_uri_segment(value: &str) -> bool {
    value
        .bytes()
        .next()
        .is_some_and(|byte| byte.is_ascii_alphanumeric())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

/// Extract a scalar field from the leading YAML frontmatter block
/// (`---\nkey: value\n...\n---`). Returns `None` when there is no
/// frontmatter or the key is absent.
///
/// Purpose-built for the embedded docs corpus, not a general YAML parser:
/// it looks for the first `\n---` after an opening `---` and matches `key`
/// only at the start of a line, so it is safe against `key`-prefixed longer
/// keys (`titles:`) and colons inside values, but it does not handle nested
/// structures, quoting, or multiple `---` blocks.
pub(crate) fn frontmatter_field<'a>(body: &'a str, key: &str) -> Option<&'a str> {
    let rest = body.strip_prefix("---")?;
    let end = rest.find("\n---")?;
    for line in rest[..end].lines() {
        if let Some(value) = line.strip_prefix(key).and_then(|v| v.strip_prefix(':')) {
            return Some(value.trim());
        }
    }
    None
}

/// Programmatic resource name: the last URI path segment.
pub(crate) fn resource_name(uri: &str) -> &str {
    uri.rsplit('/').next().unwrap_or(uri)
}

#[cfg(test)]
mod tests {
    use super::*;
    use aether_pack::{PackRuntime, load_active_packs};
    use std::fs;
    use std::path::Path;

    const TEST_PACK_MANIFEST: &str = r#"schema_version: 1
id: test-pack
name: Test Pack
version: 0.5.0
status: test
description: MCP knowledge test Pack
distribution:
  id: test-distribution
  version: 0.5.0
  composition: test-gateway
compatibility:
  aether: ">=0.5.0,<0.6.0"
  required_capabilities: []
  required_protocols: []
assets:
  knowledge: knowledge
examples:
  commissioned: false
capabilities: {}
"#;

    fn runtime() -> PackRuntime {
        aether_runtime_catalog::KernelRuntimeManifest::from_io_features(
            env!("CARGO_PKG_VERSION"),
            "aarch64-unknown-linux-musl",
            ["can", "gpio", "http", "modbus", "mqtt"],
        )
        .and_then(|manifest| manifest.pack_runtime())
        .expect("explicit MCP documentation test composition")
    }

    fn write_global(config: &Path, packs: serde_json::Value) {
        fs::create_dir_all(config).expect("create config directory");
        let source = serde_yml::to_string(&serde_json::json!({ "packs": packs }))
            .expect("serialize active pack config");
        fs::write(config.join("global.yaml"), source).expect("write global config");
    }

    #[test]
    fn kernel_catalog_has_only_seven_generic_doc_uris() {
        let resources = doc_resources(&aether_pack::ActivePackSet::empty())
            .expect("generic documentation catalog");
        assert_eq!(resources.len(), 7);
        let mut uris: Vec<_> = resources.iter().map(|d| d.uri.as_str()).collect();
        uris.sort_unstable();
        uris.dedup();
        assert_eq!(uris.len(), 7, "duplicate resource URIs");
        for d in &resources {
            assert!(d.uri.starts_with("aether://docs/"), "bad uri {}", d.uri);
        }
        assert!(!uris.contains(&"aether://packs/energy/knowledge/ess-primer"));
    }

    #[test]
    fn every_generic_doc_has_frontmatter_and_substance() {
        let resources = doc_resources(&aether_pack::ActivePackSet::empty())
            .expect("generic documentation catalog");
        for d in &resources {
            assert!(
                frontmatter_field(&d.body, "title").is_some(),
                "{} missing frontmatter title",
                d.uri
            );
            assert!(
                frontmatter_field(&d.body, "description").is_some(),
                "{} missing frontmatter description",
                d.uri
            );
            assert!(d.body.len() > 500, "{} is suspiciously short", d.uri);
        }
    }

    #[test]
    fn frontmatter_field_parses_and_rejects() {
        let body = "---\ntitle: Hello\ndescription: World thing\n---\n# Body";
        assert_eq!(frontmatter_field(body, "title"), Some("Hello"));
        assert_eq!(frontmatter_field(body, "description"), Some("World thing"));
        assert_eq!(frontmatter_field(body, "updated"), None);
        assert_eq!(frontmatter_field("# no frontmatter", "title"), None);
        // a key appearing in the body, not the frontmatter, must not match
        assert_eq!(
            frontmatter_field("---\na: b\n---\ntitle: sneaky", "title"),
            None
        );
    }

    #[test]
    fn resource_name_is_last_segment() {
        assert_eq!(
            resource_name("aether://packs/energy/knowledge/ess-primer"),
            "ess-primer"
        );
    }

    #[test]
    fn activated_energy_pack_adds_namespaced_pack_owned_knowledge_at_runtime() {
        let config = tempfile::tempdir().expect("config directory");
        let energy =
            fs::canonicalize(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packs/energy"))
                .expect("canonical repository energy pack");
        write_global(
            config.path(),
            serde_json::json!([{ "id": "energy", "root": energy }]),
        );
        let active = load_active_packs(config.path(), &runtime()).expect("validated energy pack");

        let resources = doc_resources(&active).expect("dynamic documentation catalog");

        assert_eq!(resources.len(), 12);
        assert!(
            resources
                .iter()
                .any(|doc| doc.uri == "aether://packs/energy/knowledge/ess-primer")
        );
        assert!(
            resources
                .iter()
                .find(|doc| doc.uri == "aether://packs/energy/knowledge/product-models")
                .is_some_and(|doc| doc.body.contains("owns 13 products"))
        );
    }

    #[cfg(unix)]
    #[test]
    fn active_pack_knowledge_rejects_symlinked_markdown() {
        use std::os::unix::fs::symlink;

        let site = tempfile::tempdir().expect("site root");
        let config = site.path().join("config");
        let pack = site.path().join("pack");
        let outside = site.path().join("outside.md");
        fs::create_dir_all(pack.join("knowledge")).expect("knowledge directory");
        fs::write(pack.join("pack.yaml"), TEST_PACK_MANIFEST).expect("manifest");
        fs::write(
            &outside,
            "---\ntitle: Escaped\ndescription: Escaped knowledge\n---\n# Escaped",
        )
        .expect("outside knowledge");
        symlink(&outside, pack.join("knowledge/escaped.md")).expect("knowledge symlink");
        write_global(
            &config,
            serde_json::json!([{ "id": "test-pack", "root": pack }]),
        );
        let active = load_active_packs(&config, &runtime()).expect("validated Pack directory");

        assert!(matches!(
            doc_resources(&active),
            Err(DocResourceError::SymlinkKnowledge { .. })
        ));
    }

    #[test]
    fn active_pack_knowledge_rejects_oversized_markdown() {
        let site = tempfile::tempdir().expect("site root");
        let config = site.path().join("config");
        let pack = site.path().join("pack");
        fs::create_dir_all(pack.join("knowledge")).expect("knowledge directory");
        fs::write(pack.join("pack.yaml"), TEST_PACK_MANIFEST).expect("manifest");
        fs::write(
            pack.join("knowledge/huge.md"),
            vec![b'x'; MAX_KNOWLEDGE_BYTES + 1],
        )
        .expect("oversized knowledge");
        write_global(
            &config,
            serde_json::json!([{ "id": "test-pack", "root": pack }]),
        );
        let active = load_active_packs(&config, &runtime()).expect("validated Pack directory");

        assert!(matches!(
            doc_resources(&active),
            Err(DocResourceError::KnowledgeTooLarge { .. })
        ));
    }

    #[test]
    fn active_pack_knowledge_rejects_invalid_names_and_non_regular_entries() {
        let site = tempfile::tempdir().expect("site root");
        let config = site.path().join("config");
        let pack = site.path().join("pack");
        fs::create_dir_all(pack.join("knowledge/bad-directory.md"))
            .expect("non-regular markdown entry");
        fs::write(pack.join("pack.yaml"), TEST_PACK_MANIFEST).expect("manifest");
        write_global(
            &config,
            serde_json::json!([{ "id": "test-pack", "root": pack }]),
        );
        let active = load_active_packs(&config, &runtime()).expect("validated Pack directory");

        assert!(matches!(
            doc_resources(&active),
            Err(DocResourceError::NonRegularKnowledge { .. })
        ));

        fs::remove_dir(pack.join("knowledge/bad-directory.md")).expect("remove directory entry");
        fs::write(
            pack.join("knowledge/bad name.md"),
            "---\ntitle: Bad\ndescription: Bad name\n---\n# Bad",
        )
        .expect("invalid knowledge name");
        assert!(matches!(
            doc_resources(&active),
            Err(DocResourceError::InvalidKnowledgeName { .. })
        ));
    }

    /// Exercises the same lookup `read_resource` performs: find by exact
    /// URI, and return `None` for an unknown one (the not-found path).
    #[test]
    fn catalog_lookup_finds_known_uri_and_misses_unknown_one() {
        let resources = doc_resources(&aether_pack::ActivePackSet::empty())
            .expect("generic documentation catalog");
        let known = resources[0].uri.as_str();
        let found = resources.iter().find(|d| d.uri == known);
        assert!(found.is_some());
        assert_eq!(found.unwrap().body, resources[0].body);

        let missing = resources
            .iter()
            .find(|d| d.uri == "aether://docs/does-not-exist");
        assert!(missing.is_none());
    }
}
