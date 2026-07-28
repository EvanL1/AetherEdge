---
title: Data Flow
description: SHM-native uplink and downlink paths end to end, with latency budgets
updated: 2026-07-15
---

# Data Flow

Aether moves data along two independent paths. The **uplink** carries
measurement points — telemetry (T) and signal (S) values — from devices through
aether-io into shared memory, and from there to every consumer. The **downlink**
carries action points — control (C) and adjustment (A) commands — from the rule
engine or the HTTP API through aether-automation back to a device. Live point values and
command transport use the shared-memory segment as the source of truth and
transport. No default service needs Redis or PostgreSQL for live data.

## Uplink (device → consumers)

1. A protocol frame arrives on a communication channel and the channel's
   protocol adapter in aether-io decodes it into point values.
2. aether-io commits each typed T/S batch through
   `ShmAcquisitionStateWriter` (`libs/aether-shm-bridge/src/acquisition_writer.rs`).
   The adapter validates the immutable manifest and writer generation before
   and after mutation; slot-indexed writes are private implementation detail.
3. **Event path (immediate).** After every slot write, the
   `PointWatchPublisher` (`libs/aether-shm-bridge/src/point_watch.rs`) checks the
   independent bitmap owned by each event consumer. On a hit, a bounded queue
   sends a `PointWatchEvent` to that consumer's UDS. aether-automation,
   aether-alarm, and aether-api cannot steal or overwrite one another's subscriptions. The event
   is a wake-up hint only; each consumer re-reads SHM, and polling repairs
   dropped events.
4. **Direct read path.** Consumers resolve channel/instance coordinates from
   one SQLite topology snapshot and re-read matching SHM slots. History and
   Uplink bind their exact configured points and needed routes to one committed
   point/health epoch, then pin that immutable generation for a whole
   collection/upload pass. Events do not silently change their cadence.

The production `aether-uplink` composition still uses deprecated legacy MQTT
topics and its generic `FileOutbox` delivery boundary. The experimental
CloudLink path converts the same pinned generation into `PointSample` facts,
adds publication epoch plus topology digest, and places canonical business
content in a separate `CloudLinkSpool`. MQTT QoS 1/PUBACK advances only the
transport state; a matching cloud durable application ACK is the sole removal
authority. Disconnect keeps records replayable under the same stream
position/batch ID/digest and does not block any SHM consumer.
```
Device ──frame──► aether-io protocol adapter (decode)
                        │
                        ▼  set_direct (~10 ns/point)
                  SHM T/S slot (authoritative)
                   │             │
      per-consumer │             │ periodic sampling
      bitmap + UDS │             ├─► aether-history
       ┌───────────┴────┐        └─► aether-uplink
       ▼                ▼
 aether-automation aether-alarm/aether-api
    event hint   event hint → SHM re-read
```

## Downlink (rule/API → device)

1. An external HTTP, CLI, or MCP control call becomes a transport-neutral
   `RequestContext` in aether-automation. `ControlApplication` checks the
   `device.control` permission and explicit confirmation, persists a mandatory
   attempted audit event in local SQLite, and only then calls the command
   dispatcher. An internal deterministic rule action enters the existing
   dispatcher path directly during the staged migration.
2. The dispatcher calls aether-automation's `execute_action`
   (`services/automation/src/instance_data.rs`), which resolves the instance action point to its channel command point
   **once**, from the immutable M2C index in the currently pinned
   `RoutingSnapshot`. The default local-store adapter constructs that snapshot
   from commissioned configuration, but routing itself has no storage
   dependency. The resolved target is threaded through the rest of the call so
   a concurrent routing publication cannot change the decision mid-flight.
3. The offline gate reads the channel-health SHM segment. An offline channel
   rejects the write with `ChannelUnreachable` before anything is written.
4. After value validation, `ShmDeviceCommandSink`
   (`libs/aether-shm-bridge/src/command_sink.rs`) mirrors the C or A slot. The
   writer generation and canonical path are checked before and
   after the write; a mismatch means aether-io restarted and rebuilt the segment,
   so the write is discarded and the dispatch fails rather than landing in a
   stale layout.
5. The same command adapter sends a fixed-size 56-byte frame over a Unix
   domain socket. The notification carries the
   channel/point coordinates, the value bits, issue/expiry timestamps, and a
   producer id + sequence number for deduplication. If aether-io is down, the
   notifier reconnects with exponential backoff (1–5 s). Native deployments
   default to `/tmp/aether-m2c.sock`; Docker sets `AETHER_M2C_SOCKET` to
   `/shm/rtdb/aether-m2c.sock` so both isolated containers see the socket.
6. aether-io's `ShmCommandListener`
   (`services/io/src/core/channels/shm_listener.rs`) receives the
   notification, rejects expired frames, deduplicates by sequence, and forwards
   a command to the owning channel's queue. Immediately before protocol
   dispatch, `CommandGuard` verifies that the writable point exists and that
   the value satisfies its min/max/step policy; only then can the protocol
   adapter write it to the field bus.

Live command data never transits a database: the transport is SHM plus the UDS
notification. Local SQLite stores security audit events around external
commands, but is not part of command delivery and never mirrors the live point
value. A dispatch that fails partway (shared memory written
but the notification lost, or no writer available) surfaces as an error to the
caller; see [Data Model](data-model.md) for how those failures map to HTTP
statuses.

## Latency budget

The microsecond figures are historical measurements on production hardware
(Cortex-A55 @ 1.4 GHz, ECU-1170 / EdgeLinux 22.04) recorded in the README and
CHANGELOG. The nanosecond figure is the README's stated order of magnitude for
the hot-path write; release qualification must rerun current stress gates.

| Stage | Latency | Source label |
|-------|---------|--------------|
| aether-io shared-memory write (`set_direct`) | ~10 ns/point | README |
| Data change → aether-automation event received (PointWatch delivery) | P50 206 µs, P99 526 µs | README/CHANGELOG, measured |
| + rule evaluation + control SHM write + UDS notify to aether-io | ~215 µs P50, ~540 µs P99 (cumulative) | README, measured |
| + device protocol write (Modbus / IEC 104 field bus) | +5–10 ms | README |
| aether-alarm → aether-api/aether-uplink, service HTTP hops | local HTTP | — |

The CHANGELOG also records P99.9 at 1.4–2.2 ms for the event path, and notes
that PointWatch replaced the previous 100 ms Redis-tick polling model
(50–150 ms end to end) — roughly a 500× improvement on the critical path. The
software-internal control path is sub-millisecond; the field-bus write
dominates the physical control loop.

## Optional state mirrors

A downstream state mirror is not a participant in the control path. A custom
composition may implement the `StateMirror` port and publish an eventually
consistent remote view, but no kernel service reads that mirror and its
failure cannot affect acquisition, rules, alarms, history, API reads, uplink,
or command delivery.

Custom stores stay outside this repository, consume read-only state through
published contracts, remain non-authoritative, and cannot become core service
startup dependencies.

## Related pages

- [Architecture](architecture.md) — the services these paths connect
- [Shared Memory](shared-memory.md) — slot layout, seqlock, write ownership
- [Data Model](data-model.md) — points, instances, and NaN/absence semantics
- [CloudLink MQTT v1](../reference/cloudlink-mqtt-v1.md) — experimental application-ACK/replay edge path
- [Rule Engine](rule-engine.md) — what happens after a PointWatch event arrives
