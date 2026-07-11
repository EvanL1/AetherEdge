//! Embedded documentation resources served over MCP `resources/*`.
//!
//! Content is compiled into the binary so a deployed edge device serves docs
//! that always match the tool set it ships with. The catalog is the curated
//! subset an already-connected assistant needs: domain know-how, concepts,
//! the tool reference, and the assistant-setup guide. Deployment/getting-
//! started guides are deliberately absent.
//!
//! Each `include_str!` path below is relative to this file; moving either
//! this module or the referenced doc breaks the build (a compile error, not
//! a silent runtime gap).

pub(crate) struct DocResource {
    pub uri: &'static str,
    pub body: &'static str,
}

pub(crate) const DOC_RESOURCES: &[DocResource] = &[
    DocResource {
        uri: "aether://docs/domain/ess-primer",
        body: include_str!("../../../docs/domain/ess-primer.md"),
    },
    DocResource {
        uri: "aether://docs/domain/product-models",
        body: include_str!("../../../docs/domain/product-models.md"),
    },
    DocResource {
        uri: "aether://docs/domain/control-strategies",
        body: include_str!("../../../docs/domain/control-strategies.md"),
    },
    DocResource {
        uri: "aether://docs/domain/safe-operations",
        body: include_str!("../../../docs/domain/safe-operations.md"),
    },
    DocResource {
        uri: "aether://docs/concepts/architecture",
        body: include_str!("../../../docs/concepts/architecture.md"),
    },
    DocResource {
        uri: "aether://docs/concepts/data-model",
        body: include_str!("../../../docs/concepts/data-model.md"),
    },
    DocResource {
        uri: "aether://docs/concepts/shared-memory",
        body: include_str!("../../../docs/concepts/shared-memory.md"),
    },
    DocResource {
        uri: "aether://docs/concepts/rule-engine",
        body: include_str!("../../../docs/concepts/rule-engine.md"),
    },
    DocResource {
        uri: "aether://docs/concepts/data-flow",
        body: include_str!("../../../docs/concepts/data-flow.md"),
    },
    DocResource {
        uri: "aether://docs/guides/ai-assistants",
        body: include_str!("../../../docs/guides/ai-assistants.md"),
    },
    DocResource {
        uri: "aether://docs/reference/mcp-tools",
        body: include_str!("../../../docs/reference/mcp-tools.md"),
    },
];

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

    #[test]
    fn catalog_has_eleven_unique_doc_uris() {
        assert_eq!(DOC_RESOURCES.len(), 11);
        let mut uris: Vec<_> = DOC_RESOURCES.iter().map(|d| d.uri).collect();
        uris.sort_unstable();
        uris.dedup();
        assert_eq!(uris.len(), 11, "duplicate resource URIs");
        for d in DOC_RESOURCES {
            assert!(d.uri.starts_with("aether://docs/"), "bad uri {}", d.uri);
        }
    }

    #[test]
    fn every_embedded_doc_has_frontmatter_and_substance() {
        for d in DOC_RESOURCES {
            assert!(
                frontmatter_field(d.body, "title").is_some(),
                "{} missing frontmatter title",
                d.uri
            );
            assert!(
                frontmatter_field(d.body, "description").is_some(),
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
            resource_name("aether://docs/domain/ess-primer"),
            "ess-primer"
        );
    }

    /// Exercises the same lookup `read_resource` performs: find by exact
    /// URI, and return `None` for an unknown one (the not-found path).
    #[test]
    fn catalog_lookup_finds_known_uri_and_misses_unknown_one() {
        let known = DOC_RESOURCES[0].uri;
        let found = DOC_RESOURCES.iter().find(|d| d.uri == known);
        assert!(found.is_some());
        assert_eq!(found.unwrap().body, DOC_RESOURCES[0].body);

        let missing = DOC_RESOURCES
            .iter()
            .find(|d| d.uri == "aether://docs/does-not-exist");
        assert!(missing.is_none());
    }
}
