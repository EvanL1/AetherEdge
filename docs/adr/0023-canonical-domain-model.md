# ADR-0023: Make `aether-domain` the canonical business-model owner

## Status

Accepted and implemented on 2026-07-26. ADR-0026 supersedes references to an
in-repository SunSpec adapter; canonical domain ownership remains unchanged.

## Context

The repository contained two generations of business-shaped types.

`aether-domain` is the dependency-bottom, `no_std` semantic model. It owns
identities, point addresses, samples, commands, alarms, integrations, and data
processing values. Its types encode invariants such as the separation between
acquisition-owned T/S addresses and command-owned C/A addresses.

The former `aether-model` crate was a legacy compatibility layer. Alongside
`PointType` and `PointRole`, it contained Redis key-space configuration,
SQLx-backed validation, product definitions, and SunSpec material. It
re-exported types from `aether-core`. Those concerns did not belong in one
business model.

The names are similar but the values are not mechanically interchangeable. For
example, legacy `PointType` uses the storage/protocol codes T/S/C/A, while
`PointKind` expresses the semantic roles Telemetry/Status/Command/Action.
Legacy quality values and point identifiers likewise require an explicit,
tested conversion at each compatibility boundary.

The Linux workspace no longer contains `aether-core`. Embedded wire codecs and
SHM ABI primitives are owned by the separately targeted `firmware/aether-core`
crate. The stable T/S/C/A configuration DTO is owned by `common`; neither is a
second business-model authority.

## Decision

1. `aether-domain` is the sole owner of industry-neutral business semantics
   and invariants. Application and port contracts use domain types; wire,
   storage, protocol, and Pack representations remain explicit DTOs in their
   owning boundary rather than becoming a second semantic model.
2. `firmware/aether-core` retains firmware-oriented codec and layout concerns
   inside the nested firmware workspace. Linux service configuration and wire
   DTOs stay in their adapter owner and must not become business entities.
3. `aether-model` is retired and removed. No production crate may restore a
   dependency or import. Cargo-metadata architecture tests enforce the absent
   package, path, and dependency; normal Rust compilation rejects stale imports.
4. Compatibility adapters remain narrow and convert explicitly between stable
   storage/protocol values and domain values; no blanket type aliases or
   implicit semantic conversions are permitted.
5. Legacy concerns move according to ownership rather than being copied into
   `aether-domain`:
   - Redis key-space material is retired with the RTDB compatibility path or
     remains inside an optional mirror adapter.
   - protocol-specific SunSpec material belongs in its protocol adapter or extension.
   - product definitions belong in validated Packs or their dedicated loading
     boundary.
   - transport/database validation remains in adapters or application services.
6. Migrations proceed by vertical slice with behavior and compatibility tests:
   first define a domain value and conversion test, then migrate one command,
   query, protocol adapter, or persistence adapter, and finally remove the
   legacy type and its consumer.
7. Existing `aether-rtdb*.shm` file names and `/shm/rtdb` mount labels remain
   stable on-disk compatibility identifiers. They do not restore the retired
   RTDB model or authority; a separate compatibility decision is required
   before renaming them.

## Consequences

The migration was not a global rename: database, protocol, and OpenAPI
representations remain stable at explicit compatibility boundaries.

Implementation removed every production dependency on `aether-model` and then
removed the crate. T/S/C/A storage, CSV, HTTP, and protocol representation lives
in `common`; firmware codecs and ABI layouts live only under `firmware/`;
product-directory contracts live in `aether-pack`; protocol-specific catalogs
stay downstream; command finiteness and instance-name invariants are enforced
by `aether-domain`.
