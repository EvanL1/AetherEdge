# aether-store-local

Local adapters for a gateway that must run without external services.

| Adapter | Persistence | Intended use |
|---|---|---|
| `MemoryLiveState` | process-local | SDK embedding, tests, small compositions |
| `MemoryHistorySink` | process-local | tests and host-managed persistence |
| `MemoryAuditSink` | process-local | tests and host-managed persistence |
| `SqliteAuditSink` (`sqlite-audit`) | embedded SQLite | mandatory command audit without an external service |
| `MemoryOutbox` | process-local | conformance tests and ephemeral workloads |
| `FileOutbox` | crash-recoverable file | production offline store-and-forward |

## FileOutbox

```rust
use std::sync::Arc;

use aether_ports::{DurableOutbox, OutboxMessage};
use aether_domain::TimestampMs;
use aether_store_local::FileOutbox;

# async fn example() -> Result<(), Box<dyn std::error::Error>> {
let outbox: Arc<dyn DurableOutbox> =
    Arc::new(FileOutbox::open("./data/uplink.outbox", 10_000)?);
outbox
    .enqueue(OutboxMessage::new(
        "telemetry/site-a",
        br#"{"temperature": 21.5}"#.to_vec(),
        TimestampMs::new(1_700_000_000_000),
    ))
    .await?;
# Ok(())
# }
```

Each successful mutation has been synchronized to the journal. Recovery
replays complete checksum-valid records and treats an incomplete or
checksum-invalid final record as a crash-torn tail. Corruption before a later
committed record fails open instead of discarding the later data. The journal
permits one process writer, is bounded by entry count, and can be reclaimed
with `FileOutbox::compact()`.

Long-running hosts should invoke compaction periodically; the compatibility
`uplink` does so at startup and hourly. Capacity bounds live entries, while
compaction bounds obsolete acknowledged records in the journal.

Disk durability does not define network delivery. The selected
`UplinkPublisher` decides when an entry may be acknowledged.
