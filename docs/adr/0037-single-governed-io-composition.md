# ADR-0037: Keep one governed IO composition

## Status

Accepted and implemented on 2026-08-19.

## Context

The IO service still retained several compatibility implementations after its
protocol composition and configuration generation became authoritative:

- a public router constructor assembled read routes with an unavailable channel
  application boundary; production used a separate fully governed constructor;
- `/api/channels/{id}/control` rewrapped enabled-state mutation and runtime
  reconciliation already exposed by canonical endpoints;
- `ModbusChannelConfig` stored reconnect settings that the Modbus adapter never
  read because `ChannelEntry` owns reconnect, backoff, cooldown, and recovery;
- `ShardedSlotStore` implemented a hypothetical multi-writer cache with no
  protocol consumer.

The unavailable router was used only by tests, the control alias had no CLI or
MCP consumer, and the two internal abstractions could not affect production
behavior.

## Decision

1. IO exposes only the router constructor that requires both
   `ChannelManagementApplication`, `ChannelReconciliationApplication`, and the
   access-token authenticator.
2. `ChannelManagementHttpBoundary` always contains those governed applications;
   it has no unavailable or partially composed state.
3. Remove `/api/channels/{id}/control`. Desired lifecycle uses
   `PUT /api/channels/{id}/enabled`; runtime repair uses the one-channel or
   all-channel reconciliation endpoint.
4. Remove the unused Modbus reconnect configuration. The channel runtime's
   shared reconnect policy remains the only reconnect authority.
5. Remove `ShardedSlotStore` and convenience methods with no production caller.
   GPIO keeps `AtomicBoolStore`; CAN keeps the single-writer `SlotStore`.
6. Keep source and OpenAPI tests that reject restoration of these compatibility
   surfaces.

## Consequences

- Every IO router has authenticated, confirmed, audited channel applications.
- One operation has one HTTP command and one typed response contract.
- Modbus cannot advertise an adapter-local reconnect policy that is ignored.
- No speculative multi-writer cache remains in the protocol core.
- Physical protocol adapters, automatic reconciliation, SHM authority,
  per-channel protocol diagnostics, and test-only Modbus simulation are
  unchanged.
