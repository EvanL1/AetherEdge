# Aether

[![Code Check](https://github.com/EvanL1/Aether/actions/workflows/rust-check.yml/badge.svg)](https://github.com/EvanL1/Aether/actions/workflows/rust-check.yml)
[![License](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-1.90%2B-orange.svg)](https://www.rust-lang.org/)
[![Version](https://img.shields.io/badge/version-0.5.0-yellow.svg)](CHANGELOG.md)
[![Status](https://img.shields.io/badge/status-beta-orange.svg)](CHANGELOG.md)

[中文](README-CN.md) | [Documentation](https://docs.aetheriot.workers.dev/) | [Changelog](CHANGELOG.md) | [llms.txt](https://docs.aetheriot.workers.dev/llms.txt)

**An AI-native, industry-neutral IoT edge kernel, runtime, and Rust SDK for Linux gateways.**

Aether connects field devices, keeps authoritative live state in shared memory, runs deterministic
local rules and alarms, and stores embedded history. Its default runtime works offline without an
LLM, Redis, PostgreSQL, a cloud service, or a browser.

> **Beta:** this repository is the integration workspace for the Aether kernel and the optional
> AetherEMS energy distribution. The neutral kernel is usable; the remaining compatibility work is
> tracked in [ADR-0007](docs/adr/0007-aether-core-and-ems-distribution.md).

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

Available now: typed domain/ports/application/data-plane and Pack v1 crates, six service binaries,
SHM/SQLite/local-outbox operation without external services, SDK examples, optional adapters, and
OpenAPI contract checks.

Still migrating: sensitive channel-configuration queries, instance, point/template/provisioning,
measurement-routing, history, uplink, and other configuration
mutations need complete application command/query boundaries; dev-only compatibility shims still
need removal; and Aether/AetherEMS still need independent signed releases, downstream consuming
CI, and the actual repository split. See
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
