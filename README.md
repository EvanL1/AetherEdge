# AetherEdge

[![Code Check](https://github.com/EvanL1/AetherEdge/actions/workflows/rust-check.yml/badge.svg)](https://github.com/EvanL1/AetherEdge/actions/workflows/rust-check.yml)
[![License](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-1.90%2B-orange.svg)](https://www.rust-lang.org/)
[![Version](https://img.shields.io/badge/version-0.5.0-yellow.svg)](CHANGELOG.md)
[![Status](https://img.shields.io/badge/status-beta-orange.svg)](CHANGELOG.md)

[Get started](docs/guides/getting-started.md) · [Documentation](https://docs.aetheriot.workers.dev/) · [Agent Skill](skills/aether-iot/SKILL.md) · [MCP](docs/guides/ai-assistants.md) · [中文](README-CN.md)

**Build reliable edge IoT applications with AI.**

AetherEdge is an open-source, industry-neutral IoT edge kernel, runtime, and Rust SDK for Linux
gateways. It connects field devices, keeps authoritative live state in shared memory, runs
deterministic local rules and alarms, and stores embedded history without requiring Redis,
PostgreSQL, a cloud service, a browser, or an LLM.

AetherEdge is the edge product in the [AetherIoT platform](docs/overview/platform.md), alongside
[AetherCloud](https://github.com/EvanL1/AetherCloud) and
[AetherContracts](https://github.com/EvanL1/AetherContracts). This repository was formerly named
`EvanL1/AetherIot`; software identifiers remain stable during the
[migration](docs/migration/aetheriot-to-aetheredge.md).

AI is a client of AetherEdge, not part of the hard real-time loop. Agents, CLIs, generated apps, and
operator interfaces all use the same typed command/query boundary; device control remains
deny-by-default, explicitly confirmed, and audited.

> **Beta:** AetherEdge is the industry-neutral Kernel, Runtime, and SDK. Existing crates, binaries,
> the CLI, and some compatibility artifacts retain their `aether-*` / `aether` names. The official
> energy-management implementation lives in [AetherEMS](https://github.com/EvanL1/AetherEMS).

## Install AetherEdge

AetherEdge is not an npm or Bun package. `npx` and `bunx` do not install the Kernel, Runtime, CLI,
or Rust SDK.

For a Docker-based Linux edge host, download the matching `AetherEdge-<arch>-<version>.run` file and
its `.sha256` file from [GitHub Releases](https://github.com/EvanL1/AetherEdge/releases). Verify and
run the fresh-install package on the target host:

```bash
sha256sum -c AetherEdge-<arch>-<version>.run.sha256
chmod +x AetherEdge-<arch>-<version>.run
sudo ./AetherEdge-<arch>-<version>.run
```

The `.run` package installs the six-service edge Runtime and the `aether` CLI. Releases also contain
standalone CLI archives; those do not install the Runtime. For a source checkout or SDK development,
follow [Getting Started](docs/guides/getting-started.md). Running
`cargo install --path tools/aether --locked` installs only the CLI. See
[Deployment](docs/guides/deployment.md) for Docker and bare-metal package contracts.

## Optional: connect an AI agent

The repository's Agent Skill is optional development guidance, not an AetherEdge software package.
See [Build Applications with AI](docs/guides/build-applications-with-ai.md) if you want to add it to
a compatible assistant.

Expose a running edge system as read-only MCP tools:

```bash
claude mcp add aether -- aether mcp
```

Then ask your assistant:

```text
Get started with AetherEdge. Inspect this repository and generate a read-only
operations app for the capabilities exposed by my edge runtime.
```

The optional Skill supplies the development method and pulls current Markdown from the online docs.
MCP supplies live, structured capabilities. Write tools are not registered unless the operator
starts an explicitly write-enabled session, and every write still crosses the server-enforced
permission, confirmation, validation, and audit boundary.

See [Build Applications with AI](docs/guides/build-applications-with-ai.md) for the client contract
and [Agent Quickstart](https://docs.aetheriot.workers.dev/agent-quickstart/) for a complete safe-empty
runtime setup.

## What AetherEdge provides

- **Deterministic edge runtime** — six isolated Rust services continue acquisition, rules, alarms,
  history, and uplink when no AI client is connected.
- **Local-first data plane** — shared memory is authoritative for live point state; SQLite provides
  embedded desired state, history, audit, and durable outbox storage.
- **Machine-readable contracts** — runtime manifests, OpenAPI, capability metadata, Pack manifests,
  experimental CloudLink schemas, MCP tools, and Markdown documentation give agents facts instead
  of prompt folklore.
- **One application boundary** — HTTP, CLI, MCP, and generated clients share governed queries and
  commands instead of writing SHM or storage directly.
- **Domain Packs** — industry knowledge, models, mappings, rules, and processing declarations layer
  over the kernel without becoming core dependencies.

## Headless by design

AetherEdge does not ship a generic Web Console. A fixed dashboard cannot express every industry
Pack, and a browser must never become a second configuration authority. Instead, AetherEdge ships
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

`aether-edge-sdk`, imported as `aether_sdk`, is the only supported Rust
application facade. Workspace implementation crates are source-only and cannot
be published independently. Downstream builds pin the exact commit behind a
signed source release and select local adapters through the SDK's
`local-runtime` feature.

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
| `aether-uplink` | Legacy Cloud/MQTT delivery through a durable local outbox; experimental CloudLink foundation |

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

AetherEdge is beta software. The versioned SDK, Pack v1, six-service runtime, coherent point/health
SHM epochs, embedded local operation, governed commands, MCP interface, and OpenAPI contract checks
are available. The signed `v0.5.0` source/runtime/CLI release is published;
replacement of the downstream bootstrap pin and removal of the remaining
revisionless compatibility paths are still pending. See [Architecture](ARCHITECTURE.md),
[ADR-0007](docs/adr/0007-aether-core-and-ems-distribution.md), and
[ADR-0012](docs/adr/0012-agent-first-application-surface.md),
[ADR-0013](docs/adr/0013-single-sdk-source-release.md),
[ADR-0014](docs/adr/0014-coordinated-shm-topology-publication.md), and
[ADR-0015](docs/adr/0015-configuration-authority-and-reconciliation.md) for the exact boundaries.

Point and health SHM planes publish one committed physical epoch, while History
and Uplink bind one SQLite topology snapshot to that exact epoch. SQLite is the
desired-state authority for commissioned topology, protocol mappings, logical
routes, rules, and instances, with revisioned commands and automatic runtime
reconciliation. The local release gate rejects registry publication, verifies
that every workspace package is source-only, and signs the Kernel source,
runtime, manifest, and CLI artifacts. The physical AetherEMS split and its
downstream bootstrap CI exist, but AetherEMS has not yet replaced its bootstrap
Git pin with the signed release evidence.

The broker-neutral CloudLink MQTT v1 **edge foundation** is implemented as an
experimental, opt-in candidate: strict JSON schemas/codecs, a dedicated
application-ACK-driven memory/file spool, user-selected MQTT v3.1.1 broker
binding, session/heartbeat/manifest/point telemetry, and replay tests. The
legacy MQTT adapter remains the compatibility default. AetherCloud and
AetherEdge now consume the same digest-pinned AetherContracts
`v0.1.0-alpha.3` release with identical complete-consumer locks and no pending
imports. This proves distribution integrity and public fixture execution, not
production Rust/TypeScript transport conformance: key lifecycle, a future signed ACK,
Cloud batch-position persistence, and crash-durable Cloud stores remain open.
The dual-process Broker harness is development evidence only. See
[ADR-0017](docs/adr/0017-experimental-cloudlink-mqtt-edge-foundation.md) and the
[CloudLink reference](docs/reference/cloudlink-mqtt-v1.md). Public release
authority is defined by [ADR-0018](docs/adr/0018-pinned-aethercontracts-consumption.md).

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

Run focused checks for the crates or scripts you changed. The complete
workspace matrix is enforced by pull-request CI; `quick-check.sh` remains an
optional local release gate rather than the default edit loop.

```bash
cargo fmt --all -- --check
cargo clippy -p <affected-package> --all-targets --all-features -- -D warnings
cargo test -p <affected-package>

# Optional full local release gate
./scripts/quick-check.sh
```

Tests requiring an external service are excluded from the default path.

## License

MIT OR Apache-2.0, at your option. See [LICENSE](LICENSE).
