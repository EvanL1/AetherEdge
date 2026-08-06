---
title: Protocol Adapter Reference
description: Exact AetherEdge IO feature gates, runtime protocol IDs, transport roles, point mappings, security boundaries, and implemented protocol slices
updated: 2026-08-06
---

# Protocol Adapter Reference

This page describes the protocol adapters that can be statically composed into
`aether-io`. It is a source reference, not proof that a particular installed
binary contains every adapter. The installed `runtime-manifest.json` and the
live IO protocol catalog are authoritative for that binary.

Every channel is disabled until explicitly commissioned. Remote applications
must enter through authenticated `aether-api:6005`; they must not connect to
the loopback IO management API, write shared memory, or invoke a field protocol
directly.

## Composition matrix

| Cargo feature | Runtime protocol ID | Transport role | Point types | Default build | Evidence | Implemented slice |
|---|---|---|---|---:|---|---|
| `modbus` | `modbus_tcp`, `modbus_rtu` | TCP/serial client | T/S/C/A | yes | loopback | Modbus polling and governed writes |
| `iec104` | `iec104` | TCP client | T/S/C/A | no | loopback | IEC 60870-5-104 client |
| `iec61850` | `iec61850` | TCP/ISO client | T/S/C/A | yes | unit | MMS client; see adapter-specific control constraints |
| `opcua` | `opcua` | TCP client | T/S/C/A | no | unit | Anonymous `SecurityPolicy::None` sessions only |
| `bacnet` | `bacnet_ip` | UDP client | T/S/C/A | no | loopback | Direct ReadProperty/WriteProperty |
| `dl645` | `dl645` | Serial or transparent TCP client | fixed telemetry set | no | unit | DL/T 645-2007 meter reads |
| `cjt188` | `cjt188` | Serial or transparent TCP client | T/S | no | unit | CJ/T 188 data reads |
| `iec101` | `iec101` | Serial or transparent TCP master | T/S | no | unit | Unbalanced general interrogation |
| `gb32960` | `gb32960` | TCP server | T/S | no | loopback | Allow-listed vehicle telemetry reports |
| `jt808` | `jt808` | TCP server | T/S | no | loopback | Authenticated terminal location reports |
| `mqtt` | `mqtt` | Broker subscriber | T/S | no | unit | Read-only JSON payload acquisition |
| `http` | `http` | HTTP polling client | T/S | no | loopback | Read-only JSON polling |
| `can` | `can` | Linux SocketCAN reader | T/S | yes | unit | Raw CAN frame decoding |
| `j1939` | `j1939` | Linux SocketCAN reader | T/S | no | unit | Compiled SPN decoder catalog; implies `can` |
| `gpio` | `di_do` | Linux GPIO | S/C | yes | unit | `gpiod` or legacy sysfs driver |
| `ble` | `ble` | BLE GATT client | T/S/C/A | no | unit | Polling/notifications and governed characteristic writes |
| `zigbee` | `zigbee` | TCP gateway client | T/S | no | unit | Aether Raw TCP framing, not ZNP or EZSP |

The default feature set remains industry-neutral:

```text
modbus, gpio, iec61850, can
```

CAN, J1939, and GPIO are advertised only on Linux. Enabling a Cargo feature on
another target does not create a false runtime capability. Build tooling
derives the protocol list from the exact feature set and records it in the
checksummed runtime manifest.

## Evidence tiers

The `Evidence` column reports how far each adapter's recorded verification
currently goes. The tiers form a ladder; each includes the previous one:

| Tier | Meaning |
|---|---|
| `unit` | Codec, parser, configuration, and factory logic under automated unit test. No live peer exchange. |
| `loopback` | Automated message exchange against a local in-repository peer: a loopback TCP/UDP/HTTP test server or the protocol simulator. |
| `real-device` | Verified against at least one physical vendor device, with the device model and firmware recorded. |
| `field-proven` | Commissioned in a sustained real deployment with audit history. |

No adapter currently claims `real-device` or `field-proven` in this
repository. Those tiers are deployment-specific evidence and are upgraded per
adapter only when that evidence is recorded. A tier describes the implemented
software slice named in the matrix, never conformance to the whole protocol
standard.

## New adapter configuration

All parameter objects reject unknown fields. Timing values are milliseconds.
The examples are intentionally disabled and omit point tables for clarity.

### BACnet/IP

```yaml
- id: 20
  name: building-controller
  protocol: bacnet_ip
  enabled: false
  parameters:
    host: 192.168.20.10
    port: 47808
    local_port: 0
    timeout_ms: 2000
```

Point mappings use BACnet object/property addresses:

