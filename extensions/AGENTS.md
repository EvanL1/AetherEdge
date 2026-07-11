# Extension Instructions

Extensions implement one or more ports from `aether-ports`.

- Each external dependency belongs only to the extension that uses it.
- Redis and PostgreSQL extensions are never default features of the SDK or
  daemon.
- An extension must pass the matching `aether-testkit` conformance suite.
- Failure behavior must be explicit: timeouts and connection loss are
  transient; invalid configuration is permanent.
- Extension names describe capabilities (`postgres-history`, `redis-bridge`),
  not a promise that one database owns all system state.
- Protocol and cloud adapters must tolerate disconnects and bounded retries.
