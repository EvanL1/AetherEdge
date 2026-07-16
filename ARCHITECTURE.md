# Aether Architecture

Aether is migrating from a Redis-centred multi-service EMS product to an
AI-native, industry-neutral edge kernel. The target architecture and migration
rules are defined in:

- [ADR-0001: AI-native edge kernel](docs/adr/0001-ai-native-edge-kernel.md)
- [ADR-0003: Multi-process SHM and event plane](docs/adr/0003-multi-process-shm-event-plane.md)
- [ADR-0004: Canonical service names](docs/adr/0004-canonical-service-names.md)
- [ADR-0009: Aether Data Processing](docs/adr/0009-aether-data-processing.md)
- [ADR-0010: Physical acquisition addresses](docs/adr/0010-physical-acquisition-addresses.md)
- [ADR-0011: Governed channel desired state](docs/adr/0011-governed-channel-desired-state.md)
- [ADR-0017: Experimental CloudLink MQTT edge foundation](docs/adr/0017-experimental-cloudlink-mqtt-edge-foundation.md)
- [ADR-0018: Pinned AetherContracts consumption](docs/adr/0018-pinned-aethercontracts-consumption.md)
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
  `AcquisitionStateWriter` and `DeviceCommandSink` adapters. IO acquisition can
  represent only T/S writes; automation command transport can represent only
  C/A writes and returns success only after the local SHM + UDS command plane
  accepts the frame. Neither writer port is exposed to HTTP, CLI, MCP, or AI
  clients. The legacy aggregate is test-only in the default service and CLI
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
- Redis and PostgreSQL implementations are optional integrations rather than
  prerequisites of the peripheral service data paths.
- `aether-alarm`, `aether-api`, `aether-history`, and `aether-uplink` discover logical points from
  SQLite and read current values directly from SHM. `aether-alarm` and
  `aether-api` also own isolated PointWatch bitmaps and UDS listeners.
- `aether-history` uses embedded SQLite history by default; PostgreSQL/TimescaleDB are
  enabled with the `postgres-storage` feature. `aether-uplink` retains its durable
  local outbox before MQTT.
- `aether-cloudlink` implements the transport-neutral experimental candidate
  codec and truthful Runtime Manifest/`PointSample` mapping.
  `aether-cloudlink-mqtt` is a user-broker-neutral MQTT v3.1.1/QoS 1 extension.
  Legacy MQTT remains the runtime default while public AetherContracts alpha.3
  is experimental and production credential and durable-store gates remain open.
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
- Energy mappings, rules, evaluations, and Data Processing tasks are isolated
  Pack assets with closed v1 indexes. The local Kernel CLI can build and
  atomically install a Pack-only artifact; independently published/signed
  Aether and AetherEMS artifacts plus downstream consuming CI are still
  required before repository split.

## Target runtime

The production target is a supervised set of isolated processes: `aether-io`,
`aether-automation`, `aether-alarm`, `aether-history`, `aether-api`, and `aether-uplink`. A crash, blocked
driver, or cloud outage in one process must not take down acquisition or the
other services. They share only explicit local capabilities: SHM for current
state, per-consumer UDS/bitmap event channels, SQLite configuration, and local
HTTP command APIs.

An optional single-process composition may exist for tests, simulation, or
small development profiles. It is not the deployment default and does not
replace the service binaries. Neither profile requires PostgreSQL; Redis is a
compatibility mirror and never the live-state authority.

Optional adapters may add Redis state mirroring, PostgreSQL history, MQTT
uplink, or HTTP APIs. They do not change the source-of-truth rules.

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
does not fabricate a Thing Model revision. AetherCloud and AetherIot now share
the digest-pinned public AetherContracts subset. Three public behavior artifacts
remain pending, so distribution integrity does not imply codec conformance.
Remaining implementation mismatches and release gates are recorded in ADR-0017,
ADR-0018, and `contracts/cloudlink/v1/MIGRATION.md`.

## Data-processing capability

[Aether Data Processing](docs/concepts/data-processing.md) is the implemented
industry-neutral boundary for assembling governed IoT data and invoking a
local or remote processor. The application owns history and live-state
selection, semantic bindings, quality policy, authorization, and result
validation. A processor receives a complete bounded processing frame; it never
reaches back into SHM, the history database, or an industry pack.

The opt-in v1 composition lives in `aether-api`, reads raw history through a
read-only SQLite adapter, and returns `DerivedData` through authenticated
`/api/v1/data-processing/*` routes. The Load-Forecasting implementation remains
an isolated sidecar. CLI/MCP bindings, result caching, scheduling, and a
standalone Aether orchestration process are not implemented. Current history
and SHM adapters also do not preserve device-origin sample quality end to end;
they enforce freshness, gaps, missingness, constraints, and provenance. The
SQLite history is invocation-time consistent but not bitemporal: rows have no
ingestion/source epoch, and model provenance has no training/availability cut.
Therefore historical `as_of` alone is not a leakage-safe backtest boundary.
Production direct-history composition also requires a dedicated read-only
historian directory/identity for the API; SQLite read-only mode over the base
shared writable data mount is not an authority boundary.

Processing results are expiring derived-data artifacts, not live point state
and not device commands. They are never written into the IO-owned T/S segment,
and a forecast, aggregate, estimate, or classification can influence equipment
only through a separate automation/control use case. The domain contracts live
in `aether-domain`, the ports and orchestration live in `aether-ports` and
`aether-application`, and the strict v1 wire codec lives in
`aether-data-processing`. Concrete processors and HTTP transport remain
optional; `aether-data-processor` is only a reserved future process name. The
default six-service runtime continues to operate when no processor is installed.

## Dependency Rule

```text
interfaces ----> application ----> ports ----> domain
                       ^              ^
                       |              |
runtime/composition ---+          extensions
                       |
                  data plane
```

Only a composition root may depend on both application code and concrete
extensions. CI checks the core manifests for forbidden infrastructure
dependencies.

The concrete extraction and local-outbox decisions are recorded in
[ADR-0002](docs/adr/0002-dataplane-and-local-outbox.md).
