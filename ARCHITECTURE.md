# Aether Architecture

Aether is migrating from a Redis-centred multi-service EMS product to an
AI-native, industry-neutral edge kernel. The target architecture and migration
rules are defined in:

- [ADR-0001: AI-native edge kernel](docs/adr/0001-ai-native-edge-kernel.md)
- [ADR-0003: Multi-process SHM and event plane](docs/adr/0003-multi-process-shm-event-plane.md)
- [ADR-0004: Canonical service names](docs/adr/0004-canonical-service-names.md)
- [ADR-0010: Physical acquisition addresses](docs/adr/0010-physical-acquisition-addresses.md)
- [ADR-0011: Governed channel desired state](docs/adr/0011-governed-channel-desired-state.md)
- [ADR-0017: Experimental CloudLink MQTT edge foundation](docs/adr/0017-experimental-cloudlink-mqtt-edge-foundation.md)
- [ADR-0018: Pinned AetherContracts consumption](docs/adr/0018-pinned-aethercontracts-consumption.md)
- [ADR-0023: Canonical domain-model owner](docs/adr/0023-canonical-domain-model.md)
- [ADR-0024: Typed architecture contracts](docs/adr/0024-typed-architecture-contracts.md)
- [ADR-0025: Physical IO and optional protocol extensions](docs/adr/0025-physical-io-and-optional-protocol-extensions.md)
- [ADR-0026: Minimal kernel and out-of-tree integrations](docs/adr/0026-minimal-kernel-and-out-of-tree-integrations.md)
- [ADR-0027: Minimal physical protocol set](docs/adr/0027-minimal-physical-protocol-set.md)
- [ADR-0028: Move derived-data processing downstream](docs/adr/0028-move-derived-data-processing-downstream.md)
- [ADR-0029: Headless remote application boundary](docs/adr/0029-headless-remote-application-boundary.md)
- [Target repository layout](docs/architecture/target-layout.md)
- [AI invariants](ai/invariants.md)
- [Capability safety policy](ai/safety-policy.yaml)

## Current migration state

The default Cargo graph is already external-service-free. It contains the
domain, ports, application layer, SDK, local adapters, the physical SHM data
plane, and typed SHM port adapters. In particular:

- `aether-dataplane` owns mmap layout, seqlock slots, dirty tracking, and
  snapshots without depending on Redis, SQLx, or the legacy service model.
- `aether-shm-bridge` owns the typed channel manifest, channel-aware readers,
  generation lifecycle, isolated PointWatch publication, and production
  `AcquisitionStateWriter` and `DeviceCommandSink` adapters. The writer trait
  lives in the owner-only `aether-acquisition-port` crate, whose sole direct
  runtime consumer is `aether-shm-bridge`. IO acquisition can
  represent only T/S writes; automation command transport can represent only
  C/A writes and returns success only after the local SHM + UDS command plane
  accepts the frame. Neither writer port is exposed to HTTP, CLI, MCP, or AI
  clients. The retired legacy aggregate is absent from the service and CLI
  graphs.
- `FileOutbox` provides bounded legacy store-and-forward with crash recovery.
  The experimental `CloudLinkSpool` is separate: it preserves stream
  epoch/position, canonical business digests, replay and loss evidence, and
  removes a record only after a matching cloud application ACK.
- Local SQLite is authoritative for commissioned channel desired state. The
  active protocol runtime is a rebuildable projection, and channel
  create/update/delete/enable/disable cross the same confirmed, audited
  `io.channel.manage` application boundary from HTTP, CLI, and MCP. SHM remains
  authoritative for live point values.
- Redis is absent from the kernel composition and the workspace ships no Redis
  client or mirror implementation. PostgreSQL is not a default dependency; the
  History service retains an explicitly selected migration backend while its
  extraction decision remains separate.
- `aether-alarm`, `aether-history`, and `aether-uplink` discover logical points
  from SQLite and read current values directly from SHM. `aether-alarm` owns an
  isolated PointWatch bitmap and UDS listener. The headless `aether-api` does
  not attach to SHM; remote reads use the authenticated application gateway.
- `aether-history` uses embedded SQLite history by default; PostgreSQL/TimescaleDB are
  enabled with the `postgres-storage` feature. `aether-uplink` retains its durable
  local outbox before MQTT.
- `aether-cloudlink` implements the transport-neutral experimental candidate
  codec and truthful Runtime Manifest/`PointSample` mapping. The
  user-broker-neutral MQTT v3.1.1/QoS 1 binding is owned below
  `services/uplink`; it is not an extension and IO cannot select it. Legacy
  MQTT remains the runtime default while public AetherContracts alpha.3 is
  experimental and production credential and durable-store gates remain open.
