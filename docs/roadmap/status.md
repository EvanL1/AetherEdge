# Platform status and roadmap

Status is reported separately for implemented, experimental, and planned
capabilities. Product names do not upgrade technical readiness.

## AetherEdge

**Implemented:** six-service runtime, SHM live-state authority, embedded local
operation, governed commands, `aether` CLI, `aether-edge-sdk`, Pack v1, MCP and
OpenAPI foundations, and signed `v0.5.0` source/runtime/CLI artifacts.

**Experimental:** CloudLink MQTT v1 edge foundation, application-ACK-driven
spool, AetherContracts alpha.3 consumption, and real-Broker development
evidence.

**Planned or gated:** production CloudLink key lifecycle, signed ACK, complete
joint conformance, legacy cutover, and remaining application-boundary migration.

## AetherCloud

**Implemented foundations:** modular-monolith domain/application slices,
capability-driven providers, Plan-only OpenTofu, Gateway enrollment, partial
CloudLink/telemetry persistence, artifact/deployment/job foundations, audit and
integration slices, observability, and a transport-neutral MCP interface.

**Experimental or partial:** MQTT codec and ingress, local/AWS IoT harnesses,
PostgreSQL accepted-telemetry ACK outbox, and finite audit interfaces.

**Planned or gated:** production identity, complete CloudLink durability and
mapping, production composition and workers, public job/deployment delivery,
hardened outbound integrations, and a connectable MCP server.

## AetherContracts

**Implemented, experimental:** alpha.3 specifications, closed Schemas, fixtures,
TCK, digest-pinned consumer verification, and four fixture bindings.

**Planned or gated:** production authentication key lifecycle, signed durable
ACK, complete production codecs, and a production CloudLink cutover release.

## Platform documentation

**Implemented in this migration:** shared product overview, unified navigation,
deployment topologies, user journeys, end-to-end alpha tutorial, compatibility
matrix, status page, and AetherIot to AetherEdge migration guide.

**Planned:** custom `aetheriot.dev` and `docs.aetheriot.dev` domains, automated
cross-repository version aggregation, release-channel status feeds, and a
future GitHub organization when an appropriate address is available.
