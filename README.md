# Aether

[![Code Check](https://github.com/EvanL1/Aether/actions/workflows/rust-check.yml/badge.svg)](https://github.com/EvanL1/Aether/actions/workflows/rust-check.yml)
[![License](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-1.90%2B-orange.svg)](https://www.rust-lang.org/)
[![Version](https://img.shields.io/badge/version-0.5.0-yellow.svg)](CHANGELOG.md)
[![Status](https://img.shields.io/badge/status-beta-orange.svg)](CHANGELOG.md)

[中文](README-CN.md) | [Documentation](https://aether-docs.liuyifanz-1996.workers.dev/) | [Changelog](CHANGELOG.md) | [llms.txt](https://aether-docs.liuyifanz-1996.workers.dev/llms.txt)

**An AI-native edge runtime, kernel, and SDK—built because IoT work is a
natural fit for agents.**

Aether turns industrial devices, live point state, history, alarms, rules, and
control actions into typed capabilities that AI agents and operators can
discover and use through the same command/query application API.

It is built for Linux IoT gateways, runs fully offline, and keeps acquisition
and safety deterministic when no AI client is connected. The default runtime
requires no LLM, cloud connection, Redis, PostgreSQL, or browser.

## Why Aether exists

IoT engineering is full of work that agents are unusually good at: reading
protocol and device documentation, discovering capabilities, mapping points,
interpreting live context, diagnosing faults, generating rules, checking
configurations, and operating tools through constrained actions.

Conventional gateways make that work difficult. Their knowledge is scattered
across dashboards, databases, scripts, vendor APIs, and documents written only
for humans. Adding a chatbot does not fix the underlying boundary.

Aether was built around a different premise: make the edge system itself
legible and operable to agents. Capabilities, state, policy, documentation, and
verification live beside the code as typed, machine-readable contracts. The
deterministic runtime still owns acquisition and safety; agents work through a
controlled application boundary rather than entering the real-time loop.

**AetherEMS is the first official industry reference case for this idea.** It
combines the industry-neutral Aether kernel with the optional energy pack to
show how an agent-native IoT foundation becomes an energy gateway without
turning the core into an EMS-only product.

## Start with one device. Grow without replatforming.

Aether is designed as a progressive adoption path. A team can begin with one
Linux host and one protocol, then add context, automation, AI access, safe
control, and industry knowledge without replacing the live-state authority or
changing the application boundary.

| Stage | Outcome | Aether building block |
|---|---|---|
| **Connect** | Acquire one device reliably | Protocol adapter + `aether-io` + authoritative SHM |
| **Understand** | Give raw points stable meaning and history | Instance model + alarms + embedded history |
| **Automate** | Run deterministic behavior offline | Local rules + `aether-automation` |
| **Assist** | Let agents inspect state and retrieve operational knowledge | Read-only MCP tools/resources + `llms.txt` |
| **Act** | Allow bounded physical actions | Typed commands + authentication + confirmation + audit |
| **Extend** | Build a product for another industry or infrastructure stack | Domain packs + ports + optional adapters |

Every stage remains useful without the next one. AI never becomes a dependency
of acquisition, local automation, or safety behavior.

This makes Aether suitable for teams building branded edge products, system
integrators connecting existing equipment, agent developers who need a safe
physical-world tool surface, and domain experts packaging reusable industry
knowledge.

## Prove the foundation locally

Both examples are uncommissioned, contact no field device, and require no
external service:

```bash
# Industry-neutral Aether SDK composition
cargo run -p aether-example-minimal-gateway

# Aether plus the optional energy domain pack
cargo run -p aether-example-energy-gateway
```

Expected output:

```text
Aether minimal gateway ready: 2 capabilities, no external services
AetherEMS ready: pack=energy, capabilities=7, example_channels=8, commissioned=0
```

The first composition proves the public SDK has no energy dependency. The
second proves that AetherEMS is assembled from the same kernel by adding the
energy manifest, example models, and fail-closed commissioning policy.

## What AI-native means in Aether

AI-native is a runtime contract, not a chatbot attached to a conventional
gateway:

- **Capabilities are discoverable.** MCP tools, MCP resources, `llms.txt`, and
  the repository-owned AI catalog describe what the edge node can do.
- **Core operations are typed.** Queries and commands have explicit inputs,
  permissions, risk, idempotency, confirmation, and audit policy.
- **Device control shares one application boundary.** External actions from
  AI, CLI, and HTTP cannot bypass policy by writing SHM, a database, or a
  protocol driver directly.
- **Control is deny-by-default.** A device action requires authenticated
  authority, explicit confirmation, validation, and a durable audit record.
- **The real-time plane is independent of AI.** Protocol acquisition, local
  rules, reconnect behavior, and safety continue deterministically if the
  agent, model, network, or cloud disappears.
- **Industry knowledge is composable.** Domain packs teach agents and the
  runtime about an industry without adding domain dependencies to the kernel.

The contracts are versioned with the code:

| Contract | Purpose |
|---|---|
| [`llms.txt`](llms.txt) | AI-readable documentation index |
| [`ai/catalog.yaml`](ai/catalog.yaml) | Machine-readable component and verification catalog |
| [`ai/invariants.md`](ai/invariants.md) | Non-negotiable runtime and safety invariants |
| [`ai/safety-policy.yaml`](ai/safety-policy.yaml) | Capability risk, permission, confirmation, and audit policy |
| [`AGENTS.md`](AGENTS.md) | Canonical rules for coding agents working in the repository |
| [`ai/evals`](ai/evals) | Evaluation entry point for agent behavior and architectural conformance |

## Agent-to-device trust path

```text
AI agent or operator
        │
        ▼
MCP / CLI / authenticated HTTP
        │
        ▼
typed capability + request context
        │
        ├── query or command
        ├── risk level
        ├── required permission
        ├── confirmation policy
        ├── idempotency contract
        └── mandatory audit policy
        │
        ▼
application command/query API
        │
        ├── deny ──► durable audit
        │
        └── allow ─► routing + safety validation ─► SHM/UDS ─► device driver
```

For example, the machine-readable policy declares real device control as a
high-risk command:

```yaml
device.write_point:
  kind: command
  risk: high
  permission: device.control
  idempotent: false
  confirmation: always
  audit: required
```

This metadata is enforced by the application boundary for external device
actions; it is not documentation attached to an unchecked driver call.

## Connect an AI client

With an installed and running Aether edge runtime, build the CLI/MCP adapter:

```bash
cargo build --release -p aether
./target/release/aether mcp
```

The default MCP surface is read-only. A compatible desktop agent can launch it
over stdio:

```json
{
  "mcpServers": {
    "aether": {
      "command": "/absolute/path/to/aether",
      "args": ["mcp"]
    }
  }
}
```

Read-only tools can inspect channels, instances, live SHM values, alarms,
history, rules, and runtime status. Operational documentation is also exposed
as MCP resources so the agent can retrieve repository-owned guidance instead
of guessing device semantics.

Write tools are absent unless the operator explicitly starts
`aether mcp --allow-write`. Real device actions additionally require
`AETHER_ACCESS_TOKEN` from an authenticated Admin or Engineer session:

```bash
AETHER_ACCESS_TOKEN='<signed access JWT>' aether mcp --allow-write
```

Do not store access tokens directly in a checked-in MCP configuration. Use the
client's secret store or process environment. Before enabling writes, read
[Safe Operations for AI Agents](docs/domain/safe-operations.md) and the
[MCP Tool Reference](docs/reference/mcp-tools.md).

## Safety properties for physical systems

- SHM is authoritative for current point state and has explicit writer
  ownership.
- Only acquisition owns the live telemetry/signal writer; application
  interfaces receive read-only live state.
- External C/A device commands enter through automation's authenticated,
  confirmed, and audited application use case.
- Forged actor or role headers do not grant control authority.
- Uplink commands use a separately generated service credential and a fixed
  server-side identity.
- T/S simulation writes are disabled by default because forged measurements
  can trigger real automation rules.
- AI is never placed inside a protocol polling loop or hard real-time safety
  loop.
- External-service or AI failure cannot stop local acquisition and safety
  behavior.

The detailed trust-boundary decision is recorded in
[ADR-0008](docs/adr/0008-application-control-boundary.md).

## Edge runtime

Aether acquires device data, maintains a sub-millisecond shared-memory live
state plane, evaluates local automation and alarms, stores embedded history,
and delivers cloud data through a crash-recoverable local outbox.

Production deliberately uses six independently supervised Rust processes:

| Process | Responsibility |
|---|---|
| `aether-io` | Protocol acquisition and sole telemetry/signal writer |
| `aether-automation` | Instances, rules, and control/action dispatch |
| `aether-alarm` | Alarm evaluation and lifecycle |
| `aether-history` | Embedded history and optional history adapters |
| `aether-api` | Authenticated management API and WebSocket |
| `aether-uplink` | MQTT/cloud delivery with a local durable outbox |

A blocked driver, failed cloud connection, or crashed peripheral process must
not take down acquisition or the other services. Only `aether-api` is intended
for remote access; packaged internal service APIs bind to loopback.

```text
Devices ─► aether-io ─► authoritative SHM
             │              ├─ event hint ─► aether-automation
             │              ├─ event hint ─► aether-alarm
             │              ├─ event hint ─► aether-api
             │              ├─ reconcile ──► aether-history ─► SQLite
             │              └─ reconcile ──► aether-uplink ─► FileOutbox ─► cloud
             │
             └─ optional adapters ─► Redis mirror / PostgreSQL history
```

## Kernel and SDK contract

Dependency direction is one-way:

```text
domain <- ports <- application <- runtime/interfaces
             ^
             +---- extensions
```

| Layer | Responsibility |
|---|---|
| `aether-domain` | Industry-neutral point, identity, quality, and command types |
| `aether-dataplane` | Database-free SHM layout, atomic slots, mmap I/O, snapshots |
| `aether-ports` | Capabilities such as `LiveState`, `HistorySink`, and `StateMirror` |
| `aether-application` | Commands, queries, permissions, confirmation, and audit |
| `aether-edge-sdk` (`aether_sdk`) | Stable builder and public facade |
| `extensions/*` | Local, SHM, Redis, PostgreSQL, and platform adapters |
| `packs/*` | Declarative industry knowledge; energy is the first official pack |

The default Cargo members and edge composition do not require Redis or
PostgreSQL. Embedded SQLite and the local durable outbox cover the standalone
runtime; external stores are opt-in port implementations and never live-state
authorities.

## Protocols and extensions

The standard edge build includes Modbus TCP/RTU, IEC 61850 MMS, CAN, GPIO,
Aether-485, and Virtual channels. Optional Cargo features add IEC 104, OPC UA,
MQTT, HTTP, DL/T 645, J1939, BLE, Zigbee, and Matter.

Protocol support means the adapter compiles and is covered by conformance
tests. A real deployment must still validate mappings, timeouts, command
bounds, reconnect semantics, and hardware behavior.

```text
extensions/store-local       default embedded state, audit, and outbox
extensions/redis-bridge      optional non-authoritative state mirror
extensions/postgres-history  optional external history sink
```

## Domain packs and AetherEMS

Aether is industry-neutral. A domain pack can provide models, mappings, rules,
agent guidance, capability policy references, and disabled commissioning
examples without changing the kernel.

[`packs/energy`](packs/energy) is the first official pack. It forms the
AetherEMS energy distribution while the kernel remains usable for industrial
automation, buildings, agriculture, environmental monitoring, and other IoT
domains.

## Installation and deployment

Build the CLI from source and generate a reviewable setup plan:

```bash
cargo build --release -p aether
./target/release/aether --json setup
```

Applying a plan initializes an empty local site and never enables hardware by
default. The standalone CLI installer installs only the CLI; the edge `.run`
package installs the complete six-process runtime and generates local control
credentials without printing them.

- [Getting Started](docs/guides/getting-started.md)
- [Connect AI Assistants](docs/guides/ai-assistants.md)
- [Deployment](docs/guides/deployment.md)
- [CLI Reference](docs/reference/cli.md)
- [Configuration Reference](docs/reference/configuration.md)

The browser application is an optional client and is not required by the SDK,
runtime, MCP server, or either composition example.

## Repository map

```text
crates/       stable industry-neutral kernel and SDK
extensions/   optional storage, SHM, and platform adapters
services/     six production process-isolation boundaries
tools/        Aether CLI/MCP launcher and simulator
examples/     runnable Aether and AetherEMS compositions
packs/        declarative industry knowledge
ai/           machine-readable invariants, capability policy, runbooks, evals
docs/         architecture, concepts, guides, and reference
libs/         shared runtime and configuration libraries
apps/         optional legacy browser client
```

## Development

```bash
cargo test
cargo test -p aether-example-minimal-gateway --test composition_contract
cargo test -p aether-example-energy-gateway --test composition_contract
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
./scripts/check-architecture.sh
```

Tests requiring an external service are excluded from the default path.

## Status

Version 0.5 is beta. CI enforces the industry-neutral kernel boundary, SHM
authority, local outbox, canonical service identities, fail-safe installation,
and the authenticated external device-action path.

The default AI surface is read-only. `--allow-write` exposes operational tools
only after an explicit operator decision; capability-specific permissions and
safety requirements are documented in the MCP reference. Real device actions
require authenticated authority, confirmation, validation, and durable audit.

See [ARCHITECTURE.md](ARCHITECTURE.md), the [ADR index](docs/adr), and the
[changelog](CHANGELOG.md) for exact boundaries.

## License

MIT OR Apache-2.0, at your option. See [LICENSE](LICENSE).
