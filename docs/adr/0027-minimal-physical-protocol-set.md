# ADR-0027: Keep only the distribution-owned physical protocol set

## Status

Accepted on 2026-07-27.

## Context

AetherEdge was extracted from VoltageEMS, but its Rust implementation became
larger than the source product. Production IO still retained every historical
compile-time protocol adapter even when the standard runtime manifest did not
ship it, no downstream release consumed it, or its security and interoperability
work was incomplete. Compile-time optionality reduced binary size but did not
reduce kernel ownership, maintenance, dependency, test, and audit cost.

The standard AetherEMS-compatible runtime composition already selects a smaller
set: Modbus TCP/RTU, MQTT, HTTP, raw CAN, GPIO, IEC 61850 MMS, and Aether-485.
Those protocols cover the current distribution and simulator evidence. Keeping
additional implementations as workspace shelfware contradicts ADR-0026's
minimal-kernel rule.

## Decision

1. AetherEdge owns only these IO protocol features and runtime identifiers:
   - `modbus` (`modbus_tcp`, `modbus_rtu`);
   - `mqtt`;
   - `http`;
   - `can` on Linux;
   - `gpio` (`di_do`) on Linux;
   - `iec61850`; and
   - `aether_485`.
2. The in-tree BLE, DL/T 645, IEC 60870-5-104, J1939, Matter, OPC UA, and Zigbee
   adapters, address DTOs, feature flags, dependencies, metadata, and mapping
   validators are removed. The runtime manifest rejects those feature names
   instead of advertising dormant capability.
3. The unconsumed second channel factory, generic gateway configuration DTOs,
   and string-address parser are removed. Service composition continues through
   the one `ChannelManager` path and the object-safe `ChannelRuntime` boundary.
4. Existing site configuration naming a removed protocol fails as unavailable.
   It is never mapped to another protocol or treated as a generic address.
5. Historical ADRs and external compatibility fixtures may retain old protocol
   names as records. They do not grant current runtime support.
6. Reintroducing one of these protocols requires a real downstream distribution,
   explicit security/interoperability evidence, and an accepted composition
   decision. AetherEdge will not add a speculative dynamic plugin host.

## Consequences

- Production IO and its all-features graph become smaller and easier to audit.
- The runtime manifest describes the maintained distribution rather than the
  union of every historical experiment.
- Sites using a removed adapter must remain on their existing release or provide
  a separately maintained downstream Rust composition before upgrading.
- Protocol breadth is no longer used as a product-readiness metric; verified
  acquisition and command behavior for the retained set takes priority.

## Verification

- `aether-io --all-features` compiles without a removed protocol dependency.
- Runtime-manifest tests reject retired feature identifiers and advertise only
  the retained target-appropriate adapters.
- Architecture tests prevent restoration of retired adapter sources or Cargo
  features.
- The existing Modbus simulator E2E remains the hardware-independent golden
  data-path proof.