```json
{"object_type":0,"object_instance":7,"property_id":85,"array_index":null,"priority":8}
```

`array_index` is optional. `priority` is optional and must be in `1..=16` when
used for a governed C/A write. The adapter accepts the standard numeric and
boolean application values needed by Aether points. It does not implement
Who-Is/I-Am discovery, BBMD management, foreign-device registration,
BACnet/SC, COV subscriptions, or an in-kernel BACnet object catalog.

Each request waits for the reply carrying its own invoke ID and discards
anything else within the configured timeout, so a device that answers a
request the channel already abandoned costs one read rather than every read
after it. A C/A write whose value cannot be represented as a 32-bit BACnet
Real is refused instead of being narrowed to an infinity.

### CJ/T 188

Exactly one of `host` and `device` is required:

```yaml
- id: 21
  name: heat-meter
  protocol: cjt188
  enabled: false
  parameters:
    device: /dev/ttyUSB0
    baud_rate: 2400
    parity: even
    timeout_ms: 2000
    meter_type: 32
    meter_address: "12345678901234"
```

`meter_address` contains exactly 14 decimal digits.

A transparent TCP gateway uses `host` and optional `port` instead of
`device`; the default port is `8899`. T/S mappings select a data identifier and
a typed slice:

```json
{"data_id":36895,"byte_offset":0,"data_type":"bcd_le","byte_length":4}
```

Supported `data_type` values are `bcd_le`, `u8`, `u16_le`, `u32_le`,
`i16_le`, `i32_le`, and `f32_le`. A BCD mapping requires `byte_length` in
`1..=8`; fixed-width types reject it. Valve and meter-configuration commands
are not exposed.

### IEC 60870-5-101

Exactly one of a serial device and a transparent TCP gateway is required:

```yaml
- id: 22
  name: substation-rtu
  protocol: iec101
  enabled: false
  parameters:
    device: /dev/ttyUSB1
    baud_rate: 9600
    parity: even
    timeout_ms: 3000
    link_address: 1
    link_address_size: 1
    common_address: 1
    common_address_size: 2
    cause_size: 2
    ioa_size: 3
```

TCP gateway mode replaces `device` with `host` and optional `port` (default
`2404`). T/S mappings use an information-object address and ASDU type:

```json
{"ioa":100,"type_id":13}
```

`type_id: 0` accepts any supported measurement type for the IOA. Implemented
measurement ASDUs are `1`, `3`, `5`, `7`, `9`, `11`, `13`, and `15`, plus
their CP56Time2a forms `30` through `37`. The adapter is an unbalanced master
that performs general interrogation; it does not expose IEC 101 control
commands or balanced-mode station behavior.

Point quality comes from each ASDU's own descriptor. Types other than the
integrated total carry a QDS, where invalid means `Bad` and not-topical,
substituted, or blocked mean `Uncertain`. An integrated total (`15` and `37`)
instead ends in BCR sequence notation, where only invalid, carry, and
counter-adjusted describe the reading and the low five bits are a sequence
number that carries no quality meaning.

### GB/T 32960

This adapter is a terminal-originated TCP server. It binds to loopback by
default and accepts exactly one VIN:

```yaml
- id: 23
  name: vehicle-telemetry
  protocol: gb32960
  enabled: false
  parameters:
    bind: 127.0.0.1:32960
    allowed_vins:
      - L1234567890123456
```

Mappings select a decoded report field:

```json
{"field":"soc_percent"}
{"field":"drive_motor_speed_rpm","motor_index":0}
```

Available fields cover whole-vehicle state, speed, mileage, voltage/current,
SOC, gear, insulation, accelerator/brake state, position, alarm level, and
drive-motor measurements. `motor_index` is required only for a drive-motor
field. The adapter validates BCC, accepts unencrypted real-time and reissued
reports, acknowledges valid terminal frames, and rejects unlisted VINs and
unsupported encryption before publication. It skips recognized standard data
blocks that have no configured field mapping rather than misparsing the rest
of a mixed report.

### JT/T 808

This adapter is also a terminal-originated TCP server. It binds to loopback by
default and accepts exactly one terminal, which must present a pre-provisioned
token:

```yaml
- id: 24
  name: positioning-terminals
  protocol: jt808
  enabled: false
  parameters:
    bind: 127.0.0.1:6808
    auth_tokens:
      "013800138000": replace-with-provisioned-token
```

Terminal IDs contain 12 digits for the 2013 header or 20 digits for the 2019
header. T/S mappings select a decoded location field:

```json
{"field":"speed_kmh"}
{"field":"acc_on"}
```