- `aether-domain` is the sole business-semantics owner. The former
  `aether-model` compatibility crate has been removed: Pack product contracts
  live in `aether-pack`, SunSpec material is absent from the kernel, T/S/C/A
  configuration representation lives at the `common` adapter boundary, and
  firmware-only codecs and ABI primitives stay in the nested firmware workspace.
- Domain models and knowledge are absent by default. Automation and MCP load
  them only from manifest-validated Packs explicitly selected by
  `<AETHER_CONFIG_PATH>/global.yaml`; `packs: []` is the safe empty kernel.
- The composition-provided `runtime-manifest.json` records the Aether version,
  target, services, exact IO feature set, derived protocol adapters, and live
  application capability catalog under a canonical checksum. Automation, MCP,
  and Pack tooling share its fail-closed loader; there is no synthetic
  full-distribution fallback.

The remaining kernel migration is narrower but still real:

- many local management mutations have not yet moved behind transport-neutral
  application commands with declared capability, authorization, and audit
  contracts. This includes explicit channel/runtime reload and the sensitive
  full-configuration query, which still depend on the loopback deployment
  boundary;
- Energy mappings, rules, and evaluations remain isolated Pack assets with
  closed v1 indexes. Derived-data processing and forecasting have moved to the
  downstream AetherEMS repository.

## Target runtime

The production target is a supervised set of isolated processes: `aether-io`,
`aether-automation`, `aether-alarm`, `aether-history`, `aether-api`, and `aether-uplink`. A crash, blocked
driver, or cloud outage in one process must not take down acquisition or the
other services. They share only explicit local capabilities: SHM for current
state, per-consumer UDS/bitmap event channels, SQLite configuration, and local
HTTP command APIs.

An optional single-process composition may exist for tests, simulation, or
small development profiles. It is not the deployment default and does not
replace the service binaries. Neither profile requires PostgreSQL or Redis.

Downstream Rust compositions may implement published ports for third-party
systems. Those integrations stay outside this kernel repository and do not
change the source-of-truth rules.

## Experimental CloudLink boundary

CloudLink is an application delivery protocol, not another name for MQTT. Its
stream identity, digest, resume cursor, replay, conflict handling, data-loss
evidence, and durable application ACK remain transport neutral. MQTT owns only
connection/TLS/broker authentication, exact topic ACLs, QoS, PUBACK, keepalive,
and reconnect. Neither MQTT client acceptance nor PUBACK removes a CloudLink
record.

The endpoint and topic prefix may name any operator-selected MQTT v3.1.1
broker. An AetherCloud broker is not a runtime dependency. A private broker
that AetherCloud cannot reach needs a planned bridge/site connector. Broker or
cloud outage cannot enter acquisition, automation, alarms, safety interlocks,
history, or local control loops.

CloudLink v1 carries no arbitrary RPC, physical command, point/register write,
or SHM mutation. Point telemetry contains only edge-owned address, finite
value, source timestamp, exposed quality, and coherent topology generation. It
does not fabricate a Thing Model revision. AetherCloud and AetherEdge now share
the digest-pinned public AetherContracts subset. Three public behavior artifacts
remain pending, so distribution integrity does not imply codec conformance.
Remaining implementation mismatches and release gates are recorded in ADR-0017,
ADR-0018, and `contracts/cloudlink/v1/MIGRATION.md`.

## Dependency Rule

```text
services/interfaces ----> application ----> ports ----> domain
         |                     ^
         |                     |
         +---- composition ----+
         |
         +---- kernel adapters / data plane
```

Only a composition root may depend on both application code and concrete
kernel adapters. Executable package-edge contracts live in
`tools/aether-architecture-tests`; they consume Cargo metadata rather than
matching dependency text or source-file paths. Writer authority is represented
by dedicated dependency edges, and governed command entry points have behavior
contracts. The few source policies that cannot be expressed by visibility or a
trait are parsed structurally with `syn` by recursively following Cargo target
module roots. `scripts/check-architecture.sh` is only a thin Rust-test and Cargo
graph orchestrator. README wording, branding strings, and similar prose are
not architecture gates. Deployment, installer, runtime-manifest, and Pack
layout checks run independently through
`scripts/check-distribution-contracts.sh`.

The concrete extraction and local-outbox decisions are recorded in
[ADR-0002](docs/adr/0002-dataplane-and-local-outbox.md).
