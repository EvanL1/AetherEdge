# AetherEdge Agent Instructions

This file is the canonical instruction source for coding agents working in this
repository. `CLAUDE.md` and `GEMINI.md` are symlinks to it, so every agent
reads and edits the same content. Any further tool-specific file may add usage
notes, but must not contradict this one.

## Product Direction

AetherEdge is an AI-native, industry-neutral IoT edge kernel and SDK. Energy
management is an optional domain pack, not a dependency of the core runtime.
The default distribution must run on one Linux edge host without Redis,
PostgreSQL, or any other external service. The default runtime is six Rust
processes and requires no browser application and no LLM. The optional
AetherEMS Console and Energy Pack live in
[`EvanL1/AetherEMS`](https://github.com/EvanL1/AetherEMS).

AetherIoT is the umbrella project name. This repository is the AetherEdge
product formerly named AetherIot. Preserve `aether-*`, `aether`, configuration,
installer, and protocol identifiers unless a separate compatibility decision
explicitly changes them.

## Repository Map

```text
crates/       domain, ports, application, SDK, Pack and testkit APIs
libs/         shared internal libraries (core, model, shm, config, sim)
extensions/   optional adapters chosen only by composition roots
services/     io, automation, history, api, uplink and alarm processes
tools/        aether CLI/MCP and the protocol simulator
examples/     minimal generic and compatibility composition proofs
packs/        Pack manifests
contracts/    pinned AetherContracts release consumed under ADR-0018
docs/         current concepts, guides, references and ADRs
ai/           generated agent catalog and the safety-policy authority
skills/       the repository-owned Agent Skill
firmware/     separately targeted embedded workspace
```

The unified documentation site source and deployment live in
[`EvanL1/AetherDocs`](https://github.com/EvanL1/AetherDocs).

Historical migration plans under `docs/plans/` and `docs/superpowers/` are
evidence of earlier decisions, not current architecture instructions. Current
authority is this file, accepted ADRs, the runtime manifest, OpenAPI, and the
active Pack manifests.

## Architecture Boundaries

Dependency direction is one-way:

```text
domain <- ports <- application <- runtime/interfaces
             ^
             +---- extensions
```

- Core crates under `crates/` must not depend on Redis, PostgreSQL, SQLx web
  frameworks, or concrete protocol implementations.
- Traits describe domain capabilities, never vendor command sets. Prefer
  `HistorySink` or `StateMirror` over a generic database/RTDB abstraction.
- Extensions under `extensions/` may implement core ports. Core crates must
  never depend on an extension.
- Only composition roots may choose concrete adapters.
- SHM is the authority for live point state. An external store may mirror it,
  but must never silently become the authority.
- Remote applications enter only through authenticated `aether-api:6005`. The
  internal IO, automation, history, uplink, and alarm ports stay on loopback.
- Application interfaces receive the read-only `LiveState` port. Only the
  acquisition/data-plane owner receives `LiveStateWriter`.
- AI, CLI, and HTTP interfaces use the same command/query application API.
  They must not write SHM or storage directly.

## AI Safety

- Every exposed capability declares whether it is a query or command, its risk
  level, required permission, idempotency, and confirmation policy.
- Device control is deny-by-default and always audited.
- AI is not part of hard real-time loops. Acquisition and safety behavior must
  remain deterministic when no AI client is connected.

## AI-native Documentation

- `ai/docs-manifest.json` is the generated, complete machine-readable catalog
  for agent-readable Edge repository material.
- Each catalog entry keeps a repository-local `path` for validation and an
  absolute `canonical_url` for retrieval. Published pages use the unified
  documentation site; internal Markdown uses GitHub; machine resources use
  Raw GitHub.
- `llms.txt` is generated from that catalog and must cover every catalog entry
  exactly once. Core task routes come first; ADRs, crates, extensions, plans,
  and other deep context remain discoverable under `Optional`.
- Update both generated files with
  `node scripts/build-agent-docs.mjs --write`; never edit them by hand.
- `ai/safety-policy.yaml` remains the capability-risk authority. Document
  metadata may reference its capability identifiers but must not redefine
  permission, confirmation, idempotency, or audit policy.
- Static documentation does not grant runtime authority. Runtime agents must
  query the live application capability catalog before any write.

## Key Documentation

These are the shortest paths to the most used pages. `llms.txt` and
`ai/docs-manifest.json` remain the complete catalog. `README.md` is a growth
surface and deliberately does not carry this index or a project status report.

- [Getting started](docs/guides/getting-started.md)
- [AI-native platform](docs/overview/ai-native-platform.md)
- [Build applications with AI](docs/guides/build-applications-with-ai.md)
- [Connect AI assistants](docs/guides/ai-assistants.md)
- [Connect devices](docs/guides/connect-devices.md)
- [Connect Home Assistant](docs/guides/home-assistant.md)
- [HTTP API and Swagger](docs/reference/http-api.md)
- [Deployment](docs/guides/deployment.md)
- [Platform status and roadmap](docs/roadmap/status.md)
- [Architecture](ARCHITECTURE.md)

## Rust Conventions

- Rust edition 2024; keep the pinned toolchain in `rust-toolchain.toml`.
- `mod.rs` files are forbidden.
- Library code returns typed errors; do not panic for recoverable failures.
- Avoid `unwrap` and `expect` in runtime library and binary code.
- Preserve no-std compatibility in the domain layer where practical.
- Write behavior tests before implementation and add conformance tests for
  every new port implementation.

## Verification

Local verification is risk-proportional. Always run the narrowest affected
check first, and stop once the changed behavior is covered:

- Documentation or ADR-only changes: validate the affected links, numbering,
  and documentation checks. Do not run Cargo commands.
- CI, YAML, or shell-only changes: parse or lint the affected files and run
  the directly related script tests. Do not compile the Rust workspace.
- A single Rust crate: run formatting plus that crate's focused Clippy and
  tests. Include direct dependants only when a public contract changed.
- Cross-crate architecture, dependency direction, composition roots, or live
  state authority: run the affected package tests and
  `./scripts/check-architecture.sh`.
- External-service tests remain opt-in and must be explicitly marked.

Full-workspace verification is owned by pull-request CI. Do not run the full
workspace suite locally by default. CI is responsible for:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --lib --bins
./scripts/check-architecture.sh
```

Run that full suite locally only when the user explicitly requests it, when
cutting a release, or when PR CI is unavailable and the change spans the
workspace. After pushing, inspect CI once. Do not continuously poll successful
CI runs; retrieve detailed logs only for failures or when the user asks.

## Change Discipline

- Do not mix frontend work into edge-kernel changes.
- Do not edit generated files; regenerate them through the documented command.
- Record changes to dependency direction or data authority as an ADR.
- Keep compatibility shims during staged migration and state their removal
  criteria in the relevant ADR.

## Cursor Cloud specific instructions

Durable, non-obvious notes for agents in the Cursor Cloud VM. The startup
update script runs `cargo fetch`; the Rust toolchain (`1.90.0`) auto-installs
from `rust-toolchain.toml` on first build. Standard lint/test/build commands
live in the `## Verification` section above — use those, not new ones.

- This product is headless by design: no browser UI ships here. Verify it by
  running services and reading data over HTTP, not through a web app. Standard
  bring-up is in [Getting started](docs/guides/getting-started.md).
- The default runtime is SHM-authoritative and needs **no Redis and no
  Postgres**. `scripts/ci-e2e-test.sh` is a legacy Redis-based demo; do not use
  it for a live check. Redis, `socat`, and `cargo-nextest` are not installed
  (tests fall back to `cargo test`).
- Hardware-free end-to-end (no Docker): build `simulator aether-io aether-api
  aether`, then run against the ready-made `config.e2e` (4 Modbus TCP channels
  on ports 5020-5023 matching `tools/simulator/scenarios/e2e_{pv,battery,diesel,load}.yaml`).
- GOTCHA: `config.e2e`'s per-channel point/mapping CSVs are **git-ignored and
  not committed** — only `config.e2e/io/io.yaml` is. Without them a synced
  channel loads **0 points** and never acquires (status `running:false`,
  `read_count:0`). Regenerate them first with
  `python3 scripts/generate-e2e-config.py` (PyYAML is present), then
  `aether init` + `aether sync --confirmed --config-path config.e2e --db-path <dir> --force`.
- Running services from source (not via `aether services start`/Docker): each
  service reads `AETHER_DB_PATH=<db-dir>/aether.db`. Set `AETHER_LOG_DIR` to a
  writable dir — the default per-channel log path is `/app/logs`, which is not
  writable in the VM (errors are noisy but non-fatal). `aether-io` and
  `aether-api` require `JWT_SECRET_KEY` (>=32 bytes); `aether-api` also requires
  `AETHER_BOOTSTRAP_ADMIN_PASSWORD` (>=16 chars, no common default) on first
  start to create the `admin` user.
- Authenticated reads: `POST /api/v1/auth/login` expects the **hex MD5 digest**
  of the password (not plaintext); tokens expire in 30 min. `aether-api:6005`
  proxies internal services at `/api/v1/{io,automation,history,uplink,alarm}/*`
  and reads the same SHM segment `aether-io` writes.
