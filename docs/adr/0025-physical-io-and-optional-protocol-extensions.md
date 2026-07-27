# ADR-0025: Keep production IO physical and move protocol catalogs to optional extensions

## Status

Accepted and implemented on 2026-07-26. Decision 4 and its in-repository
extension migration are superseded by ADR-0026; the remaining Rust-only,
physical-IO, host-network, routing, and simulation decisions stay in force.

## Context

Production IO retained several compatibility surfaces that were not part of a
physical acquisition runtime:

- an in-process `virtual` channel advertised by every build and runtime
  manifest;
- a SunSpec model catalog embedded directly in the production IO crate;
- host-network handlers that wrote systemd-networkd files and launched
  `networkctl` from the IO HTTP interface;
- a Python transform host and an installer `py` service-group alias; and
- a routing cache that still exposed representation-layer point types and
  zero-consumer dispatch and batch APIs.

The virtual adapter made simulation look like a production protocol even
though the repository owns a separate protocol simulator. The host-network
routes were not application capabilities and therefore had no shared
permission, confirmation, audit, or reconciliation contract. SunSpec data made
every default IO build carry a protocol-specific catalog even after the legacy
model crate was retired. The remaining Python and routing surfaces described
retired architectures.

## Decision

1. Production `aether-io` contains only explicitly compiled physical protocol
   adapters. The `virtual` identifier, adapter, address type, mapping,
   metadata, documentation, and runtime advertisement are removed.
2. Hardware-independent tests use `tools/simulator` over a real protocol such
   as Modbus. No debug-only or hidden in-process simulation adapter is retained
   in the production service.
3. Site configurations that name `virtual` must be migrated before upgrade.
   They fail as an unavailable protocol afterward. This is intentional:
   silently mapping one adapter to another could cross the wrong device
   boundary.
4. SunSpec models and expansion logic live in the default-off `aether-sunspec`
   extension. `aether-io/sunspec` explicitly composes that extension and
   implies the Modbus transport feature. Default builds and manifests do not
   advertise SunSpec.
5. IO no longer configures the host operating system or starts `networkctl`.
   Host networking is an installer/operator responsibility. Any future remote
   network-management capability requires ports, an application command,
   authorization, confirmation, audit, and a platform adapter before an HTTP
   route can expose it.
6. The production Python transform host stays retired. Python is limited to
   simulator and test tooling. The installer no longer accepts `py` or
   `dev-py` compatibility groups.
7. Following the `KeySpaceConfig` removal in ADR-0023, `aether-routing` uses
   canonical domain point kinds, drops its representation-layer dependency and
   zero-consumer APIs, and keeps private stable logical-route keys for SQLite
   snapshots. Those keys are not a Redis authority or public protocol.

## Compatibility and migration

Before upgrading, operators must replace each `virtual` channel with a
commissioned physical protocol configuration or move the scenario to
`tools/simulator`. They must also stop calling the retired internal
host-network endpoints and provision the host out of band.

SunSpec distributions opt in with the `sunspec` IO feature and publish a
runtime manifest generated from that exact feature set. Configurations
containing SunSpec aliases are rejected by builds that do not advertise the
feature.

Pinned upstream contract fixtures may retain historical protocol names as
external compatibility evidence. They do not grant runtime support.

## Consequences

- Default IO builds are smaller and expose only capabilities they implement.
- Simulation can exercise protocol framing without becoming a production
  control path.
- Vendor model data is isolated behind a composition-root choice.
- Host mutation cannot bypass the application safety model through an internal
  IO route.
- The removal is deliberately visible rather than a silent compatibility
  downgrade.
- Adding another simulated adapter, script host, host-network route, legacy
  keyspace type, or default SunSpec advertisement requires superseding this
  ADR.

## Verification

Architecture checks enforce the Rust-only IO boundary, absence of the
in-process simulation and host-network surfaces, SunSpec extension ownership,
default-off feature composition, canonical routing ownership, and installer
alias retirement. Runtime-manifest contract tests verify the exact default and
feature-selected protocol set.
