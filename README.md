# AetherIot

[![Code Check](https://github.com/EvanL1/AetherIot/actions/workflows/rust-check.yml/badge.svg)](https://github.com/EvanL1/AetherIot/actions/workflows/rust-check.yml)
[![License](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-1.90%2B-orange.svg)](https://www.rust-lang.org/)
[![Version](https://img.shields.io/badge/version-0.5.0-yellow.svg)](CHANGELOG.md)
[![Status](https://img.shields.io/badge/status-beta-orange.svg)](CHANGELOG.md)

[Get started](docs/guides/getting-started.md) · [Documentation](https://docs.aetheriot.workers.dev/) · [Agent Skill](skills/aether-iot/SKILL.md) · [MCP](docs/guides/ai-assistants.md) · [中文](README-CN.md)

**Build reliable edge IoT applications with AI.**

AetherIot is an open-source, industry-neutral IoT edge kernel, runtime, and Rust SDK for Linux
gateways. It connects field devices, keeps authoritative live state in shared memory, runs
deterministic local rules and alarms, and stores embedded history without requiring Redis,
PostgreSQL, a cloud service, a browser, or an LLM.

AI is a client of AetherIot, not part of the hard real-time loop. Agents, CLIs, generated apps, and
operator interfaces all use the same typed command/query boundary; device control remains
deny-by-default, explicitly confirmed, and audited.

> **Beta:** AetherIot is the industry-neutral Kernel, Runtime, and SDK. Existing crates, binaries,
> the CLI, and some compatibility artifacts retain their `aether-*` / `aether` names. The official
> energy-management implementation lives in [AetherEMS](https://github.com/EvanL1/AetherEMS).

## Start with an AI agent

Install the repository's Agent Skill in any compatible coding assistant:

```bash
npx skills add EvanL1/AetherIot -s aether-iot
```

Build the CLI and expose the running edge system as read-only MCP tools:

```bash
cargo build --release -p aether
claude mcp add aether -- ./target/release/aether mcp
```

Then ask your assistant:

```text
Get started with AetherIot. Inspect this repository and generate a read-only
operations app for the capabilities exposed by my edge runtime.
```

The Skill supplies the development method and pulls current Markdown from the online docs. MCP
supplies live, structured capabilities. Write tools are not registered unless the operator starts
an explicitly write-enabled session, and every write still crosses the server-enforced permission,
confirmation, validation, and audit boundary.

See [Build Applications with AI](docs/guides/build-applications-with-ai.md) for the client contract
and [Agent Quickstart](https://docs.aetheriot.workers.dev/agent-quickstart/) for a complete safe-empty
runtime setup.

## What AetherIot provides

- **Deterministic edge runtime** — six isolated Rust services continue acquisition, rules, alarms,
  history, and uplink when no AI client is connected.
- **Local-first data plane** — shared memory is authoritative for live point state; SQLite provides
  embedded desired state, history, audit, and durable outbox storage.
- **Machine-readable contracts** — runtime manifests, OpenAPI, capability metadata, Pack manifests,
  MCP tools, and Markdown documentation give agents facts instead of prompt folklore.
- **One application boundary** — HTTP, CLI, MCP, and generated clients share governed queries and
  commands instead of writing SHM or storage directly.
- **Domain Packs** — industry knowledge, models, mappings, rules, and processing declarations layer
  over the kernel without becoming core dependencies.

## Headless by design

AetherIot does not ship a generic Web Console. A fixed dashboard cannot express every industry
Pack, and a browser must never become a second configuration authority. Instead, AetherIot ships
the contracts, Agent Skill, and development guidance needed to generate or maintain fit-for-purpose
applications.

User interfaces are downstream clients and reference implementations. They consume published
application APIs, remain replaceable, and receive no direct SHM, SQLite, or internal-service
access. The optional AetherEMS Console is one energy-domain implementation of this model.

## Try the SDK

These compositions require no external service and commission no hardware:

```bash
cargo run -p aether-example-minimal-gateway
cargo run -p aether-example-energy-gateway
```

The first is an empty industry-neutral gateway. The second proves a disabled-by-default Energy Pack
composition. They are SDK smoke tests, not the supervised production runtime.

## Edge runtime

| Process | Responsibility |
|---|---|
| `aether-io` | Protocol acquisition and sole telemetry/status writer |
| `aether-automation` | Instances, rules, and audited control dispatch |
| `aether-alarm` | Alarm evaluation and lifecycle |
| `aether-history` | Embedded history and optional history adapters |
| `aether-api` | Authenticated remote application API and WebSocket |
| `aether-uplink` | Cloud/MQTT delivery through a durable local outbox |

```text
Devices -> aether-io -> authoritative SHM
                         |-> automation and alarms
                         |-> API and embedded history
                         `-> durable outbox -> optional cloud

domain <- ports <- application <- runtime/interfaces
             ^
             `---- extensions
```

Only `aether-api` is a remote application boundary. The other process APIs stay on loopback.
Generated clients must use published application capabilities and must not expose or proxy those
internal ports.

## Project status

AetherIot is beta software. The versioned SDK, Pack v1, six-service runtime, coherent point/health
SHM epochs, embedded local operation, governed commands, MCP interface, and OpenAPI contract checks
are available. The first independent signed public release and removal of the remaining
revisionless compatibility paths are still pending. See [Architecture](ARCHITECTURE.md),
[ADR-0007](docs/adr/0007-aether-core-and-ems-distribution.md), and
[ADR-0012](docs/adr/0012-agent-first-application-surface.md) for the exact boundaries.

## Documentation

- [Agent Quickstart](https://docs.aetheriot.workers.dev/agent-quickstart/)
- [Build Applications with AI](docs/guides/build-applications-with-ai.md)
- [Connect AI Assistants](docs/guides/ai-assistants.md)
- [Connect Devices](docs/guides/connect-devices.md)
- [HTTP API and Swagger](docs/reference/http-api.md)
- [Deployment](docs/guides/deployment.md)
- [llms.txt](https://docs.aetheriot.workers.dev/llms.txt) and
  [llms-full.txt](https://docs.aetheriot.workers.dev/llms-full.txt)

## Development

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --lib --bins
./scripts/check-openapi-contracts.sh
./scripts/check-architecture.sh
```

Tests requiring an external service are excluded from the default path.

## License

MIT OR Apache-2.0, at your option. See [LICENSE](LICENSE).
