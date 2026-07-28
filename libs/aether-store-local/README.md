# aether-store-local

Local adapters for a gateway that must run without external services.

| Adapter | Persistence | Intended use |
|---|---|---|
| `MemoryLiveState` | process-local | SDK embedding and tests |
| `MemoryHistorySink` | process-local | tests and host-managed persistence |
| `MemoryAuditSink` | process-local | tests and host-managed persistence |
| `SqliteAuditSink` (`sqlite-audit`) | embedded SQLite | mandatory command audit |
| routing loaders (`sqlite-routing`) | embedded SQLite | commissioned definitions to storage-agnostic routing snapshots |
| `MemoryOutbox` | process-local | conformance tests and ephemeral workloads |
| `FileOutbox` | crash-recoverable file | production offline store-and-forward |
| `MemoryCloudLinkSpool` | process-local | application-ACK/replay conformance |
| `FileCloudLinkSpool` | crash-recoverable file | CloudLink positions, replay, and loss evidence |
| `FileCloudLinkChallengeLedger` | crash-recoverable file | signed-session challenge replay protection |

## FileOutbox

`FileOutbox` is the zero-service durable queue for legacy uplink delivery. It
uses exclusive process locking, bounded records, atomic replacement, FIFO
visibility, and explicit acknowledgement. Corrupt or oversized state fails
closed instead of being silently discarded.

## CloudLink storage

CloudLink storage is intentionally distinct from the generic outbox. Its spool
owns stream epoch and position, stable batch identity and digest, replay
windows, explicit data-loss evidence, and removal only after a matching cloud
application acknowledgement. The challenge ledger reserves signed challenge
identities durably before a session becomes usable.

These adapters do not own transport sessions, MQTT topics, routing, SHM, or
application authorization. Composition roots select them behind the relevant
ports.

```bash
cargo test -p aether-store-local
cargo test -p aether-store-local --features sqlite-routing \
  --test sqlite_routing_contract --test sqlite_physical_topology_contract
```

Licensed under either MIT or Apache-2.0, at your option.
