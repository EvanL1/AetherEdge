# Aether Agent Instructions

This file is the canonical instruction source for coding agents working in this
repository. Tool-specific files may add usage notes, but must not contradict it.

## Product Direction

Aether is an AI-native, industry-neutral IoT edge kernel and SDK. Energy
management is an optional domain pack, not a dependency of the core runtime.
The default distribution must run on one Linux edge host without Redis,
PostgreSQL, or any other external service.

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

## Rust Conventions

- Rust edition 2024; keep the pinned toolchain in `rust-toolchain.toml`.
- `mod.rs` files are forbidden.
- Library code returns typed errors; do not panic for recoverable failures.
- Avoid `unwrap` and `expect` in runtime library and binary code.
- Preserve no-std compatibility in the domain layer where practical.
- Write behavior tests before implementation and add conformance tests for
  every new port implementation.

## Verification

Run the narrowest affected test first, then:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --lib --bins
./scripts/check-architecture.sh
```

Tests that require an external service must be explicitly marked and must not
be part of the default verification path.

## Change Discipline

- Do not mix frontend work into edge-kernel changes.
- Do not edit generated files; regenerate them through the documented command.
- Record changes to dependency direction or data authority as an ADR.
- Keep compatibility shims during staged migration and state their removal
  criteria in the relevant ADR.
