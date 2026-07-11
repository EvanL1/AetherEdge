# ADR-0005: Make SHM the only core live-state plane

## Status

Accepted and implemented on 2026-07-10.

## Context

The acquisition and automation processes already preferred shared memory, but
their default dependency graphs, startup paths, rule fallback reads, instance
CRUD, health checks, and compatibility mirrors still required a Redis-shaped
RTDB. A host could therefore have two disagreeing live-state views, and a Redis
outage could prevent otherwise local edge behavior.

## Decision

1. `aether-io` is the sole writer of acquired Telemetry/Signal channel slots.
2. `aether-automation` reads live values from SHM and writes routed
   Control/Adjustment commands through the generation-checked SHM + UDS action
   dispatcher. UDS is a notification plane; SHM remains the data authority.
3. Missing SHM data is an explicit unavailable state. Core services never fall
   back to Redis, an in-process RTDB, or a network database.
4. SQLite remains the local authority for configuration, instances, routing,
   rules, execution history, audit data, and other non-live metadata.
5. Redis may be installed only as an optional external `StateMirror` that
   consumes SHM. It cannot participate in startup, routing authority, command
   acceptance, or correctness of the core runtime.
6. Virtual channel points receive normal SHM slots; they no longer rely on a
   secondary live-value store.
7. An Action without an M2C route fails closed. It is not stored as a successful
   local-only command.
8. Rule writes to derived Measurement points are rejected until a separate,
   automation-owned derived-state SHM plane is designed. Automation must never
   write the IO-owned Telemetry/Signal slots.
9. Automation treats the initial SHM reader as a startup requirement. If IO is
   not ready after the bounded retry window, startup fails and the process
   supervisor retries it instead of running indefinitely against an empty
   fallback state plane.

## Consequences

- The default `aether-io` and `aether-automation` Cargo graphs contain neither
  Redis nor the legacy `aether-rtdb` abstraction.
- Redis configuration, cleanup, synchronization, name indexes, rule mirrors,
  and Redis-only HTTP endpoints are removed from the two services.
- Stateful calculation functions use process-local state for now; a future
  restart-safe implementation must use a local embedded checkpoint port.
- Deployments that still need Redis enable the explicit `redis` Compose profile
  and run an optional mirror adapter.
- This is intentionally breaking. No Redis fallback or dual-write compatibility
  shim is retained.
