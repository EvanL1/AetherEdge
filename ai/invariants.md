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
11. Data processors receive complete, bounded `ProcessingFrame` values from
    Aether; they never discover inputs by reading SHM, history storage, or
    configuration databases directly.
12. `DerivedData` is not authoritative live point state and is never written
    into the IO-owned T/S plane.
13. A data-processing result cannot dispatch a device command. Planning,
    authorization, confirmation, audit, and control remain separate application
    use cases.
14. Missing processors or processor failure cannot stop acquisition, history,
    alarms, deterministic rules, or local safety behavior.
15. `data_processing.process` is non-idempotent: an input digest identifies a
    complete assembled content snapshot but does not promise request replay,
    de-duplication, or a cached result.
16. A live SHM sample may replace only a `Last`-aggregated final history cell;
    it must never stand in for a `Mean`, `Sum`, `Min`, or `Max` interval bucket.
17. A forecast target must never also appear in `future_covariates`; future
    target values are unknown by definition.
18. `as_of` is an event-time frame bound, not proof of a historical knowledge
    cut. An agent must not describe a backtest as leakage-safe unless history
    is frozen/bitemporal with source epochs and the artifact set is frozen or
    carries validated training/availability cuts.
19. Persisted `history_config.storage_*` is saved intent, not proof of the
    active historian writer after a storage update. Data Processing remains
    disabled until reconnect/restart and sentinel verification complete.
20. SQLite read-only flags do not replace OS permissions. Production direct
    history reads give the API an independent read-only historian directory or
    identity, separate from its writable configuration/audit store.
21. Physical acquisition samples carry a channel identity and enter only
    through the IO-owned `AcquisitionStateWriter`; logical application points
    carry an instance identity. HTTP, CLI, MCP, automation, alarm, history,
    and uplink never receive the acquisition writer.
22. Pack activation requires a target-compatible, checksummed runtime manifest
    whose capabilities and protocols match the concrete composition. A process
    never fills a missing manifest by assuming a full feature set.
23. Local SQLite is authoritative for commissioned channel desired state; the
    active protocol runtime is a rebuildable projection. HTTP, CLI, and MCP
    create, update, delete, enable, or disable channels only through the
    confirmed, audited `io.channel.manage` application command and never
    coordinate SQLite and `ChannelManager` directly.
