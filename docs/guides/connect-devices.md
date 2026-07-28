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
      read_timeout_ms: 3000

  - id: 3
    name: "SENSOR#1"
    protocol: "modbus_rtu"
    enabled: false
    parameters:
      device: "/dev/ttyS4"
      baud_rate: 9600
      read_timeout_ms: 3000
```

The `parameters` block is protocol-specific: Modbus TCP requires `host` and
`port`; Modbus RTU requires `device` and `baud_rate`; MQTT requires a plaintext
field-network `broker` URL plus at least one subscription; HTTP requires an
outbound polling `url` and optionally `poll_interval_ms` (the historical
`interval_ms` spelling remains accepted). Protocol names are normalized before
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
(C, digital command), and adjustment (A, analog setpoint). Point definitions
and protocol mappings are authored as reviewed configuration and applied
atomically with `aether sync --confirmed` while runtime owners are stopped.
Online channel point APIs are read-only.

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
| HTTP (`http`) | no | Outbound polling only; implies `json-mapping` |
| CAN (`can`) | yes | Linux only |
| GPIO (`gpio`) | yes | Linux only |
| Aether-485 (`aether_485`) | yes | Private RS-485 protocol |

BLE, DL/T 645, IEC 60870-5-104, J1939, Matter, OPC UA, and Zigbee are not
kernel capabilities. A configuration naming one of them fails as unavailable;
it is never silently translated to another adapter.

CAN and GPIO are OS-gated in
`services/io/src/core/channels/channel_creation.rs` and exist only on Linux.
GPIO accepts an explicit channel parameter `driver: gpiod` or
`driver: sysfs`; omission preserves the historical sysfs selection. MQTT and
HTTP are concrete `ChannelRuntime` compositions, not build-only declarations.
Their JSON mappings are compiled from the same transactional point-topology
snapshot as the channel runtime.

Protocol availability is read from the signed runtime manifest rather than a
second service-local discovery endpoint. A channel fails before persistence
when its protocol is absent from that build or its required parameters are
invalid. Hardware-independent protocol tests run against `tools/simulator`;
the production IO binary contains no in-memory simulation protocol.

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
   measurement points (M, commissioned C2M routing), and instance action
   points (A) drive channel control and adjustment points through commissioned
   M2C routing. Author
   instances and measurement routes in the commissioned automation
   configuration. For one online physical action route, use the authenticated,
   explicitly confirmed `aether routing action upsert` command.

3. **Validate and apply offline configuration.** Run `aether sync --dry-run`,
   stop the runtime owners, then run `aether sync --confirmed`. Sync validates
   the complete configuration and writes it into SQLite, where the services
   load it. There is intentionally no partial CLI mutation for instance,
   measurement-routing, or point-topology configuration.

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

Then read a live value through the authenticated application client:

```bash
aether channels points <channel_id> --type T --json
aether models instances data <instance_id> --json
```

The IO query reads authoritative SHM internally. If the channel point updates
but the instance point does not, the routing
entry is missing or wrong. SHM is the authoritative live view, so no external
database needs to be running for this check.

What offline looks like: `aether channels status` reports
`connected: false`, the channel-health SHM entry becomes offline, and point
values stop updating — their timestamps go stale. A point that has *never*
been acquired is a NaN sentinel in shared memory, not a zero; see
[Data Model](../concepts/data-model.md) for why unavailability is a
first-class value. For a whole-system pass, check the host supervisor and all
six service health endpoints.

## Related pages

- [Data Model](../concepts/data-model.md) — products, instances, and the four point types
- [System Architecture](../concepts/architecture.md) — where io and automation sit and how data flows between them
- [Writing Rules](writing-rules.md) — putting mapped points to work in control logic
