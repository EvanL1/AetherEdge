# Machine-readable contracts

This directory is reserved for stable configuration, command, event, and MCP
schemas. Generated files must carry a generated header and are updated through
the repository generation task rather than edited manually.

During migration, Rust application types are the source of truth. Schema drift
will be checked before legacy HTTP payloads are removed.
