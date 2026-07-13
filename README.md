# AetherIot

[![Code Check](https://github.com/EvanL1/AetherIot/actions/workflows/rust-check.yml/badge.svg)](https://github.com/EvanL1/AetherIot/actions/workflows/rust-check.yml)
[![License](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-1.90%2B-orange.svg)](https://www.rust-lang.org/)
[![Version](https://img.shields.io/badge/version-0.5.0-yellow.svg)](CHANGELOG.md)
[![Status](https://img.shields.io/badge/status-beta-orange.svg)](CHANGELOG.md)

[中文](README-CN.md) | [Documentation](https://docs.aetheriot.workers.dev/) | [Changelog](CHANGELOG.md) | [llms.txt](https://docs.aetheriot.workers.dev/llms.txt)

**An AI-native, industry-neutral IoT edge kernel, runtime, and Rust SDK for Linux gateways.**

AetherIot connects field devices, keeps authoritative live state in shared memory, runs deterministic
local rules and alarms, and stores embedded history. Its default runtime works offline without an
LLM, Redis, PostgreSQL, a cloud service, or a browser.

> **Beta:** AetherIot is the industry-neutral kernel, runtime, and SDK. The energy-management
> implementation and distribution lives independently in
> [AetherEMS](https://github.com/EvanL1/AetherEMS). Existing Rust crates, binaries, and the CLI
> retain their `aether-*` / `aether` names for API compatibility. Remaining release work is tracked
> in [ADR-0007](docs/adr/0007-aether-core-and-ems-distribution.md).

AetherIot is deliberately headless: it ships no product-specific Web UI, frontend image, or
frontend system service. The EMS operator console is owned and released by AetherEMS, which uses
the same authenticated application API as other clients.

## Try the SDK

These compositions require no external service and commission no hardware:

```bash
cargo run -p aether-example-minimal-gateway
cargo run -p aether-example-energy-gateway
```

The first is an empty industry-neutral gateway. The second adds the optional
[Energy Pack](packs/energy). They are SDK smoke tests, not the supervised production runtime.

## Edge runtime

| Process | Responsibility |
|---|---|
| `aether-io` | Protocol acquisition and sole telemetry/status writer |
| `aether-automation` | Instances, rules, and audited control dispatch |
| `aether-alarm` | Alarm evaluation and lifecycle |
| `aether-history` | Embedded history and optional history adapters |
| `aether-api` | Authenticated management API and WebSocket |
| `aether-uplink` | Cloud/MQTT delivery through a durable local outbox |

Start from the reviewed safe-empty configuration in
[Getting Started](docs/guides/getting-started.md), then use `aether doctor` for acceptance. The
browser client, external databases, and cloud connectivity are optional.

## Swagger UI

The built-in interface documentation is generated from each service's Rust OpenAPI contract. It
is feature-gated; include it in an edge package with:

```bash
./scripts/build-installer.sh v0.5.0 arm64 -s rust --enable-swagger
```

| Service | Swagger UI | OpenAPI JSON |
|---|---|---|
| `aether-io` | `http://127.0.0.1:6001/docs` | `http://127.0.0.1:6001/openapi.json` |
| `aether-automation` | `http://127.0.0.1:6002/docs` | `http://127.0.0.1:6002/openapi.json` |
| `aether-history` | `http://127.0.0.1:6004/docs` | `http://127.0.0.1:6004/openapi.json` |
| `aether-api` | `http://<edge-host>:6005/docs` | `http://<edge-host>:6005/openapi.json` |
| `aether-uplink` | `http://127.0.0.1:6006/docs` | `http://127.0.0.1:6006/openapi.json` |
| `aether-alarm` | `http://127.0.0.1:6007/docs` | `http://127.0.0.1:6007/openapi.json` |

Only `aether-api` is intended for remote access. Keep the other five services on loopback. The
documentation routes are public and never bypass operation authorization. Governed channel,
automation, alarm, and Data Processing operations show their authentication, confirmation,
correlation, accepted/degraded results, and audit contract in Swagger; remaining service-local
management routes are still migration work.
Enable Swagger only on a trusted commissioning network.

## Architecture and safety

```text
Devices -> aether-io -> authoritative SHM
                         |-> automation and alarms
                         |-> API and embedded history
                         `-> durable outbox -> optional cloud

domain <- ports <- application <- runtime/interfaces
             ^
             `---- extensions
```

- SHM is authoritative for current point state; external stores may only mirror it.
- Only acquisition owns telemetry/status writes. Application interfaces are read-only consumers.
- Device control is deny-by-default and requires permission, confirmation, validation, and audit.
- Channel commissioning, external device actions, manual rule execution, and physical
  action-routing changes share application command boundaries across HTTP, CLI, and MCP; MCP
  writes additionally require explicit `--allow-write`.
- AI is outside polling and hard real-time safety loops.

## Maturity

Available now: a beta, versioned domain/ports/application/data-plane SDK; Pack v1; six service
binaries; SHM/SQLite/local-outbox operation without external services; SDK examples; optional
adapters; and OpenAPI contract checks. Point and health SHM planes publish one committed physical
epoch, while History and Uplink bind one SQLite topology snapshot to that exact epoch. SQLite is
the single desired-state authority for commissioned topology, protocol mappings, logical routes,
rules, and instances, with revisioned commands and automatic runtime reconciliation.

Still migrating: supported clients must finish sending explicit channel and rule revisions before
the remaining revisionless compatibility paths can be removed. Direct test-only instance and
routing mutation helpers are already gone. The local release workflow validates one
dependency-ordered catalog of public crates, compiles their
exact archives in a clean-room consumer, checks established APIs for SemVer compatibility, and
gates separate attested Kernel, CLI, crate, and Pack artifacts. The physical repository split and
downstream AetherEMS consuming CI now exist, but no tag has yet established the first independent
registry/GitHub release or replaced the downstream bootstrap Git pin with signed artifacts. The
former EMS frontend has moved to AetherEMS as its independently tested Console. See
[Architecture](ARCHITECTURE.md) for the current facts.

## Documentation

- [Getting Started](docs/guides/getting-started.md)
- [Connect Devices](docs/guides/connect-devices.md)
- [HTTP API and Swagger](docs/reference/http-api.md)
- [Connect AI Assistants](docs/guides/ai-assistants.md)
- [Deployment](docs/guides/deployment.md)
- [Architecture](ARCHITECTURE.md) and [ADR index](docs/adr)

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
