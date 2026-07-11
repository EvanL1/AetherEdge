# ADR-0001: Adopt an AI-native edge-kernel architecture

## Status

Accepted for incremental migration on 2026-07-10. Runtime-composition clauses
were amended by [ADR-0003](0003-multi-process-shm-event-plane.md) on the same
date; ADR-0003 governs where the two records differ.

## Context

AetherEMS grew as a multi-service energy-management product. Its live data is
now authoritative in shared memory, while Redis remains a delayed mirror used
by several peripheral services. PostgreSQL/TimescaleDB is useful for some
history deployments but is not required by the edge kernel. The current
workspace nevertheless compiles database clients through default features and
the generated workspace-hack dependency.

The project is intended to become an AI-native, industry-neutral IoT library
and edge runtime. It must remain useful without a browser UI and without
external infrastructure.

## Decision

1. Introduce four stable layers: domain, ports, application, and runtime.
2. Keep infrastructure and protocols in optional extension crates.
3. Replace the Redis-shaped `Rtdb` abstraction with small capability ports.
4. Use SHM as the live-state authority and local embedded persistence for the
   standalone distribution.
5. Keep the six production service processes independently supervised. An
   optional single-process composition may exist for development or simulation,
   but it is not the default deployment and does not replace the service form.
6. Treat Redis as an optional state mirror/event bridge and PostgreSQL as an
   optional history/audit sink.
7. Move energy-specific models, mappings, rules, and knowledge into an energy
   domain pack.
8. Expose one typed command/query application API to CLI, MCP, and optional
   HTTP transports.
9. Make capability metadata and safety policy machine-readable and validate AI
   behavior with repository-owned evaluations.

## Consequences

### Positive

- The SDK and four peripheral services do not require external services; the
  remaining aether-io/aether-automation compatibility paths are migrated incrementally.
- Third-party adapters depend on narrow stable contracts.
- AI agents can discover capabilities, risks, invariants, and verification
  commands without reverse-engineering service internals.
- Energy remains a supported vertical without constraining other industries.

### Negative

- Compatibility shims will temporarily coexist with the new ports.
- Cross-process SHM generations, event hints, and supervision contracts require
  explicit compatibility and restart testing.
- More production processes remain to package and supervise than in a monolith.

## Migration constraints

- Existing services remain buildable until their behavior has equivalent
  application-level tests.
- No bulk file move may combine semantic changes with path changes.
- A new adapter is used by at least one composition test before the legacy path
  is removed.
- Redis and PostgreSQL dependencies must disappear from the default dependency
  graph before this ADR is considered fully implemented.
