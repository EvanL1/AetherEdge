# Aether invariants

These rules are more important than the current directory layout.

1. SHM is authoritative for current point values.
2. A point has exactly one live writer for each ownership class.
3. Configuration discovery never depends on scanning live-state keys.
4. Device commands pass authorization, safety policy, idempotency handling, and
   audit before reaching a driver.
5. Read-only AI capabilities cannot mutate device, configuration, or storage
   state.
6. External-service failure cannot stop local acquisition or local safety
   rules.
7. Offline uplink data is bounded and durably queued before acknowledgement.
8. Redis and PostgreSQL are optional adapters, never startup prerequisites.
9. Domain packs cannot introduce Rust dependencies into the core.
10. AI disconnection cannot affect deterministic runtime behavior.
