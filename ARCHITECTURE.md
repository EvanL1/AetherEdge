# Aether Architecture

Aether is migrating from a Redis-centred multi-service EMS product to an
AI-native, industry-neutral edge kernel. The target architecture and migration
rules are defined in:

- [ADR-0001: AI-native edge kernel](docs/adr/0001-ai-native-edge-kernel.md)
- [ADR-0003: Multi-process SHM and event plane](docs/adr/0003-multi-process-shm-event-plane.md)
- [ADR-0004: Canonical service names](docs/adr/0004-canonical-service-names.md)
- [Target repository layout](docs/architecture/target-layout.md)
- [AI invariants](ai/invariants.md)
- [Capability safety policy](ai/safety-policy.yaml)

## Current migration state

The default Cargo graph is already external-service-free. It contains the
domain, ports, application layer, SDK, local adapters, the physical SHM data
plane, and the read-only SHM bridge. In particular:

- `aether-dataplane` owns mmap layout, seqlock slots, dirty tracking, and
  snapshots without depending on Redis, SQLx, or the legacy service model.
- `FileOutbox` provides bounded local store-and-forward with crash recovery.
- Redis and PostgreSQL implementations are optional integrations rather than
  prerequisites of the peripheral service data paths.
- `aether-alarm`, `aether-api`, `aether-history`, and `aether-uplink` discover logical points from
  SQLite and read current values directly from SHM. `aether-alarm` and
  `aether-api` also own isolated PointWatch bitmaps and UDS listeners.
- `aether-history` uses embedded SQLite history by default; PostgreSQL/TimescaleDB are
  enabled with the `postgres-storage` feature. `aether-uplink` retains its durable
  local outbox before MQTT.

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
compatibility mirror while remaining legacy aether-io/aether-automation paths are migrated.

Optional adapters may add Redis state mirroring, PostgreSQL history, MQTT
uplink, or HTTP APIs. They do not change the source-of-truth rules.

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
