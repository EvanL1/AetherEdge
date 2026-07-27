---
title: Data Processing Flow
description: How Aether assembles governed IoT data, invokes a request-driven processor, validates derived data, and keeps control separate
updated: 2026-07-11
---

# Data Processing Flow

Aether uses a request-driven data-processing path. The
`DataProcessingApplication` assembles a complete, time-bounded
`ProcessingFrame` and sends it to a selected `DataProcessor`. A processor never
receives a site identifier and then calls back to SHM, history, or
configuration to discover its inputs. **Data requests the processor; the
processor does not request Aether data.**

This path starts after device data has been decoded and published. It remains
outside the SHM write path, historical persistence, rule and alarm hot paths,
and command delivery. Version 1 serves an authenticated HTTP query; a future
scheduled planning cycle must use the same boundary. Its failure cannot stop
device polling, live-state publication, deterministic protection, alarms, or
existing control behavior.

## End-to-end path

```
Caller: authenticated HTTP / in-process application work
                            │
                            ▼
               DataProcessingApplication
                 │ resolve task, binding,
                 │ processor policy, as_of
                 │
        ┌────────┼───────────────┐
        │        │               │
        ▼        ▼               ▼
  HistoryQuery LiveState   CovariateSource
  historical  current SHM  weather / calendar /
  observations Last-only   schedule / tariff
               live tail
        │        │               │
        └────────┼───────────────┘
                 ▼
         ProcessingFrame assembler
          align / aggregate / validate /
          quality-policy / digest
                 │
                 ▼ complete, bounded request
            DataProcessor
        local algorithm / model sidecar /
               remote service
                 │
                 ▼
      ProcessingResult validation
                 │
                 ▼
       authenticated HTTP response
                 │ future separate use case
                 ▼
              planner
                 │ constraints and policy
                 ▼
       ControlApplication → SHM + UDS → IO
```

The upper path is a query and derived-data path. The lower control path begins
only after a separate application decision. `DataProcessor` has no reference
to `LiveStateWriter`, an action dispatcher, alarm lifecycle, a planner, or a
protocol adapter.

## 1. Start with an explicit task request

The caller selects an enabled `DataProcessingTask`, not arbitrary site points,
a processor URL, or a model file. A request identifies at least:

- the task and task-contract version;
- the commissioned binding in which the task runs;
- caller/request context and a bounded deadline;
- a stable `as_of` timestamp;
- task-specific parameters such as forecast horizon;
- task-specific typed options. Processor route and artifact policy are
  resolved from commissioned configuration, never selected by the caller.

`as_of` freezes the frame's event-time cut. Historical and live observations
newer than the cut are excluded. External forecasts or schedules record the
version that was available at that cut. This prevents those sources from
crossing their declared time boundary; it is not an API replay guarantee or,
with the current historian and artifact metadata, proof of a leakage-safe
offline backtest.

The application resolves one configuration revision for the complete request:
task schema, site binding, unit and sign rules, time policy, quality limits,
processor, and egress policy cannot change halfway through frame assembly.
Configuration follows the normal Aether path after `aether sync`; a runtime
service does not parse an industry pack's YAML directly.

## 2. Query the historical window

`HistoryQuery` supplies the task's bounded lookback window for its bound
observation fields. It is a domain capability, not a database abstraction.
`DataProcessingApplication` and `DataProcessor` see neither SQL nor
storage-specific series keys.

The default production adapter opens the existing `aether-history.db` lazily in
read-only/query-only mode. It does not create or migrate the historian schema.
Every feature read in one logical request shares one SQLite transaction and
therefore one snapshot. For cadence `c`, history labels end at `as_of`; a label
`t` reduces raw observations in `(t-c, t]` using that feature's commissioned
aggregation and duplicate policy. Empty buckets stay missing, and the source
watermark is the newest numeric raw row that participated rather than `t`.

`aether-history` alone owns schema migration, writes, retention, and the file
lifecycle. The query adapter depends on SQLite snapshot/WAL semantics and
read permissions. If the file is temporarily absent or inaccessible, only the
processing request becomes typed-unavailable; its lazy reader may recover on a
later call. An external historian is not silently substituted.

Production permissions must make that ownership physical: the API receives a
dedicated read-only historian database/WAL/SHM directory or read-only identity,
separate from its writable configuration/audit database. SQLite read-only
flags on the base shared `/app/data:rw` mount do not provide that boundary.

