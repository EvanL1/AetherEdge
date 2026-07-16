# aether-store-local

Local adapters for a gateway that must run without external services.

| Adapter | Persistence | Intended use |
|---|---|---|
| `MemoryLiveState` | process-local | SDK embedding, tests, small compositions |
| `MemoryHistorySink` | process-local | tests and host-managed persistence |
| `MemoryHistoryQuery` | process-local | bounded logical history fixtures for Data Processing |
| `MemoryCovariateSource` | process-local | known-future covariate fixtures for Data Processing |
| `SnapshotCovariateSource` | atomically replaceable JSON | production known-future covariates without an external service |
| `MemoryAuditSink` | process-local | tests and host-managed persistence |
| `SqliteAuditSink` (`sqlite-audit`) | embedded SQLite | mandatory command audit without an external service |
| `MemoryOutbox` | process-local | conformance tests and ephemeral workloads |
| `FileOutbox` | crash-recoverable file | production offline store-and-forward |
| `MemoryCloudLinkSpool` | process-local | deterministic application-ACK/replay conformance |
| `FileCloudLinkSpool` | crash-recoverable file | experimental CloudLink positions, replay, and loss evidence |

`MemoryHistoryQuery` and `MemoryCovariateSource` are keyed by the complete
versioned `BindingIdentity`. They project only requested logical features in
request order, apply half-open time windows and hard sample limits, and retain
one exact provenance entry per returned feature. Unknown bindings are
permanent commissioning errors; an empty selected window remains an
availability outcome.

These read adapters are deliberately separate from `MemoryHistorySink`.
Querying or replacing a deterministic fixture does not mutate the append-only
history sink and never changes SHM live-state authority.

## SnapshotCovariateSource

`SnapshotCovariateSource` is the production zero-service adapter for forecast
covariates such as weather predictions. Construction retains only the path and
hard limits, so a missing optional snapshot does not prevent the host from
starting. Every forecast resolution reads the currently published file on a
blocking worker, applies the byte bound before parsing, and fully validates it.

```rust
use aether_store_local::{SnapshotCovariateLimits, SnapshotCovariateSource};

# fn example() -> Result<(), Box<dyn std::error::Error>> {
let limits = SnapshotCovariateLimits::new(
    4 * 1024 * 1024, // file bytes
    256,             // bindings
    32,              // runs per binding
    64,              // features per run and response
    4_096,           // samples per run and response
)?;
let source = SnapshotCovariateSource::open("./data/covariates.json", limits)?;
# let _ = source;
# Ok(())
# }
```

The JSON shape is strict: unknown fields and unknown enum values are rejected.
Timestamps are UTC Unix milliseconds. A run has one issue time, one source
watermark, one exact valid-time grid, and a redaction-safe logical source
reference for every nondeterministic feature.

```json
{
  "schema": "aether.covariate-snapshot.v1",
  "bindings": [
    {
      "id": "example-site",
      "revision": 1,
      "runs": [
        {
          "issued_at_ms": 1783741200000,
          "watermark_ms": 1783741800000,
          "valid_times_ms": [1783743300000, 1783744200000],
          "features": [
            {
              "name": "temp_avg",
              "value_type": "number",
              "unit": "Cel",
              "source_ref": "weather.nwp.air_temperature",
              "values": [32.1, 32.0],
              "quality": ["good", "good"]
            }
          ]
        }
      ]
    }
  ]
}
```

`value_type` is `number`, `string`, or `boolean`; non-numeric features omit
`unit`. Values may be `null` only when the matching quality is `missing`.
Quality is `good`, `uncertain`, `substituted`, or `missing`.

For a requested `as_of`, the adapter selects the newest run whose
`issued_at_ms <= as_of`. It never silently falls back to an older run when the
newest eligible run has the wrong grid, type, or unit. Its selected watermark
must also be at or before `as_of`. The requested half-open window and sample
count define an exact regular grid; missing, extra, or off-grid valid times are
an `InvalidData` outcome rather than a truncated response.

For a v1 interval-end forecast with cadence `c`, that future grid begins at
`as_of+c`. The current energy load/PV tasks require `issued_at` for every
non-calendar future covariate.

`quarter_hour` is reserved and must not be stored in the file. When requested
as a numeric future covariate with unit `1`, it is generated deterministically
from UTC valid time, with `calendar.utc.quarter_hour` provenance and the
request `as_of` as its watermark.

Publish updates by writing and syncing a sibling temporary file, then renaming
it over the configured path on the same filesystem. The next resolution sees
the new run set. A missing file returns `Unavailable`; an invalid update
returns `InvalidData` (or `Rejected` for a hard bound) and never reuses a stale
in-memory snapshot. Avoid in-place writes, which can expose a partial file to a
concurrent reader.

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
committed record fails closed instead of discarding the later data. The journal
permits one process writer, is bounded by entry count, and can be reclaimed
with `FileOutbox::compact()`.

Long-running hosts should invoke compaction periodically; the compatibility
`uplink` does so at startup and hourly. Capacity bounds live entries, while
compaction bounds obsolete acknowledged records in the journal.

Disk durability does not define network delivery. The selected
`UplinkPublisher` decides when an entry may be acknowledged.

## CloudLink spools

`MemoryCloudLinkSpool` and `FileCloudLinkSpool` implement the dedicated
`CloudLinkSpool` port. They preserve stream epoch, monotonic position, stable
batch identity/digest, offer/PUBACK state, last durable application ACK, and
capacity-overflow data-loss evidence. A transport publish never removes a
record. Stale-session, wrong-stream, wrong-batch, and wrong-digest ACKs fail
closed; an exact duplicate ACK is idempotent.

The file adapter owns an exclusive process lock and synchronizes every state
transition in an incremental journal. Recovery truncates only an incomplete
tail; a checksum or semantic failure is corruption and fails closed even in the
last complete record. `FileCloudLinkSpool::compact()` atomically rewrites cursor
metadata plus live records, and the adapter compacts before accepting more work
after 256 mutations. Its file format is independent of legacy `FileOutbox` and
cannot be opened through the generic outbox port.
