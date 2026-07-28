# AetherEdge

[![Code Check](https://github.com/EvanL1/AetherEdge/actions/workflows/rust-check.yml/badge.svg)](https://github.com/EvanL1/AetherEdge/actions/workflows/rust-check.yml)
[![License](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](#license)
[![Rust](https://img.shields.io/badge/rust-1.90%2B-orange.svg)](https://www.rust-lang.org/)
[![Version](https://img.shields.io/badge/version-0.0.1-yellow.svg)](https://github.com/EvanL1/AetherEdge/releases)
[![Status](https://img.shields.io/badge/status-beta-orange.svg)](https://github.com/EvanL1/AetherEdge/releases)

**Documentation:** [docs.aetheriot.dev](https://docs.aetheriot.dev/) ·
[Getting started](docs/guides/getting-started.md) ·
[User journeys](docs/overview/user-journeys.md) ·
[Connect devices](docs/guides/connect-devices.md) ·
[Connect AI](docs/guides/ai-assistants.md) · [中文](README-CN.md)

**Connect physical devices, prove the data path, and commission deterministic
behavior—without making the cloud, a browser, or an AI model part of the
control loop.**

AetherEdge is an open-source, industry-neutral IoT edge kernel, six-service
runtime, CLI, and Rust SDK for Linux gateways. Shared memory is authoritative
for live point state; embedded SQLite stores desired state, history, audit, and
a durable outbox. The default distribution needs no Redis, PostgreSQL, cloud
service, browser, or LLM.

AI is a replaceable client behind the same typed, governed application boundary
as every other client. Device control is deny-by-default, explicitly confirmed,
and audited. Already commissioned acquisition, safety, rules, and alarms keep
running deterministically when every external client is disconnected.

## Is AetherEdge the right starting point?

| You want to… | Start with… |
|---|---|
| Connect field devices and run local behavior on a Linux gateway | **AetherEdge** |
| Deploy an energy-management solution and operator Console | [**AetherEMS**](https://github.com/EvanL1/AetherEMS) |
| Coordinate an edge fleet or cloud jobs | [**AetherCloud**](https://github.com/EvanL1/AetherCloud) |
| Implement or validate a shared protocol | [**AetherContracts**](https://github.com/EvanL1/AetherContracts) |

AetherEdge's direct users are device manufacturers, system integrators,
solution builders, application developers, and edge operators. It deliberately
does not pretend to be a finished application for every industry.

## From a blank host to a useful Edge

The product journey is:

```text
safe-empty install -> operator identity -> disabled device channel
  -> physical/logical point mapping -> read-only data proof
  -> reviewed behavior -> explicit commissioning -> audit and operation
```

Every consequential change follows:

```text
inspect -> plan -> validate -> confirm -> apply -> audit -> observe -> revise
```

Creating configuration never silently enables hardware.

### 1. Install a safe-empty runtime

Download the matching `.run` package and checksum from
[GitHub Releases](https://github.com/EvanL1/AetherEdge/releases), then verify and
run the fresh-install package on the target Linux host:

```bash
sha256sum -c AetherEdge-<arch>-<version>.run.sha256
chmod +x AetherEdge-<arch>-<version>.run
sudo ./AetherEdge-<arch>-<version>.run
```

The installer creates the six services, `aether` CLI, private bootstrap
credentials, embedded database, and an empty configuration. It does not add a
device, enable a rule, or install a domain solution.

### 2. Establish identity and prove the empty runtime

Start with the local health gate:

```bash
aether doctor
```

A healthy first boot has six healthy services and valid SHM. Sign in with the
private bootstrap credential, change that password immediately, create a
dedicated account for normal operation, and export its signed
`AETHER_ACCESS_TOKEN`. Then prove that nothing was commissioned implicitly:

```bash
aether channels list --json
aether models instances list --json
aether rules list --json
```

The channel, instance, and rule collections should all be empty.
[Getting Started](docs/guides/getting-started.md) covers the exact bootstrap and
token flow.

### 3. Create one channel—still disabled

Choose a protocol included in the installed IO build. A governed create command
requires authentication and confirmation, but the new channel remains disabled
unless `--enabled true` is explicitly requested:

```bash
AETHER_ACCESS_TOKEN='<signed access JWT>' aether channels create \
  --name "PLC#1" \
  --protocol modbus_tcp \
  --params '{"host":"192.168.1.10","port":502}' \
  --confirmed
```

Before enabling it, declare the physical points, map protocol addresses, bind
the required points to a logical instance supplied by a Domain Pack, and review
unresolved mappings. Follow [Connect Devices](docs/guides/connect-devices.md)
for that complete workflow.

### 4. Prove observation before control

```text
device -> aether-io -> authoritative SHM -> API and embedded history -> client
```

Verify channel health, timestamps, quality, freshness, topology generation,
historical samples, and unmapped points. A connected socket without fresh data
is not a healthy acquisition path, and a missing value is not zero.

Once the mapping is complete, enable the channel with the latest desired-state
revision returned by the channel query:

```bash
AETHER_ACCESS_TOKEN='<signed access JWT>' aether channels enable <CHANNEL_ID> \
  --expected-revision <REVISION> \
  --confirmed
```

The first useful milestone is a read-only data path. Do not add physical
commands merely to prove acquisition.

### 5. Add and commission deterministic behavior

Add logical models, calculations, alarms, and local rules through a downstream
Domain Pack or application composition. Draft rules and control paths stay
disabled until their inputs, targets, permissions, failure behavior, and audit
path have been reviewed.

```text
review disabled behavior -> validate -> confirm -> enable
  -> inspect audit evidence -> observe the physical outcome
```

A successful command acceptance is not proof that the physical device reached
the requested state. Observe that outcome separately.

### 6. Choose a replaceable client

All clients enter through authenticated `aether-api:6005`:

| Client | Use it for |
|---|---|
| `aether` CLI | Installation, commissioning, diagnostics, and operations |
| HTTP/OpenAPI | Dedicated applications and generated clients |
| Read-only MCP | AI-assisted inspection and explanation |
| Temporary write-enabled MCP | One bounded, explicitly authorized maintenance task |
| `aether-edge-sdk` | A downstream solution or embedded composition |
| Downstream Console | A domain-specific operator experience, such as AetherEMS |

The other five process APIs stay on loopback. Clients must not proxy them or
write SHM or SQLite directly. AetherEdge ships no universal Web Console; a UI
is a replaceable application client, never a second state authority.

To attach an existing runtime to Claude in the default read-only mode:

```bash
claude mcp add aether -- aether mcp
```

Set `AETHER_ACCESS_TOKEN` for the session. Use SSH stdio or an HTTPS ingress for
a remote Edge—never expose an internal service port. See
[Connect AI Assistants](docs/guides/ai-assistants.md).

## Developing without field hardware

Run the industry-neutral SDK composition or a protocol verification simulator.
Neither commissions a physical device:

```bash
cargo run -p aether-example-minimal-gateway
cargo run -p simulator -- \
  --scenario tools/simulator/scenarios/modbus_protocol_verification.yaml \
  --port 5020
```

A source checkout is a developer path, not the normal operator installation
flow. See [Getting Started](docs/guides/getting-started.md).

## Build a downstream solution

```bash
cargo add aether-edge-sdk --features local-runtime
```

`aether-edge-sdk`, imported as `aether_sdk`, is the supported Rust application
facade. A downstream product combines the SDK, a Domain Pack, and a dedicated
application or agent in its own repository. Domain processors, models, and
Consoles do not become dependencies of the AetherEdge kernel. AetherEMS is the
reference energy-domain implementation of this model.

## Runtime model

| Process | Responsibility |
|---|---|
| `aether-io` | Protocol acquisition and sole telemetry/status writer |
| `aether-automation` | Instances, rules, and audited control dispatch |
| `aether-alarm` | Alarm evaluation and lifecycle |
| `aether-history` | Embedded history and optional history adapters |
| `aether-api` | Authenticated headless application gateway |
| `aether-uplink` | Durable legacy Cloud/MQTT delivery and experimental CloudLink foundation |

```text
Devices -> aether-io -> authoritative SHM
                         |-> automation and alarms
                         |-> API and embedded history
                         `-> durable outbox -> optional cloud

              domain <- ports <- application <- runtime/interfaces
                        ^
                        `---- downstream static Rust adapters (out of tree)
```

AetherEdge currently delivers the integrator-grade runtime, application
contracts, governed commands, MCP foundations, Pack v1, and SDK facade. The
complete conversational intent compiler, simulation, temporary behavior, and
continuous outcome evaluation remain product direction. See the
[platform status](docs/roadmap/status.md) for the exact delivery boundary.

## Contributing

Development setup and verification live in
[CONTRIBUTING.md](CONTRIBUTING.md). Repository rules for agents and
contributors live in [AGENTS.md](AGENTS.md).

## License

Licensed under either [MIT](LICENSE-MIT) or
[Apache-2.0](LICENSE-APACHE), at your option.