The read transaction is a snapshot of the database when the request runs, not
of what the database contained at historical `as_of`. Rows lack ingestion
time and source/configuration epoch. Consequently, a late correction with an
old event timestamp can appear in a later replay, and a physical remap hidden
behind the same logical `(series_key, point_id)` can join old and new source
epochs. Expected task/binding revisions guard current configuration only; they
cannot filter metadata absent from history rows. Use a frozen database/export
for offline evaluation, or add a bitemporal, epoch-bearing `HistoryQuery`
before claiming point-in-time reproducibility.

The runtime's SQLite authority guard compares the route with persisted
`history_config.storage_*`. Those settings are saved intent: a
`PUT /hisApi/storage` does not reconnect the active writer. Treat any storage
change as a maintenance boundary—disable processing, reconnect or restart the
historian, validate an expected sentinel series, and restart `aether-api`—so
the reader is not joined to a configured path different from the active write
path.

The optional HTTP history adapter is only for a loopback upstream that already
materializes the exact cadence grid. It supports `last/reject`, not raw
aggregation. Processors never receive either database or history API access.

## 3. Apply the current live tail

Persisted history can lag the current device value by its sampling or flush
interval. `DataProcessingApplication` reads only the task's required points
through the read-only `LiveState` port and may replace the corresponding final
interval cell for explicitly mapped features. A partial live tail changes only
those mapped cells; unrelated historical features retain their stored values
and provenance.

This is valid only when that feature's commissioned history aggregation is
`Last`. An instantaneous SHM value cannot represent a `Mean`, `Sum`, `Min`, or
`Max` bucket, so v1 rejects `live_tail: true` for those policies. The energy
load and PV tasks both use `Mean` for their targets and therefore forbid live
tail.

SHM remains authoritative for current T/S state. The live read is not a second
historian and contributes only the available current samples with their source
timestamps. The frame assembler:

1. rejects unwritten, non-finite, future, or over-age live values;
2. maps an accepted sample only to that feature's final interval-end cell;
3. follows the task's explicit source-authority rule for overlaps instead of
   arbitrary last-writer-wins behavior;
4. records whether the final interval came from history, live state, or both;
5. preserves missingness instead of silently substituting zero.

The processor does not map SHM, understand slots or writer generations, or
receive `LiveStateWriter`. This keeps the shared-memory ABI inside Aether and
preserves IO's exclusive ownership of T/S writes.

The current SHM bridge labels accepted finite live values as `good`; it does
not preserve device-origin sample quality. Likewise, the current SQLite history
schema stores numeric observations without source quality. Version 1 enforces
freshness, gaps, missingness, numeric constraints, provenance, and issue time,
but a deployment that requires end-to-end device quality must provide a
quality-bearing source adapter.

## 4. Resolve covariates and context

Some tasks need data that is not a device observation. A forecast may use
future weather, calendar fields, tariffs, known setpoints, or a production
schedule. A configured `CovariateSource` returns only fields declared by the
task.

Every covariate carries event time and source provenance. Forecasted
covariates also carry an issue time or version so assembly selects the
forecast that was available at `as_of`, not a later corrected forecast. This
closes the covariate-vintage boundary only; the history and model-artifact
cuts have the separate limitations above.
Deterministic calendar fields may be generated locally using the task's
declared timezone policy.

Required and optional fields are explicit. If a required covariate is absent,
the frame is unavailable unless the task declares a specific fallback. A
stale value is not silently forward-filled merely because a processor accepts
a number.

## 5. Assemble the ProcessingFrame

The application assembles source-specific samples into one processor-neutral
`ProcessingFrame`. A conceptual forecast frame looks like this:

```json
{
  "schema": "aether.processing-frame.v1",
  "as_of": "2026-07-11T12:00:00Z",
  "cadence_seconds": 900,
  "history": {
    "timestamps": ["2026-07-11T11:45:00Z", "2026-07-11T12:00:00Z"],
    "features": {
      "load": {
        "value_type": "number",
        "unit": "kW",
        "values": [820.0, 835.0],
        "quality": ["good", "good"]
      }
    }
  },
  "future_covariates": {
    "timestamps": ["2026-07-11T12:15:00Z", "2026-07-11T12:30:00Z"],
    "features": {
      "temperature": {
        "value_type": "number",
        "unit": "Cel",
        "values": [32.1, 32.0],
        "quality": ["good", "good"]
      }
    }
  },
  "static_features": {},
  "quality": {
    "input_watermark": "2026-07-11T11:59:58Z",
    "missing_ratio": 0.0,
    "max_gap_seconds": 900,
    "live_tail_included": false,
    "substituted_samples": 0
  },
  "provenance": [
    {
      "segment": "history",
      "feature": "load",
      "source_kind": "history",
      "source_ref": "energy.site.load.active_power",
      "watermark": "2026-07-11T11:59:58Z"
    }
  ]
}
```

