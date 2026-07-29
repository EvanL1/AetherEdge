# ADR-0035: Align IO composition with the runtime manifest

## Status

Accepted and implemented on 2026-08-05.

## Context

The maintained distribution compiled MQTT and HTTP and listed them in its
runtime manifest, but `ChannelManager` had no branch that could instantiate
either adapter. GPIO had both gpiod and sysfs implementations, while the
composition root always selected sysfs and ignored the reviewed driver
parameter. The service also exposed a separate `/api/protocols` registry that
listed only Modbus, CAN, and GPIO, contradicting the runtime manifest.

JSON adapters independently reread four physical point tables after the IO
configuration loader had already read them. Those reads could observe a
different SQLite generation. Browser-oriented channel search, minimal-list,
global-point, and type-specific point aliases had no production consumers and
duplicated the canonical channel and point queries.

## Decision

1. Every MQTT or HTTP feature selected by a distribution has a concrete
   `ChannelRuntime` factory. Invalid broker, subscription, URL, or timing
   parameters fail before desired state commits. ADR-0042 later replaced
   per-protocol `ChannelManager` branches with one statically composed factory
   registry.
2. MQTT is event-driven and HTTP is outbound polling only. Both receive a JSON
   mapper compiled from the same `RuntimeChannelConfig` generation as the rest
   of the channel; adapters do not query SQLite.
3. The IO topology loader reads T/S/C/A point rows in one SQLite transaction.
4. GPIO's reviewed `driver` parameter explicitly selects `gpiod` or `sysfs`.
   Omission preserves the historical effective sysfs selection; new sites may
   explicitly commission gpiod.
5. The runtime manifest is the sole compiled-protocol catalog. The incomplete
   `/api/protocols` registry and its metadata types are retired.
6. `/api/channels` and `/api/channels/{id}/points` remain the canonical query
   surfaces. The zero-consumer `/api/channels/list`, `/api/channels/search`,
   `/api/points`, and four type-specific point-configuration aliases are
   retired. Mapping and authoritative SHM point reads remain available.

## Consequences

- Distribution feature claims now correspond to instantiable runtime behavior.
- One channel activation cannot mix physical point generations.
- GPIO backend ownership is explicit without silently changing existing sites.
- Offline documentation and the signed runtime manifest replace a second,
  incomplete online protocol catalog.
- Protocol implementations, governed channel lifecycle, SHM authority, channel
  diagnostics, and device command safety remain unchanged.

## Verification

- All-feature IO tests construct concrete MQTT and HTTP runtimes through the
  static protocol registry.
- Validation tests reject inert MQTT and HTTP configurations before persistence.
- GPIO tests cover both reviewed backend selections.
- OpenAPI tests reject retired aliases and preserve canonical queries.
- Architecture tests require MQTT/HTTP branches, transactional topology reads,
  and mapper construction without a second database read.
