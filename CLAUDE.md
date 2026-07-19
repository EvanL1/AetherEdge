# AetherEdge Claude Notes

Read [`AGENTS.md`](AGENTS.md) first. It is the canonical policy for product
direction, dependency boundaries, AI safety, Rust conventions, verification,
and change discipline. This file only records concise repository navigation
notes and must not override it.

## Current product boundary

- AetherEdge is a headless, industry-neutral IoT edge kernel and SDK.
- The default runtime is six Rust processes and requires no Redis,
  PostgreSQL, browser application, or LLM.
- SHM is authoritative for live point state. Optional stores are adapters or
  mirrors, never implicit authorities.
- Remote applications enter through authenticated `aether-api:6005`. The
  internal IO, automation, history, uplink, and alarm ports stay on loopback.
- The optional AetherEMS Console and Energy Pack live in
  [`EvanL1/AetherEMS`](https://github.com/EvanL1/AetherEMS).

## Repository map

```text
crates/       domain, ports, application, SDK, Pack and testkit APIs
extensions/   optional adapters chosen only by composition roots
services/     io, automation, history, api, uplink and alarm processes
tools/        aether CLI/MCP and simulator
examples/     minimal generic and compatibility composition proofs
docs/         current concepts, guides, references and ADRs
firmware/     separately targeted embedded workspace
```

The unified documentation site source and deployment live in
`EvanL1/AetherDocs`.

Historical migration plans under `docs/plans/` and `docs/superpowers/` are
evidence of earlier decisions, not current architecture instructions. Current
authority is `AGENTS.md`, accepted ADRs, the runtime manifest, OpenAPI, and the
active Pack manifests.

## Common checks

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --lib --bins
./scripts/check-architecture.sh
```

Use the narrowest affected test first. External-service tests belong only to
their explicit extension jobs and must not enter the default verification
path.