The wire codec may be JSON, CBOR, or another versioned representation; the
semantic contract stays the same. Named, typed fields keep Aether independent
of private algorithm or tensor names and make requests inspectable and usable
in offline conformance fixtures.

The Aether-side application owns work tied to site-data semantics:

- resolving instance and point bindings;
- ordering timestamps in UTC and applying declared local-time policy;
- validating that commissioned unit, scale, offset, point kind, and target sign
  already match the task (v1 performs no runtime unit/sign conversion);
- applying task-declared aggregation and resampling rules;
- aligning fields to a common time grid and preserving missingness masks;
- checking lookback, freshness, skew, gap, and completeness requirements;
- deriving processor-independent fields declared by the task;
- recording source watermarks and provenance.

The processor owns work tied to its implementation or artifact:

- selecting the configured algorithm or allowed model artifact;
- ordering named fields for that implementation;
- algorithm-specific transforms, scaling, and tensor construction;
- executing a deterministic library, local model, or remote endpoint;
- inverse-transforming outputs and reporting implementation provenance.

This split keeps device and point semantics out of processors and private
algorithm representation out of the Aether kernel.

After validation, the application computes a canonical input digest over the
versioned task identity, versioned binding identity, processor contract,
optional artifact selector, normalized frame (including `as_of`), and typed
options. Processor endpoint, request ID, submission time, and deadline are not
digest inputs. Independent invocations of the exact same normalized governed
content therefore have the same digest; repeating only `as_of` does not ensure
that content when sources are mutable. Version 1 does not use the digest for
replay or de-duplication.

An artifact selector's version and digest identify the bytes used. Version 1
does not carry artifact `trained_through` or `available_at`, so selecting a
current pinned model for an old `as_of` may still introduce model-vintage
leakage. Historical model evaluation must use an externally frozen artifact
registry/cut until that metadata is part of commissioning and validation.

## 6. Invoke the DataProcessor

`DataProcessingApplication` sends the complete frame, input digest, request
ID, deadline, typed output contract, and processor selection policy through
the `DataProcessor` port. An adapter may call:

- an in-process deterministic algorithm;
- a model endpoint in a sidecar on the same edge host;
- a separately supervised local processing service;
- an explicitly configured remote processing API.

A model endpoint is only one request-driven processor. It receives the named
observations and covariates in the request and may own model artifacts,
feature ordering, scaling, tensor construction, and execution. It does not
receive a `plant_id` and then query InfluxDB, Aether history, SHM, or site
configuration.

All processor locations implement the same port and receive the same governed
frame. Location does not change data authority or grant access to arbitrary
Aether state.

Calls are bounded by payload, concurrency, and deadline limits.
`data_processing.process` is non-idempotent: v1 has no replay store or
de-duplication contract, and another call may execute processor work again. A
caller retries only a typed retryable failure under its own bounded policy,
with a fresh deadline and awareness that earlier work may already have run.

For a remote processor, egress policy is part of adapter selection. Only
task-declared frame fields and required correlation metadata leave the host;
credentials, unrelated points, internal storage identifiers, and control
capabilities do not.

## 7. Validate ProcessingResult and DerivedData

Transport success does not make processor output trusted.
`DataProcessingApplication` validates the common `ProcessingResult` envelope
and its task-typed processor output. Only then does Aether stamp `DerivedData`
for a consumer.

Common checks include:

- supported result-contract version;
- matching request ID, task ID, binding revision, and input digest;
- selected processor and artifact identity, version, and digest;
- finite numeric outputs and expected engineering units;
- expected output shape, timestamp count, and strictly ordered time axis;
- task-specific ranges and consistency constraints;
- issue time, processor provenance, quality, and bounded expiry;
- explicit `produced`, `fallback`, or `unavailable` status.

For a forecast, validation also requires timestamps inside the requested future
horizon and correctly ordered confidence bounds or quantiles when present.

A processor may return fallback data only when the task allows that named
fallback and the result identifies it. An approved persistence forecast can be
usable with degraded quality; a zero-filled array produced after an exception
cannot be reported as a successful forecast. Invalid output rejects the entire
result rather than leaking partially trusted values to consumers.

## 8. Return derived data

Version 1 returns the validated `DerivedData` directly from the authenticated
`POST /api/v1/data-processing/process` route. It does not implement a result
cache, replay store, durable derived-data sink, CLI binding, or MCP tool. Those
are possible separate capabilities only after their authority, retention, and
side-effect policies are defined.

`DerivedData` does not enter IO-owned T/S slots or masquerade as a measured
point. Its provenance, quality summary, and expiry remain visible to the HTTP
consumer.

