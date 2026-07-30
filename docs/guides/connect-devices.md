---
title: Connect Devices
description: Configure channels, choose protocols, and map device points to instances
updated: 2026-07-30
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
```

The `parameters` block is protocol-specific: Modbus TCP wants a host and
port, Modbus RTU wants a serial device and baud rate, MQTT wants a broker URL
and subscription topics, and so on. The registry matches canonical names and
aliases case-insensitively while ignoring `-`, `_`, `.`, and spaces, so
`modbus-tcp`, `ModbusTCP`, and `modbus_tcp` all resolve to the same protocol
without allocating a normalized string.

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

IO advertises only protocols that the same binary can construct through its
production composition root. Most are behind compile-time Cargo features
(`services/io/Cargo.toml`), so a given binary usually contains only a subset.
The default feature list selects Modbus, GPIO, Aether-485, IEC 61850, and CAN.
The production registry composes CAN and GPIO only on Linux: a Linux default
build advertises all five, while a non-Linux default build does not advertise
CAN or GPIO.

| Protocol | Selected by default | Platform notes |
|----------|--------------------:|----------------|
| Modbus TCP/RTU (`modbus`) | yes | |
| IEC 60870-5-104 (`iec104`) | no | |
| IEC 61850 MMS (`iec61850`) | yes | |
| OPC UA (`opcua`) | no | Optional feature; currently restricted to anonymous `SecurityPolicy::None` sessions. |
| MQTT (`mqtt`) | no | event-driven, read-only JSON payload acquisition |
| HTTP (`http`) | no | read-only HTTP JSON polling; the unified channel task owns scheduling |
| DL/T 645-2007 (`dl645`) | no | smart meters over serial or TCP |
| CAN (`can`) / J1939 (`j1939`) | CAN yes, J1939 no | Linux only; `j1939` implies `can` |
| GPIO (`gpio`) | yes | Linux only |
| BLE GATT (`ble`) | no | notification/poll acquisition plus governed GATT writes |
| Zigbee (`zigbee`) | no | Aether Raw TCP gateway framing only |
| Aether-485 (`aether_485`) | yes | private RS-485 protocol |

CAN, J1939, and GPIO are Linux-gated in the IO protocol factory registry
(`services/io/src/protocols/factory.rs`), so selecting their Cargo features
does not advertise a runtime on macOS. Hardware-independent protocol tests run
against `tools/simulator`; the production IO binary contains no in-memory
simulation protocol.

The rule of thumb: **if a channel fails to create, check the feature gate
first.** Channel activation reports that the protocol is unavailable in the
current IO runtime build when its adapter was not composed.

One statically composed, process-wide immutable factory registry owns
discovery, strict parameters, point mapping validation, aliases, polling
defaults, and runtime construction. `ChannelManager` holds no SQLite pool and
performs no protocol switch or mapping prepass; it adds only common runtime
policy, logging, command guards, lifecycle/task ownership, and SHM wiring. The
signed runtime manifest is checked for exact agreement with this registry. A
feature that merely compiles but cannot construct a channel is therefore a
test failure, not an advertised capability. The former `matter` feature was
removed because its private UDP frame was not an interoperable Matter
implementation; see
[ADR-0027](../adr/0027-exact-io-protocol-capabilities.md).

## Point mapping contracts

Each activation consumes an owned, complete `RuntimeChannelConfig` snapshot.
The selected factory compiles adapter-owned mapping schemas into
adapter-owned typed addresses once; there is no shared closed protocol-address
union. Protocol runtimes do not own a SQLite pool or reload topology later.

| Protocol | Accepted point mappings |
|----------|-------------------------|
| MQTT / HTTP | T uses `{"json_path":"$.value","data_type":"float"}` (`float` or `int`); S uses `{"json_path":"$.online","data_type":"bool"}`. C/A and string mappings are rejected. |
| BLE | `{"service_uuid":"180f","characteristic_uuid":"2a19","data_format":"uint16","notify":true}`. Notifications are T/S only; C/A use governed writes. |
| Zigbee | T/S require `{"ieee_address":...,"endpoint":1,"cluster_id":1026,"attribute_id":0}`. C/A are rejected because the raw gateway transport has no correlated command acknowledgement yet. |
| J1939 | T uses `{"spn":190}`. S additionally requires an explicit raw discrete value, for example `{"spn":110,"active_raw_value":130}`. |

Scale, offset, and reverse come from the point row rather than being repeated
inside these mapping objects. Unknown mapping fields fail closed. MQTT/HTTP
accept only finite values that can enter live state, J1939 accepts only SPNs in
the compiled decoder catalog, and Zigbee accepts only the implemented Aether
Raw TCP framing—there is no `gateway_type` selector for unimplemented ZNP or
EZSP transports. The SHM acquisition boundary accepts only finite numeric or
boolean T/S samples and canonical point quality; text, missing, and non-finite
values never enter live state.

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
   be created through the authenticated CLI. Read the current
   `logical_routing_revision` with `aether routing list`, then submit one
   revision-fenced route:

   ```bash
   AETHER_ACCESS_TOKEN='<signed access JWT>' \
     aether routing measurement upsert 1 9 \
       --channel-id 1 --channel-type T --channel-point-id 101 \
       --expected-revision 7 --confirmed
   ```

   This submits one governed routing command through `aether-api`. Successful
   responses return the revision required by the next mutation.

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
