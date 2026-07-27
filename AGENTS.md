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
libs/         shared kernel implementation (SHM, local storage, config, sim)
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

Historical plans and scratch design material must stay outside GitHub. Preserve
accepted decisions in ADRs instead.

Current authority is this file, accepted ADRs, the runtime manifest, OpenAPI,
and the active Pack manifests.

## Architecture Boundaries

Dependency direction is one-way:

```text
domain <- ports <- application <- services/interfaces
                              ^
                              +---- composition-selected kernel adapters
```

- Core crates under `crates/` must not depend on Redis, PostgreSQL, SQLx web
  frameworks, or concrete protocol implementations. The kernel workspace ships
  no Redis client or mirror implementation; those belong downstream.
- `aether-domain` owns all industry-neutral business semantics. The retired
  `aether-model` compatibility crate must not be restored; wire, storage,
  protocol, and Pack DTOs stay in their owning adapters or contract crates.
- Traits describe domain capabilities, never vendor command sets. Prefer
  `HistorySink` or `StateMirror` over a generic database/RTDB abstraction.
- AetherEdge owns no in-tree `extensions/` layer. Optional third-party
  integrations live in downstream repositories and consume the published SDK,
  port, and testkit contracts.
- Only composition roots may choose concrete adapters; non-composition
  libraries must not bind application behavior to SHM, SQLite, HTTP, MQTT, or
  another concrete runtime implementation.
- SHM is the authority for live point state. An external store may mirror it,
  but must never silently become the authority.
- Remote applications enter only through authenticated `aether-api:6005`. The
  internal IO, automation, history, uplink, and alarm ports stay on loopback.
- Application interfaces receive the read-only `LiveState` port. Only the
  acquisition/data-plane owner receives `LiveStateWriter`.
- AI, CLI, and HTTP interfaces use the same command/query application API.
  They must not write SHM or storage directly.
- The production `aether-io` runtime is Rust-only and contains only explicitly
  composed, distribution-owned Modbus, MQTT, HTTP, raw CAN, GPIO, IEC 61850,
  and Aether-485 adapters. It must not load scripts, launch
  subprocesses, provide an in-process simulation protocol, or manage host
  networking. Python and protocol simulators are limited to tooling outside
  `services/`.
- Protocol-specific model catalogs such as SunSpec stay outside this kernel.
  A future downstream IO plugin must be pure Rust, statically composed through
  an accepted generic contract, and unable to write SHM or bypass governed
  commands directly.
- CloudLink is an Aether-native protocol. Only `aether-uplink` may own its
  transport, session, signing, spool, acknowledgement, and replay lifecycle.

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
  exactly once. Core task routes come first; ADRs, crates, libraries, service
  adapters, and other deep context remain discoverable under `Optional`.
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
- Distribution, installer, Compose, runtime-manifest, or Pack layout changes:
  run `./scripts/check-distribution-contracts.sh`.
- External-service tests remain opt-in and must be explicitly marked.

Full-workspace verification is owned by pull-request CI. Do not run the full
workspace suite locally by default. CI is responsible for:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo nextest run --workspace --lib --bins --tests
./scripts/check-architecture.sh
./scripts/check-distribution-contracts.sh
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
