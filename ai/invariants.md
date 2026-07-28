# Aether invariants

These rules are more important than the current directory layout.

1. SHM is authoritative for current point values.
2. A point has exactly one live writer for each ownership class.
3. Configuration discovery never depends on scanning live-state keys.
4. Device commands pass authorization, safety policy, declared retry policy,
   and audit before reaching a driver. A correlation ID is not an idempotency
   key; current device-control capabilities are non-idempotent.
5. Read-only AI capabilities cannot mutate device, configuration, or storage
   state.
6. External-service failure cannot stop local acquisition or local safety
   rules.
7. Offline uplink data is bounded and durably queued before acknowledgement.
8. Redis and PostgreSQL are optional adapters, never startup prerequisites.
9. Domain packs cannot introduce Rust dependencies into the core.
10. AI disconnection cannot affect deterministic runtime behavior.
11. Physical acquisition samples carry a channel identity and enter only
    through the IO-owned `AcquisitionStateWriter`; logical application points
    carry an instance identity. HTTP, CLI, MCP, automation, alarm, history,
    and uplink never receive the acquisition writer.
12. Pack activation requires a target-compatible, checksummed runtime manifest
    whose capabilities and protocols match the concrete composition. A process
    never fills a missing manifest by assuming a full feature set.
13. Local SQLite is authoritative for commissioned channel desired state; the
    active protocol runtime is a rebuildable projection. HTTP, CLI, and MCP
    create, update, delete, enable, or disable channels only through the
    confirmed, audited `io.channel.manage` application command and never
    coordinate SQLite and `ChannelManager` directly.
14. MQTT client acceptance and MQTT PUBACK are transport evidence, never a
    CloudLink durable business acknowledgement. A CloudLink record is removable
    only after a matching application ACK validates session, stream epoch,
    position, batch identity, and canonical digest.
15. CloudLink replay preserves stream position, batch identity, and business
    digest. Equal identity with different content is a fail-closed conflict;
    unavailable retained ranges produce explicit data-loss evidence.
16. CloudLink is broker neutral. A customer-selected MQTT broker is supported,
    AetherCloud does not have to own the broker, and broker/cloud failure cannot
    affect acquisition, rules, alarms, safety, history, or local control.
17. CloudLink v1 has no physical-control, arbitrary-RPC, direct SHM-write, or
    point/register-write capability. Legacy MQTT control topics are never
    automatically translated into CloudLink.
18. Edge telemetry never fabricates a Thing Model revision. It preserves the
    real `PointAddress`, source timestamp, exposed quality, and coherent topology
    generation; business point facts remain distinct from operational telemetry
    and OpenTelemetry signals.
19. Shared contract authority is the digest-pinned AetherContracts release.
    AetherEdge and AetherCloud keep the same closed consumer lock; local wire,
    authentication, fixture-manifest, and gate files cannot redefine the public
    core.
20. Complete distribution integrity and public fixture execution are not
    production state-machine, authentication, signed-ACK, real-Broker, or
    crash-durability conformance.
21. Contract consumption never follows `main`, `latest`, a floating tag, or a
    version range and never falls back to a sibling checkout. Legacy remains
    default, and contract adoption adds no physical-control operation.
