---
title: Connect Devices
description: Configure channels, choose protocols, and map device points to instances
updated: 2026-07-10
---

# Connect Devices

A device attaches to Aether as a **channel** owned by the communication
service. A channel is one device connection: a protocol, the transport
parameters that protocol needs, and a point table describing what the device
exposes. Remote clients still enter through authenticated `aether-api:6005`;
they do not connect to the IO process port directly. Channel points then map to
device **instances** — the logical thing-model that rules and applications work
against (see [Data Model](../concepts/data-model.md)).

## Channels

Channels can be authored in `config/io/io.yaml` and loaded into SQLite by
`aether sync`; services never read the YAML directly. The shipped template is
intentionally `channels: []`. The following illustrative TCP and serial
connections remain disabled until an operator commissions them:

```yaml
channels:
  - id: 1
    name: "PLC#1"
    protocol: "modbus_tcp"
    enabled: false
    parameters:
      host: "192.168.1.10"
      port: 502
      connect_timeout_ms: 3000
      read_timeout_ms: 3000

  - id: 3
    name: "SENSOR#1"
    protocol: "modbus_rtu"
    enabled: false
    parameters:
      device: "/dev/ttyS4"
      baud_rate: 9600
      data_bits: 8
      stop_bits: 1
      parity: "N"
```

The `parameters` block is protocol-specific: Modbus TCP wants a host and
port, Modbus RTU wants a serial device and line settings, MQTT wants a broker
URL and subscription topics, and so on. Protocol names are normalized before
matching (`normalize_protocol_name` in `services/io/src/utils.rs`), so
`modbus-tcp`, `ModbusTCP`, and `modbus_tcp` all resolve to the same protocol.

Channels can also be created at runtime without touching YAML:

```bash
AETHER_ACCESS_TOKEN='<signed access JWT>' aether channels create \
  --name "PLC#2" --protocol modbus_tcp \
  --params '{"host": "192.168.1.11", "port": 502}' \
  --confirmed
```

The governed command goes through the application gateway, records audit
evidence, and creates the channel disabled by default. `aether channels list`,
`update`, `delete`, `enable`, and `disable` cover the rest of the lifecycle;
state-changing commands require confirmation and revision fencing where
applicable.

Each channel carries a point table split by the four point types —
telemetry (T, analog measurement), signal (S, digital status), control
(C, digital command), and adjustment (A, analog setpoint). Points are
managed with `aether channels points list|add|update|delete` or authored as
CSV tables next to the channel YAML and picked up by `aether sync`.

## Protocol availability

AetherEdge owns the seven protocol features used by its maintained runtime
compositions. The default Cargo feature set compiles Modbus, GPIO, Aether-485,
IEC 61850, and CAN; the AetherEMS-compatible distribution additionally selects
MQTT and HTTP.

| Protocol | Compiled by default | Platform notes |
|----------|--------------------:|----------------|
| Modbus TCP/RTU (`modbus`) | yes | Golden simulator E2E protocol |
| IEC 61850 MMS (`iec61850`) | yes | TCP/MMS implementation |
| MQTT (`mqtt`) | no | Event-driven JSON payloads; implies `json-mapping` |
| HTTP (`http`) | no | Polling and webhook modes; implies `json-mapping` |
| CAN (`can`) | yes | Linux only |
| GPIO (`gpio`) | yes | Linux only |
| Aether-485 (`aether_485`) | yes | Private RS-485 protocol |

BLE, DL/T 645, IEC 60870-5-104, J1939, Matter, OPC UA, and Zigbee are not
kernel capabilities. A configuration naming one of them fails as unavailable;
it is never silently translated to another adapter.

Two protocols are additionally OS-gated in the channel factory
(`services/io/src/protocols/gateway/factory.rs`): CAN and GPIO are
compiled only on Linux, so they never exist in a macOS build regardless of
features. Hardware-independent protocol tests run against `tools/simulator`;
the production IO binary contains no in-memory simulation protocol.

The rule of thumb: **if a channel fails to create, check the feature gate
first.** The factory's error is literal about it — `Unsupported protocol:
{name}. Check if the required feature is enabled.` — and the cause is almost
always a protocol that was not compiled in, not a configuration typo.

## Mapping points to instances

Channel points are protocol-flavored (register 62001 on channel 2); rules
and dashboards want model-flavored values (battery pack state of charge).
The bridge is an instance plus routing:

1. **Define the instance.** An instance binds a device to a product template
   supplied by the active Domain Pack in `config/automation/instances.yaml`.
   The default distribution intentionally starts empty and owns no
   industry-specific product. The product defines which measurement and action
   points the instance has; instance properties fill in its validated static
   values. Use a downstream solution such as AetherEMS when you need a ready
   energy-domain model.

2. **Map channel points to instance points.** Routing wires a channel point
   to an instance point: telemetry and signal points feed instance
   measurement points (M, the `route:c2m` table), and instance action
   points (A) drive channel control and adjustment points (`route:m2c`). Entries can
   be created through the CLI:

   ```bash
   aether routing create 1 --point-type M --point-id 9 \
     --channel-id 1 --four-remote T --channel-point-id 101
   ```

   which submits the governed routing command through `aether-api`, or in bulk
   with `aether routing batch`.

3. **Run `aether sync`** if the instance or routing was authored in YAML.
   Sync validates the configuration and writes it into SQLite, where the
   services load it; `--dry-run` validates without writing.

4. **Verify.** Two checks, one per side of the bridge:

   ```bash
   aether channels unmapped-points 1     # channel side
   aether routing list --channel 1       # instance side
   ```

   The first (`GET /api/channels/{id}/unmapped-points` on io) lists
   points declared on the channel whose protocol mapping is still empty —
   points io cannot poll because they are not yet wired to a protocol
   address. The second shows every routing entry touching the channel, so a
   forgotten instance binding stands out as a missing row.

## Verifying a connection

Check the channel status first:

```bash
aether channels status 1
```

This calls `GET /api/channels/{id}/status` and returns `connected`,
`running`, `last_update`, and cumulative statistics (read/write counts,
average response time). Note that `connected` checks both the transport
state and data freshness: a channel that holds its TCP connection but has
received no data for 90 seconds reports `false`.

Then watch a live value. On the channel side,
`GET /api/channels/{channel_id}/{T|S|C|A}/{point_id}` returns the current
value with its timestamp and raw protocol value. For direct inspection, open
the shared-memory REPL:

```bash
aether shm
```

If the channel point updates but the instance point does not, the routing
entry is missing or wrong. SHM is the authoritative live view, so no external
database needs to be running for this check.

What offline looks like: `aether channels status` reports
`connected: false`, the channel-health SHM entry becomes offline, and point
values stop updating — their timestamps go stale. A point that has *never*
been acquired is a NaN sentinel in shared memory, not a zero; see
[Data Model](../concepts/data-model.md) for why unavailability is a
first-class value. For a whole-system pass — services up, SQLite readable,
shared memory attached — run `aether doctor`.

## Related pages

- [Data Model](../concepts/data-model.md) — products, instances, and the four point types
- [System Architecture](../concepts/architecture.md) — where io and automation sit and how data flows between them
- [Writing Rules](writing-rules.md) — putting mapped points to work in control logic
