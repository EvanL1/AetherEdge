---
title: Connect Devices
description: Configure channels, choose protocols, and map device points to instances
updated: 2026-07-10
---

# Connect Devices

A device attaches to Aether as a **channel** in the communication service
(io, port 6001). A channel is one device connection: a protocol, the
transport parameters that protocol needs, and a point table describing what
the device exposes. Channel points then map to device **instances** — the
logical thing-model that rules and dashboards work against (see
[Data Model](../concepts/data-model.md)).

## Channels

Channels are authored in `config/io/io.yaml` and loaded into SQLite
by `aether sync`; services never read the YAML directly. A trimmed example
from the shipped template (`config.template/io/io.yaml`), showing one
TCP and one serial connection:

```yaml
channels:
  - id: 1
    name: "PCS#1"
    protocol: "modbus_tcp"
    enabled: true
    parameters:
      host: "192.168.1.10"
      port: 502
      connect_timeout_ms: 3000
      read_timeout_ms: 3000

  - id: 3
    name: "GENSET#1"
    protocol: "modbus_rtu"
    enabled: true
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
aether channels create --name "PCS#2" --protocol modbus_tcp \
  --params '{"host": "192.168.1.11", "port": 502}'
```

which calls `POST /api/channels` on io. `aether channels list`,
`update`, `delete`, `enable`, and `disable` cover the rest of the lifecycle.

Each channel carries a point table split by the four point types —
telemetry (T, analog measurement), signal (S, digital status), control
(C, digital command), and adjustment (A, analog setpoint). Points are
managed with `aether channels points list|add|update|delete` or authored as
CSV tables next to the channel YAML and picked up by `aether sync`.

## Protocol availability

io speaks 14 protocols, but most are behind compile-time Cargo features
(`services/io/Cargo.toml`), so a given binary usually contains only a
subset. The default feature set compiles Modbus, GPIO, Aether-485,
IEC 61850, and CAN.

| Protocol | Compiled by default | Platform notes |
|----------|--------------------:|----------------|
| Modbus TCP/RTU (`modbus`) | yes | |
| IEC 60870-5-104 (`iec104`) | no | |
| IEC 61850 MMS (`iec61850`) | yes | |
| OPC UA (`opcua`) | no | Optional feature; currently restricted to anonymous `SecurityPolicy::None` sessions. |
| MQTT (`mqtt`) | no | event-driven JSON payloads; enabling pulls in `json-mapping` |
| HTTP (`http`) | no | polling and webhook modes; enabling pulls in `json-mapping` |
| DL/T 645-2007 (`dl645`) | no | smart meters over serial or TCP |
| CAN (`can`) / J1939 (`j1939`) | CAN yes, J1939 no | Linux only; `j1939` implies `can` |
| GPIO (`gpio`) | yes | Linux only |
| BLE GATT (`ble`) | no | |
| Zigbee (`zigbee`) | no | via TCP gateway |
| Matter (`matter`) | no | |
| Aether-485 (`aether_485`) | yes | private RS-485 protocol |
| Virtual | always | no feature gate; exists for testing and simulation |

Two protocols are additionally OS-gated in the channel factory
(`services/io/src/protocols/gateway/factory.rs`): CAN and GPIO are
compiled only on Linux, so they never exist in a macOS build regardless of
features. Virtual is the one protocol with no gate at all — it is always
available, and it is the right first target for trying out rules and
mappings before real hardware is involved.

The rule of thumb: **if a channel fails to create, check the feature gate
first.** The factory's error is literal about it — `Unsupported protocol:
{name}. Check if the required feature is enabled.` — and the cause is almost
always a protocol that was not compiled in, not a configuration typo.

## Mapping points to instances

Channel points are protocol-flavored (register 62001 on channel 2); rules
and dashboards want model-flavored values (battery pack state of charge).
The bridge is an instance plus routing:

1. **Define the instance.** An instance binds a device to a product
   template in `config/automation/instances.yaml`. The default distribution
   intentionally starts empty; optional examples live under
   `packs/energy/examples/config/automation/instances.yaml`:

   ```yaml
   instances:
     pcs_01:
       product_name: PCS
       name: "PCS #1"
       properties:
         rated_power: 500.0
         rated_voltage: 380.0
   ```

   The product defines which measurement points and action points the
   instance has; the properties fill in the template's static values.

2. **Map channel points to instance points.** Routing wires a channel point
   to an instance point: telemetry and signal points feed instance
   measurement points (M, the `route:c2m` table), and instance action
   points (A) drive channel control and adjustment points (`route:m2c`). Entries can
   be created through the CLI:

   ```bash
   aether routing create 1 --point-type M --point-id 9 \
     --channel-id 1 --four-remote T --channel-point-id 101
   ```

   which calls `POST /api/instances/{id}/routing` on automation, or in bulk with
   `aether routing batch`.

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
