---
title: System Architecture
description: Isolated edge services communicating over shared memory, per-consumer UDS events, SQLite, HTTP, and MQTT
updated: 2026-07-11
---

# System Architecture

Aether is an AI-native industrial edge gateway built as six independently
supervised Rust services around a shared-memory hot path. Devices are polled by
aether-io, values land in SHM, and each real-time consumer resolves its logical
points from SQLite and reads the segment directly. A downstream composition
may mirror SHM into an external store, but no kernel service reads that mirror.
Generated applications and downstream product interfaces are optional clients;
they are not architecture boundaries of the edge kernel.

```
  Devices ─────► aether-io(:6001) ───── authoritative SHM live state
   protocols       sole T/S writer       │          │
                         ▲               │          └─ downstream read-only mirror
                         │ SHM + UDS      │
                         └──── aether-automation(:6002) (rules / C/A command owner)
                                          │
                 ┌──────────────┬──────────┼──────────────┐
                 ▼              ▼          ▼              ▼
          aether-alarm(:6007) aether-history(:6004) aether-api(:6005) aether-uplink(:6006)
          SHM + own event  SHM sampling  SHM + own event  SHM sampling
          bitmap / UDS     SQLite history bitmap / UDS    durable outbox
                 │              │          │              │
                 └─ local HTTP ─┘          └─ WebSocket   └─ MQTT cloud

  SQLite aether.db ───── configuration/discovery for every process
  SQLite history.db ──── default local historian store
  PostgreSQL/TimescaleDB ─ optional history adapter
```

In the reference Docker deployment (`docker-compose.yml`) every container runs
with `network_mode: host`. `/dev/shm` is mounted read/write at `/shm/rtdb` for
the main segments, per-consumer subscription bitmaps, and cross-container UDS
sockets. The five internal process APIs bind to `127.0.0.1`; only the
JWT-protected `aether-api` gateway is remotely reachable. Device actions are
also authenticated again by automation, so loopback headers cannot impersonate
an operator. No core service
mounts a Redis socket or waits for an external database.

## Services

Default deployment ports are defined in `libs/common/src/service_ports.rs` and
used as tooling fallbacks when runtime configuration does not override them.

| Service | Port | Role |
|---------|------|------|
| aether-io | 6001 | Communication service — explicitly compiled maintained drivers (Modbus, IEC 61850, MQTT, HTTP, raw CAN, GPIO, Aether-485), channel management, sole writer of telemetry into shared memory |
| aether-automation | 6002 | Model service — product definitions, device instances, rule engine execution |
| aether-history | 6004 | Historical data service — embedded SQLite by default; optional PostgreSQL / TimescaleDB via `postgres-storage` |
| aether-api | 6005 | API gateway — unified REST API, WebSocket push to browsers, JWT authentication |
| aether-uplink | 6006 | Network service — MQTT broker integration for the cloud uplink, TLS certificate management |
| aether-alarm | 6007 | Alarm service — alarm rules, alarm events, notifications |
| TimescaleDB | 5432 | Optional time-series database for historical data, runtime-configured through aether-history |

## Communication paths

Latency figures below come from historical `README.md` and CHANGELOG
measurements on production hardware (Cortex-A55 @ 1.4 GHz, ECU-1170). Release
qualification uses the current cross-process stress and soak gates.

| Path | Mechanism | Latency class |
|------|-----------|---------------|
| aether-io → all consumers (live data) | Shared-memory write; each consumer resolves configured slots from SQLite | ~10 ns per point into SHM |
| aether-io → aether-automation/aether-alarm/aether-api (point-change hints) | Independently filtered PointWatch bitmap + UDS per consumer | bounded, sub-millisecond local event path; polling repairs drops |
| aether-automation → aether-io (control commands) | Shared-memory write plus UDS notification (`ShmCommandListener` on the aether-io side) | sub-millisecond; ~215 µs P50 including rule evaluation (measured) |
| aether-io → device (protocol write) | Field bus (Modbus, IEC 104, etc.) | +5–10 ms; dominates the physical control loop |
| aether-alarm → aether-api, aether-uplink | HTTP (targets configured via `AETHER_API_URL` / `AETHER_UPLINK_URL`) | local HTTP |
| aether-uplink → cloud | Legacy MQTT by default; experimental broker-neutral CloudLink MQTT v1 is opt-in | network |
| aether-api → generated/downstream clients | Authenticated HTTP and WebSocket | network |
| all services ↔ SQLite | In-process configuration discovery (`AETHER_DB_PATH`); aether-history uses a separate embedded history file | local |

