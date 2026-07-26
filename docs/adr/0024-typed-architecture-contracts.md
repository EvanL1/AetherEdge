# ADR-0024: Typed Architecture Contracts

- Status: Accepted
- Date: 2026-07-26

## Context

The repository architecture gate had accumulated package checks, source-text
regular expressions, exact file paths, documentation policy, Compose checks,
and installer tests in one large Bash process. A source file rename could
silently remove coverage, comments and test fixtures could trigger false
positives, and an undeclared host interpreter could stop the gate before later
contracts ran.

Some boundaries are stronger than any static source scan. Dependency direction
belongs in Cargo's package graph. Writer authority should be represented by a
capability that ordinary applications cannot obtain. Governed HTTP, CLI, and
MCP commands should be proved by behavior tests around the shared application
facade. A small residue, such as detecting direct mutation SQL in an adapter,
cannot currently be expressed by Rust visibility alone.

## Decision

1. Architecture enforcement follows this order:
   - Rust visibility and narrow capability/trait ownership;
   - behavior and conformance tests at application and adapter boundaries;
   - Cargo metadata assertions for package edges and composition direction;
   - structural `syn` inspection only for rules that cannot be represented by
     the first three mechanisms.
2. `AcquisitionStateWriter` moves out of the general application-port surface
   into `aether-acquisition-port`. `aether-shm-bridge` is its only direct
   runtime dependency owner, and IO receives the concrete writer from its
   composition root. Other services retain read-only SHM capabilities.
3. The architecture test crate discovers workspace packages and target roots
   through `cargo metadata`. Its source checker follows Rust module
   declarations from those target roots, honors `#[path]`, skips `cfg(test)`
   structurally, and fails closed on unreadable, missing, or invalid modules.
   It does not maintain a list of source filenames.
4. Governed channel, point-topology, instance, routing, rule, and safety
   boundaries are exercised as behavior tests. The former Bash fixture tests
   and exact handler-token counts are removed.
5. The published raw `ChannelPointManifest::slot` compatibility method remains
   deprecated. Its equivalence to typed `slot_for` is a behavior contract, and
   production use fails the warning-denying CI build.
6. `scripts/check-architecture.sh` is a thin orchestrator for Rust contracts,
   feature-resolved Cargo graph checks, and the domain `no_std` build. README
   wording, branding strings, and similar prose are not architecture gates.
   Deployment and installer contracts run independently via
   `scripts/check-distribution-contracts.sh`.

## Consequences

- Renaming or splitting a Rust source module cannot silently disable a package
  or source boundary; Cargo metadata or module traversal follows the change.
- Application crates do not gain acquisition-write authority merely by
  depending on the general `aether-ports` crate.
- Direct mutation SQL and retired compatibility symbols still require static
  inspection, but the inspection is AST-based, ignores comments, and has
  focused self-tests.
- Architecture checks no longer require Ruby, Python, Docker, or installer
  tooling. Distribution checks may require deployment tools, but they report
  under a separate contract and aggregate failures.
- Adding a new owner-only capability or an exception to dependency direction
  requires an explicit metadata contract and an ADR update.

## Verification

```bash
cargo test -p aether-architecture-tests \
  --test workspace_boundaries --test source_boundaries
cargo test -p aether-acquisition-port --test port_contract
cargo test -p aether-shm-bridge \
  --test acquisition_writer_contract --test channel_manifest_contract
./scripts/check-architecture.sh
```