The adapter handles registration (`0x0100`), authentication (`0x0102`),
location reports (`0x0200`), escaping, XOR checksums, and platform responses.
It rejects location data before authentication. Encryption and subpackage
reassembly are deliberately unsupported and fail closed. Available mappings
cover the location core, status bits, mileage, fuel, recorder speed,
temperature, signal/IO state, analog inputs, wireless signal, and GNSS
satellite count.

### Dial-in server bounds

GB/T 32960 and JT/T 808 are the only adapters that accept connections instead
of opening them.

Each channel carries exactly one vehicle: `allowed_vins` must name one VIN and
`auth_tokens` must name one terminal. A point on these channels selects a
report field and carries no vehicle selector beside it, so two vehicles on one
channel would write the same point IDs and the later report would silently
overwrite the earlier one with another vehicle's readings. Give each vehicle
its own channel on its own listen port. Both fields stay collections so that
per-point vehicle selection can relax this later without invalidating
configurations written today.

Both adapters also apply the same fixed bounds to what an unknown peer can
take:

- One channel holds at most 64 concurrent terminal connections. A peer past
  that limit is closed at accept and recorded in channel diagnostics.
- A connection that has not yet named an allow-listed VIN or passed
  authentication is closed after 15 seconds of silence and recorded as an
  error. Once the terminal has identified itself, its idle bound becomes
  120 seconds and a timeout closes the connection without raising an error.
- An `accept` failure caused by one peer never stops the listener. Any other
  `accept` failure ends the accept loop and moves the channel to `Error`, so
  the channel supervisor rebuilds it under the configured reconnect policy.
- A JT/T 808 terminal that supplies an incorrect token receives one failure
  response and then loses the connection, so a configured terminal ID cannot
  be used as an unlimited guessing oracle.
- A point whose configured transform cannot produce a finite value is skipped
  and recorded; the rest of the report is still published and the terminal
  keeps its connection.

These bounds are not configurable.

## Existing DL/T 645 behavior

The repository already contains a DL/T 645-2007 adapter. A channel requires a
12-digit BCD `meter_address` and exactly one of `host` or `device`. TCP defaults
to port `8899`; serial defaults to 2400 baud, 8 data bits, one stop bit, and
even parity.

The current runtime exposes a fixed telemetry catalog for total forward and
reverse active energy, phase voltage/current, total active/reactive power, and
power factor. It does not accept arbitrary inline point mappings. This is a
deliberate compatibility boundary, not a generic DL/T 645 model catalog.

## COMTRADE files

COMTRADE is an offline CFG/DAT format, not a live IO channel and not a shared
memory writer. Use the CLI to validate metadata or normalize samples:

```bash
aether comtrade inspect --cfg disturbance.cfg
aether --json comtrade inspect --cfg disturbance.cfg --dat disturbance.dat
aether comtrade export-csv \
  --cfg disturbance.cfg \
  --dat disturbance.dat \
  --output disturbance.csv
```

When `--dat` is omitted, the CLI uses the CFG path with a `.dat` suffix. It
supports ASCII, BINARY, BINARY32, and FLOAT32 records, unpacks digital bits,
applies each analog channel's `a`/`b` engineering conversion, and applies the
COMTRADE time multiplier.

## Deliberate exclusions

- **HL7 is not an IO adapter.** An out-of-tree healthcare integration may
  translate HL7 v2, FHIR, or another application message into Aether domain
  commands and queries, then enter through authenticated `aether-api`. It may
  not add a remote listener to `aether-io` or write SHM directly.
- **COMTRADE is not advertised as a protocol.** Importing a disturbance file
  does not establish a live channel.
- **Protocol-specific model catalogs stay outside the kernel.** A downstream
  static Rust composition may consume published SDK, port, and testkit
  contracts, but it cannot bypass governed commands or live-state authority.

## Commissioning and verification

The `Evidence` column in the composition matrix records each adapter's
current verification tier. Even a `loopback` tier is not a substitute for a
device profile, vendor point table, serial/electrical validation, or
interoperability certification.

Before enabling a physical channel:

1. inspect the installed runtime manifest and live protocol catalog;
2. validate the exact device revision, transport settings, and mapping table;
3. keep inbound listeners on a dedicated field network and firewall them;
4. create the channel disabled and prove read-only data first;
5. verify timestamps, quality, freshness, and scaling against the device;
6. commission C/A points only after permission, range, step, confirmation,
   audit, and physical-outcome checks are in place.

See [Connect Devices](../guides/connect-devices.md) for the complete channel
lifecycle and [Configuration Reference](configuration.md) for configuration
authority and synchronization.
