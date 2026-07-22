# Contributing to Aether

Thank you for helping improve Aether. Aether is an AI-native,
industry-neutral IoT edge kernel and SDK. Its default runtime is a set of
independently restartable processes that share live point state through SHM;
Redis, PostgreSQL, and other external services are optional extensions.

By participating in this project, you agree to follow the
[Code of Conduct](CODE_OF_CONDUCT.md).

## Before you start

- Search existing issues and pull requests before opening a duplicate.
- Use a bug report for reproducible defects and a feature request for new
  behavior. Use the channels described in [SUPPORT.md](SUPPORT.md) for usage
  questions.
- Do not open a public issue for a vulnerability. Follow
  [SECURITY.md](SECURITY.md) instead.
- For a large change, open an issue before implementation so its boundaries
  can be reviewed. Changes to dependency direction, live-state authority,
  process isolation, or AI command safety require an ADR under `docs/adr/`.

## Architecture contract

Contributions must preserve these invariants:

```text
domain <- ports <- application <- runtime/interfaces
             ^
             +---- extensions
```

- The default build and runtime must work on one Linux edge host without
  Redis, PostgreSQL, or another external service.
- SHM is authoritative for live point state. An external store may mirror or
  persist state, but must not silently become the real-time authority.
- Only the acquisition/data-plane owner may write acquired live state.
  Application, HTTP, CLI, and AI interfaces use the shared command/query API.
- Production processes remain isolated and independently restartable. Do not
  collapse them merely to avoid defining an SHM or event-plane boundary.
- Core crates depend on capability ports, not database vendors, web
  frameworks, or concrete device protocols. Optional integrations belong
  under `extensions/` and core crates never depend on them.
- AI is outside hard real-time and safety loops. Device control is
  deny-by-default, permission checked, confirmation aware, and audited.
- Keep energy-specific concepts in an optional pack or adapter unless they are
  genuinely industry-neutral.

The complete rules are in [AGENTS.md](AGENTS.md),
[ARCHITECTURE.md](ARCHITECTURE.md), and the accepted ADRs in `docs/adr/`.

## Development workflow

1. Fork the repository and create a focused branch from the target branch.
2. Add or update a behavior test before changing implementation. New port
   implementations need conformance tests.
3. Make the smallest coherent change. Do not mix frontend work into an edge
   kernel change or perform unrelated directory rewrites.
4. Update public documentation and an ADR when the contract changes.
5. Run the narrowest affected test, then the repository checks below.

The pinned Rust toolchain is declared in `rust-toolchain.toml`. Initialize
submodules after cloning:

```bash
git submodule update --init --recursive
```

For the full local gate, run:

```bash
./scripts/quick-check.sh
```

During the edit loop, prefer the focused form for the package you changed:

```bash
cargo fmt --all -- --check
cargo clippy -p <affected-package> --all-targets --all-features -- -D warnings
cargo test -p <affected-package>
```

`quick-check.sh` is an optional local release gate, not the default edit loop;
the complete workspace matrix is enforced by pull-request CI.

The equivalent required checks used by the repository are:

```bash
cargo fmt --all -- --check
./scripts/check-architecture.sh
cargo check --workspace
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo clippy --workspace --lib --bins -- \
  -D clippy::unwrap_used -D clippy::expect_used
cargo test --workspace --lib --bins
./scripts/check-openapi-contracts.sh
```

`cargo-nextest` is optional; `scripts/quick-check.sh` uses it when installed
and otherwise falls back to `cargo test`. Tests requiring an external service
must be clearly marked and kept out of the default verification path. Run an
extension-specific suite only when your change affects that extension.

Rust code uses edition 2024. `mod.rs` files are forbidden. Runtime libraries
and binaries return errors for recoverable failures and must not introduce
`unwrap` or `expect` into production paths.

## Pull requests

Keep each pull request reviewable and explain:

- the problem and user-visible outcome;
- the affected architectural layer and process boundary;
- whether live-state authority, external dependencies, device control, or AI
  permissions change;
- tests and commands run, including any check not run and why;
- documentation, migration notes, and ADR changes when applicable.

Pull requests must pass required CI and receive maintainer review before
merge. Reviewers may ask for a smaller scope, additional tests, or an ADR when
a change creates a long-lived architectural decision.

## Licensing

Unless you explicitly state otherwise, a contribution intentionally submitted
for inclusion in Aether is licensed under the repository's
`MIT OR Apache-2.0` terms, consistent with [LICENSE-MIT](LICENSE-MIT) and
[LICENSE-APACHE](LICENSE-APACHE).