## 9. Hand off without crossing subsystem boundaries

The following handoffs preserve ownership:

Only the authenticated HTTP response is implemented in v1. The other rows are
constraints on possible future consumers, not current integrations.

| From Data Processing | Consumer | Allowed handoff | Not allowed |
|----------------------|----------|-----------------|-------------|
| Forecast or estimate | Authenticated HTTP caller | Return typed derived data and provenance | Expose arbitrary source data or processor secrets |
| Detection score | Alarm application | Submit a typed observation for alarm policy evaluation | Create, acknowledge, or clear an alarm inside the processor |
| Precomputed result snapshot | Future deterministic rule or scheduler | Read validated, unexpired data without network I/O in the hot path | Synchronously call a remote processor during rule execution |
| Forecast or other derived data | Future planner/optimizer | Use as one bounded-quality planning input | Treat the result as a device command |
| Proposed action from a future planner | `ControlApplication` | Re-authorize, constrain, confirm, audit, and dispatch normally | Bypass control policy because a processor produced the input |

Automated control therefore uses a separate sequence:

```
future scheduled planning cycle
        │
        ▼
DataProcessingApplication.process
        │ validated, unexpired DerivedData
        ▼
planner / optimizer / deterministic policy
        │ proposed actions
        ▼
safety constraints + permission + confirmation + audit
        │
        ▼
ControlApplication
        │
        ▼
existing SHM + UDS command path → aether-io → device
```

When processing is unavailable, a planning cycle either uses an explicitly
configured deterministic fallback or skips the cycle. It does not continue an
expired plan or replay a stale command when a device reconnects. Acquisition,
rules that do not depend on derived data, alarms, and safety behavior continue.

AI can explain input watermarks, missingness, processor/artifact versions,
fallbacks, and expiry because those facts are in the result contract. It does
not gain a route from `DerivedData` to SHM or a device.

## Failure behavior

Failures are expected at the edge and have deterministic, observable outcomes.

| Failure | Required behavior |
|---------|-------------------|
| Unknown, disabled, or incompatible task/binding | Reject before reading data or invoking a processor |
| History unavailable or watermark too old | Mark the frame unavailable; use only a task-declared fallback that does not invent observations |
| SHM unavailable, unwritten, or stale | Do not substitute Redis, a mirror, or zero; continue only if the task explicitly permits history-only input |
| Required covariate missing or issued after `as_of` | Reject the frame or use a named covariate fallback with degraded quality |
| Excessive gaps, clock skew, or unit/sign mismatch | Reject during frame validation; do not ask the processor to guess |
| Processor deadline, disconnect, overload, or circuit open | Return processor unavailable; bound retries and leave acquisition/control unaffected |
| Processor contract or artifact mismatch | Reject the complete result and surface the incompatibility |
| NaN, infinity, wrong shape, bad timestamps, or invalid task constraints | Reject the complete result as invalid processor output |
| Processor uses an approved fallback | Preserve its name and degraded quality so consumers can apply task policy |
| Planning cannot obtain usable derived data | Skip the cycle or run an approved deterministic fallback; do not dispatch a stale action |
| AI client disconnected | Deterministic edge behavior continues; Data Processing is not in a hard real-time loop |

Every failure retains a request ID and machine-readable category. Observability
may record source watermarks, missingness, durations, processor health,
artifact identity, and validation reason while avoiding raw
sensitive frame payloads unless an explicit retention policy permits them.

## Boundary summary

```
raw protocol bytes
        │
        ▼
IO decode and canonical T/S publication
        │
        ▼
SHM live authority ──► history persistence/query
        │                       │
        └──────────┬────────────┘
                   ▼
          Aether Data Processing
   bounded ProcessingFrame → DataProcessor
                   │
                   ▼
       validated, expiring DerivedData
          │          │           │
          ▼          ▼           ▼
       query UI   alarm policy  planner
                                 │
                                 ▼
                         ControlApplication
```

Bulk ETL, warehouses, dashboards, and arbitrary BI queries sit outside this
operational edge path. Deterministic rules and alarm lifecycle remain their
own applications. Data Processing connects governed observations to bounded
derived results; it does not absorb the systems on either side.

## Related pages

- [Aether Data Processing](data-processing.md) — purpose, task contract, processor boundary, and non-goals
- [Data Flow](data-flow.md) — authoritative uplink and control paths
- [Shared Memory](shared-memory.md) — current live-state authority and writer ownership
- [System Architecture](architecture.md) — runtime services and downstream adapters
- [Rule Engine](rule-engine.md) — deterministic scheduling and execution
- [Safe Operations for Applications and Agents](../guides/safe-operations.md) — control safety and authorization policy
