# ADR-0026: Keep integrations outside the minimal edge kernel

## Status

Accepted on 2026-07-27. This decision supersedes the in-repository extension
ownership selected by ADR-0020 and decision 4 of ADR-0025. It narrows, but does
not otherwise replace, ADR-0017: CloudLink remains experimental until that
ADR's production gates close, while `aether-uplink` becomes its sole edge
runtime owner.

## Context

AetherEdge is the smallest industry-neutral AetherIoT edge kernel. Its default
distribution is six Rust processes on one Linux host, with SHM as the live-state
authority and no required broker, external database, browser, LLM, script host,
or protocol simulator.

The repository's `extensions/` directory no longer described one architecture
boundary. It mixed required kernel infrastructure (`aether-shm-bridge`,
`aether-store-local`, and JWT authentication), the native CloudLink MQTT
binding, optional third-party integrations, storage experiments, HTTP clients,
and the SunSpec model catalog. Some packages had no composition consumer,
while non-composition libraries depended directly on concrete SHM types.
Calling all of those packages extensions hid process ownership and allowed
parallel implementations to accumulate.

Extensibility does not require implementations to live in the kernel
repository. AetherEdge already exposes domain, port, application, SDK, Pack,
and testkit contracts that downstream Rust compositions can consume.

## Decision

1. AetherEdge does not own an `extensions/` source layer. The directory is
   removed and an architecture contract prevents its restoration. Protocol
   simulation and scripts remain under `tools/simulator`; they are not renamed
   extensions.
2. Required native infrastructure is internal kernel code:
   - JWT authentication moves to `libs/aether-auth-jwt`;
   - the SHM runtime bridge moves to `libs/aether-shm-bridge`; and
   - zero-external-service storage moves to `libs/aether-store-local`.
   Package identifiers remain stable during the path migration.
3. CloudLink is a native Aether protocol. `aether-cloudlink` retains
   transport-neutral protocol semantics, while `aether-uplink` is the sole
   owner of the MQTT transport, session, signing, spool, acknowledgement, and
   replay lifecycle. IO must not establish a CloudLink broker session.
4. History storage is owned by `aether-history`. Remote applications enter
   through `aether-api`; API uses the internal `HistoryQuery` boundary instead
   of opening the History service's SQLite database. Parallel, uncomposed
   Redis and PostgreSQL bridge packages are removed. The inherited generic
   infrastructure package, Redis client, retry/configuration helpers, warning
   monitor, and client-only CI job are also removed rather than retained as
   uncomposed shelfware. Its two used SQLite service-configuration reads move
   into `common` while unused generic SQLite wrappers are deleted.
5. Home Assistant is a downstream integration rather than kernel source. Its
   implementation is extracted from this repository. Industry-neutral domain
   and application contracts may remain only when they have a kernel consumer
   independent of Home Assistant. The uncomposed generic Integration domain
   model, provider/projection ports, synchronizer, generation store, candidate
   wire crate, and CloudLink Integration routes are therefore removed together;
   a future real consumer must introduce its narrow contract downstream first.
6. SunSpec is not shipped in AetherEdge. Its model catalog, discovery, and
   expansion code belong to a downstream, statically composed Rust IO plugin.
   Standard IO builds reject `sunspec` as unsupported. AetherEdge will add no
   speculative plugin host, dynamic-library ABI, script runtime, or child
   process merely to preserve the former implementation.
7. A future IO plugin contract requires a real downstream consumer and a
   separate decision. Such a plugin returns canonical topology and samples to
   IO, does not write SHM or storage directly, does not own host networking,
   and cannot bypass governed commands.
8. Concrete service-private HTTP clients stay with their owning composition
   root when they support a retained kernel capability. Unused adapters are
   removed rather than preserved as unpublished workspace shelfware.
9. Non-composition libraries depend on domain and port contracts, not concrete
   runtime implementations. Cross-adapter dependencies are removed. The SDK's
   explicitly documented `local-runtime` facade remains the compatibility
   exception accepted by ADR-0013 and ADR-0022.

## Migration

The migration is intentionally staged so every commit builds:

1. replace SHM-specific rule and topology dependencies with canonical contracts;
2. relocate required native packages without renaming their Cargo packages;
3. remove SunSpec and zero-consumer storage bridges;
4. restore History service ownership and the API-to-History query boundary;
5. move CloudLink MQTT ownership to Uplink and remove it from IO;
6. extract third-party integrations and remove the final `extensions/` root;
7. regenerate runtime manifests, release metadata, and agent documentation.

Where an accepted compatibility contract cannot move atomically, a temporary
shim must identify its removal criteria. No compatibility shim may restore an
external service to the default runtime or let IO load scripts or launch child
processes.

## Consequences

- Repository placement expresses ownership: core contracts in `crates/`, shared
  kernel implementation in `libs/`, process-owned adapters in `services/`, and
  simulation in `tools/simulator`.
- The kernel source and dependency graph become smaller, while downstream
  integrations remain possible through stable Rust contracts.
- SunSpec and Home Assistant are no longer capabilities of the standard
  AetherEdge build. Operators needing them use a downstream composition.
- CloudLink has one process owner instead of parallel IO and Uplink MQTT paths,
  and carries only its native session, manifest, telemetry, data-loss, spool,
  acknowledgement, and replay protocol rather than a speculative generic
  Integration extension.
- The Rust workspace has no Redis client dependency; an operator-selected
  Redis server alone does not become a kernel capability or live-state plane.
- Removing unpublished, uncomposed adapters may require recreating them in a
  downstream repository if a real deployment later needs them.
- AetherEdge still guarantees a six-process, zero-external-service default
  distribution.

## Verification

Architecture and behavior checks enforce:

- absence of an `extensions/` root and Cargo paths into it;
- scripts and communication simulation only under `tools/simulator`;
- no SunSpec source, model assets, feature, or default runtime advertisement;
- explicit rejection of unavailable SunSpec configurations;
- Uplink-only CloudLink runtime ownership;
- no API ownership of the History database;
- no non-composition dependency on SHM runtime implementations; and
- unchanged SHM authority, command governance, six-process distribution, and
  zero-external-service defaults.
