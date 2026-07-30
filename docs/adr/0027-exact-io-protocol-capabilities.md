# ADR-0027: Publish only activatable IO protocol capabilities

## Status

Accepted and implemented on 2026-07-29.

## Context

An IO Cargo feature, the runtime manifest, protocol discovery, and the
production channel composition root described different protocol sets. Several
optional adapters compiled in isolation but could not be activated from the
immutable SQLite channel snapshot:

- MQTT and HTTP tried to reopen point mappings through an unset adapter-owned
  SQLite pool;
- BLE and Zigbee were present in discovery but absent from the production
  channel composition;
- J1939 decoded SPNs without projecting them onto governed channel point IDs;
  and
- the `matter` feature implemented a private five-byte UDP frame rather than
  Matter sessions, Interaction Model TLV, or ReportData. It could not
  interoperate with a conforming Matter device.

Advertising compiled but unreachable code is a false runtime capability.
Keeping an incomplete wire protocol behind an opt-in feature is also unsafe:
configuration can appear valid while no real device can satisfy it.

## Decision

1. A protocol is advertised only when the same build can discover it and
   construct its production `ChannelRuntime`. The signed runtime manifest,
   protocol registry, and production composition root must describe the same
   canonical protocol types for the current target. CAN, J1939, and GPIO remain
   absent from a non-Linux registry even if their Cargo features are selected.
2. All explicitly composed protocol runtimes consume the complete immutable
   `RuntimeChannelConfig` snapshot. Full reconciliation loads channels plus the
   four typed point tables in one transaction with a fixed five scans, passes
   each owned snapshot by value through runtime projection, and verifies the
   result with a fixed two-scan channel-authority/tombstone witness. Protocol
   runtimes do not own a SQLite pool or reload point topology after activation
   begins.
3. The SHM acquisition boundary accepts only finite numeric or boolean T/S
   samples with canonical point quality. Text, missing, and non-finite values
   fail closed. MQTT and HTTP compile their JSON mappings from the snapshot;
   command-point, string, cross-point-type, and duplicate transform fields fail
   closed before activation.
4. HTTP is a polling adapter. Its uncomposed webhook handler and duplicate
   background polling loop are removed. MQTT remains event driven. Process-wide
   reconnect ownership stays in the channel runtime rather than being
   duplicated in adapter configuration. HTTP redirects are disabled and its
   response body remains bounded even without `Content-Length`.
5. BLE owns its GATT mapping schema. Zigbee is a read-only T/S acquisition
   adapter over the implemented raw TCP gateway framing. Its previous control
   path reported success after a socket write without correlating the gateway
   response, so C/A mappings and outbound command encoding are removed until
   an acknowledged command transaction exists. The unused gateway selector
   and its unimplemented ZNP and EZSP variants are also removed.
6. J1939 maps explicitly configured SPNs onto governed telemetry or signal
   point IDs before emitting acquisition samples. Mappings must reference the
   compiled decoder catalog, and signal mappings declare an explicit
   `active_raw_value`; error/not-available encodings are not converted into a
   good signal. The adapter does not publish decoder SPN identifiers directly
   into SHM.
7. The `matter` Cargo feature, protocol identifier, discovery entry, runtime
   advertisement, private frame implementation, and configuration DTO are
   removed. A future Matter adapter requires a separate decision and a
   conforming Rust implementation of commissioning/session security,
   Interaction Model encoding, subscription reports, and simulator or device
   interoperability tests.
8. One statically composed, process-wide immutable protocol factory registry
   owns each canonical identifier, compatibility aliases, discovery metadata,
   strict channel validation, mapping validation, default polling interval,
   and synchronous runtime builder. Each adapter owns its mapping schema and
   typed addresses and compiles them once during its selected builder.
   `ChannelManager` holds no SQLite pool and performs no protocol switch or
   mapping prepass; it owns only common lifecycle wiring. The runtime manifest
   remains an independent release contract and is checked for exact parity
   with the registry rather than becoming an IO-library dependency.

## Compatibility and migration

Existing MQTT, HTTP, BLE, Zigbee, and J1939 channels must use the narrowed
parameter and mapping contracts described above. Previously accepted fields
that had no runtime effect are rejected instead of being silently ignored.
In particular, Zigbee no longer accepts `gateway_type` or C/A mappings, and
J1939 signal mappings require `active_raw_value`.

Configurations naming `matter` fail with the normal unavailable-protocol error
after upgrade. There is no compatibility shim because mapping the identifier
to the former private UDP framing would continue the false capability, while
mapping it to another protocol could target the wrong device. Operators must
remove those channels or keep them outside AetherEdge until a conforming
adapter is separately accepted.

## Consequences

- Runtime capability metadata becomes executable evidence rather than a list
  of source files that happen to compile.
- Protocol adapters have one configuration generation and no hidden database
  ownership.
- Protocol lookup performs no normalized-string allocation, and channel
  activation creates only the heterogeneous `ChannelRuntime` trait object; the
  registry itself uses synchronous function pointers.
- Affected adapters expose fewer parameters, but every retained parameter has
  production behavior.
- Removing the pseudo-Matter code reduces apparent protocol breadth while
  preserving the actual device interoperability of the product.

## Verification

Runtime-manifest tests compare its canonical protocols with the protocol
factory registry for the exact compiled feature set. Registry tests enforce
canonical and alias uniqueness. A production-root test constructs every
discoverable driver example through its registered factory. Adapter tests
reject invalid or unsupported mapping and transport modes, and architecture
checks prevent a second runtime dispatch table or SQLite ownership from
returning to protocol runtimes.
