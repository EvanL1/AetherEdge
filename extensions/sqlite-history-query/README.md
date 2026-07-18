# Aether SQLite History Query

Production `HistoryQuery` adapter for the embedded SQLite file owned by
`aether-history`. It reads the existing table:

```text
history(time_ms, series_key, point_id, value)
```

The adapter never initializes or migrates that schema. Construction is lazy,
so a temporarily absent history database does not block the composition root.
Each query opens or reuses SQLite in read-only mode plus `query_only=ON`; a
missing or inaccessible file returns typed `Unavailable` and a later query can
recover after the file appears. All SQL uses bound parameters.

Read-only SQLite flags are not a filesystem security boundary. A production
host must expose the historian database family (database plus WAL/SHM files)
to the API reader through a dedicated read-only directory mount or an
equivalent independently permissioned account/ACL. The current base Compose
mounts all of `/app/data` read-write into `aether-api`, so it does not yet meet
this defense-in-depth requirement and the production route remains blocked
until a deployment override separates historian read access from the API's own
writable configuration/audit database.

Each commissioned route fixes the exact task identity, binding identity,
feature definition (including unit), task cadence, core
`HistoryAggregation`, and physical series.
There is no implicit aggregation default: the route and `HistoryWindow` must
declare the same policy. Duplicate handling is likewise task-owned: `Latest`
keeps the greatest SQLite `rowid` at a timestamp before aggregation, while
`Reject` fails with typed invalid data. A logical
`(task, binding, feature)` route is unique. The same physical
`(series_key, point_id)` may be reused by different tasks so each task can apply
its own commissioned cadence and aggregation policy.

For a logical interval-end label `t`, raw numeric observations in
`(t - cadence, t]` are reduced with the commissioned `Mean`, `Last`, `Sum`,
`Min`, or `Max` policy and stamped at `t`. The EMS load route explicitly uses
`Mean`. The output is always the complete requested grid; a bucket without
numeric observations is represented by `FeatureValue::missing()` and
`SampleQuality::Missing`. Null rows do not participate in aggregation or
advance provenance. A stored feature's watermark is the maximum timestamp of
a numeric raw observation that actually participated in aggregation—not the
bucket label. Reads never advance beyond `HistoryWindow::cutoff()`.

Every stored-feature query requests `max_raw_samples_per_feature + 1` rows. If
the extra row exists, the operation fails closed instead of silently
truncating raw input. Calendar features, including UTC quarter-hour-of-day,
are generated deterministically from the same interval-end grid.
All feature reads in one logical query share one read transaction and therefore
one SQLite snapshot.

That snapshot is read-consistent at invocation time; it is not a historical
point-in-time snapshot for `as_of`. The current table has neither an
`ingested_at`/system-time column nor a source or configuration epoch. A row
backfilled after an old `as_of` can therefore appear in a later replay, and a
device remapped behind the same `(series_key, point_id)` can concatenate old
and new physical sources. Task and binding revisions fail closed against the
current route, but cannot filter epochs that were never stored with the rows.
Use a frozen database/export captured at the evaluation cut for an offline
golden or backtest. A rigorous point-in-time adapter needs bitemporal ingestion
metadata plus a stored source/binding epoch and queries both cuts.

The adapter path is also deployment configuration, not proof of the active
historian writer. `history_config.storage_*` records saved intent, while
`PUT /hisApi/storage` does not reconnect the running history backend. Disable
Data Processing across a storage change, reconnect or restart
`aether-history`, verify the active SQLite backend and a commissioned sentinel
series, then restart `aether-api` with the same path. The current authority
check cannot by itself distinguish saved intent from an unreconnected writer.

```rust
use aether_domain::{
    BindingIdentity, FeatureDefinition, FeatureRole, HistoryAggregation,
    HistoryDuplicatePolicy, TaskIdentity,
};
use aether_sqlite_history_query::{
    SqliteHistoryFeatureRoute, SqliteHistoryQuery, SqliteHistoryQueryConfig,
};

# async fn example() -> Result<(), Box<dyn std::error::Error>> {
let task = TaskIdentity::new("energy.site-load-forecast", 1)?;
let binding = BindingIdentity::new("site-a", 1)?;
let load = FeatureDefinition::numeric("load", FeatureRole::History, "kW")?;
let route = SqliteHistoryFeatureRoute::stored(
    task,
    binding,
    load,
    900_000,
    HistoryAggregation::Mean,
    HistoryDuplicatePolicy::Latest,
    "inst:1:M",
    "101",
    "site.load.active_power",
)?;
let config = SqliteHistoryQueryConfig::new(
    "/var/lib/aether/aether-history.db",
    vec![route],
    100_000,
)?;
let history = SqliteHistoryQuery::open(config).await?;
# let _ = history;
# Ok(())
# }
```

Run its real-SQLite contract tests with:

```bash
cargo test -p aether-sqlite-history-query
```