The UDS notification channel reconnects automatically with exponential backoff
(1–5 s) if aether-io restarts, so an aether-io restart does not require
restarting aether-automation.

Two properties keep the hot path safe:

- **Write ownership.** aether-io is the only writer of telemetry/signal slots in
  shared memory; aether-automation is the only writer of control/action slots. See
  [Shared Memory](shared-memory.md).
- **Events are hints, SHM is truth.** Event consumers always re-read the slot;
  aether-history and aether-uplink retain interval-based sampling semantics.
- **External stores stay outside the live path.** All six default services
  start and operate without Redis or PostgreSQL. A downstream mirror cannot
  become part of the control path.

## Startup order

aether-io owns point/health SHM publication and normally starts first. It
publishes both planes with one non-zero epoch and a final commit witness;
aether-automation can only attach to files that match its SQLite-derived
manifests and the same committed publication.

The ordering is enforced in application code, not by making every peripheral
service depend on Redis. Peripheral SHM readers open lazily and can start
before aether-io; a missing writer is a retryable read-time condition. On an
aether-io restart, new point and health generations are fully initialized and
atomically renamed over their canonical paths. Existing consumers keep the
old inode until their periodic identity check reopens the new generation, and
their subscription bitmaps are not truncated:

1. During startup, aether-automation calls
   `common::dependency::wait_for_dependency("aether-io", <aether-io>/health, 30s)`
   (`services/automation/src/bootstrap.rs`). The helper
   (`libs/common/src/dependency.rs`) polls the health URL every 2 seconds
   until it returns HTTP 2xx or the timeout expires. If aether-io is still not
   healthy after 30 seconds, aether-automation logs a warning and continues
   starting — with shared memory possibly unavailable until aether-io comes up.
2. When aether-automation opens live state,
   `ShmReadTopologyGeneration` checks both physical headers and the commit
   witness. A hash, slot count, epoch, or writer-generation mismatch remains
   retryably unavailable until the service publishes one complete generation.

## Configuration flow

```
config/*.yaml ──► aether sync ──► SQLite (aether.db) ──► services load at startup
```

Configuration is authored as YAML (and CSV point tables) under `config/`. The
`aether` CLI parses it and writes it into the shared SQLite database
(`tools/aether/src/core/syncer.rs`); services read only from SQLite — no
service crate parses YAML. Every service container receives the same
`AETHER_DB_PATH` pointing at `aether.db`. aether-io automatically reconciles
channel runtime, point/health layout, protocol mappings, and routing
projections from SQLite; governed explicit reconciliation remains available
for operator recovery.

## Where state lives

- **Live point values** — the shared-memory segment (`AETHER_SHM_PATH`,
  `/dev/shm` on Linux). This is the source of truth for the hot path; see
  [Shared Memory](shared-memory.md).
- **Downstream mirrors** — a custom composition may observe SHM through a
  read-only contract and publish an eventually consistent external view. It is
  never a source of truth or a startup dependency of kernel services.
- **SQLite (`aether.db`)** — all configuration: channels, products,
  instances, rules, service settings. Written only by `aether sync` and the
  services' own config APIs.
- **History database** — embedded `aether-history.db` by default. PostgreSQL /
  TimescaleDB remain opt-in adapters for larger deployments.

## Related pages

- [Shared Memory](shared-memory.md) — segment layout, seqlock, write ownership
- [Data Flow](data-flow.md) — upstream and downstream paths end to end
- [Rule Engine](rule-engine.md) — how aether-automation evaluates and executes rules
- [Data Model](data-model.md) — products, instances, points
- [Deployment Guide](../guides/deployment.md) — Docker Compose and installer
