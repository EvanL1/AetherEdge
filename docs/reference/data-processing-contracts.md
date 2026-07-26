---
title: Data Processing Contracts
description: Version 1 contracts for processing frames, requests, derived results, validation, and failure semantics
updated: 2026-07-11
---

# Data Processing Contracts

This reference specifies the implemented version 1 contracts for **Aether Data
Processing**. Rust domain values and orchestration live in `aether-domain`,
`aether-ports`, and `aether-application`; `aether-data-processing` provides the
strict transport-neutral JSON codec, and `extensions/http-data-processor`
implements the optional HTTP transport.

The contract encodes one architectural rule: Aether assembles a complete input
frame, then requests a processor. A `DataProcessor` never receives credentials
or callbacks with which to read Aether's SHM, history database, or site
configuration.

## Contract family

| Contract | Identifier | Purpose |
|----------|------------|---------|
| Task declaration | `aether.data-processing-task.v1` | Portable domain semantics and input bindings |
| Application request | `aether.data-processing.process-task-request.v1` | Select a commissioned task, binding, data cut, and typed options |
| Processing frame | `aether.processing-frame.v1` | Aligned observations and known-future covariates |
| Processor request | `aether.data-processing.request.v1` | Resolved task and binding, deadline, complete frame, digest, and typed options |
| Result envelope | `aether.data-processing.result.v1` | Status, provenance, expiry, and typed derived data |
| Forecast output | `aether.data-processing.output.forecast.v1` | Processor-produced time-indexed forecast values |
| Accepted derived data | `aether.derived-data.v1` | Aether-stamped, validated task output |
| Error envelope | `aether.data-processing.error.v1` | Typed transport or processor failure |

The HTTP adapter uses the media type:

```text
application/vnd.aether.data-processing+json;version=1
```

Contract identifiers are exact, case-sensitive strings. Version 1 consumers
must reject an unsupported major version rather than guessing how to interpret
it. Additive fields may be introduced only where the schema explicitly permits
them; implementations must not use unknown fields to smuggle vendor-specific
commands through the common envelope.

The keywords **MUST**, **MUST NOT**, **SHOULD**, and **MAY** below are normative.

## Common value rules

- Times MUST be RFC 3339 UTC strings ending in `Z`, at or after the Unix epoch,
  with no finer than millisecond precision. They map losslessly to
  `TimestampMs`; encoders may omit a zero fractional part, but MUST NOT emit
  more than three fractional digits. Processors MUST NOT silently reinterpret
  a local time or a numeric epoch value.
- Durations and cadences MUST be integer seconds greater than zero.
- Numeric values MUST be finite JSON numbers. `NaN`, positive infinity, and
  negative infinity are invalid.
- Missing samples MUST be JSON `null` and have sample quality `missing`.
- Units MUST be explicit for numeric features and outputs. A task declaration
  chooses the canonical unit. The current v1 runtime verifies that commissioned
  source metadata already matches that unit and sign convention; it does not
  convert either at request time.
- Feature names MUST match the task declaration exactly and be unique within a
  segment.
- Timestamps MUST be strictly increasing after Aether has resolved duplicates.
- Digests use lowercase hexadecimal SHA-256 with the prefix `sha256:`.

## `ProcessingFrame`

A `ProcessingFrame` carries all data the selected processor is permitted to
use. It has historical observations, optional known-future covariates, optional
static features, aggregate quality, and redaction-safe provenance.

### Shape

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
      "temp_avg": {
        "value_type": "number",
        "unit": "Cel",
        "values": [32.1, 32.0],
        "quality": ["good", "good"]
      },
      "quarter_hour": {
        "value_type": "number",
        "unit": "1",
        "values": [49, 50],
        "quality": ["good", "good"]
      }
    }
  },
  "static_features": {
    "rated_power": {
      "value_type": "number",
      "unit": "kW",
      "value": 2500.0,
      "quality": "good"
    }
  },
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
    },
    {
      "segment": "future_covariates",
      "feature": "temp_avg",
      "source_kind": "covariate",
      "source_ref": "weather.nwp.air_temperature",
      "watermark": "2026-07-11T11:50:00Z",
      "issued_at": "2026-07-11T11:40:00Z"
    },
    {
      "segment": "future_covariates",
      "feature": "quarter_hour",
      "source_kind": "calendar",
      "source_ref": "calendar.utc.quarter_hour",
      "watermark": "2026-07-11T12:00:00Z"
    },
    {
      "segment": "static_features",
      "feature": "rated_power",
      "source_kind": "constant",
      "source_ref": "energy.site.rated_power",
      "watermark": "2026-07-11T12:00:00Z"
    }
  ]
}
```

### Top-level fields

| Field | Required | Validation |
|-------|----------|------------|
| `schema` | yes | Exactly `aether.processing-frame.v1` |
| `as_of` | yes | Logical cutoff for the request; UTC RFC 3339 |
| `cadence_seconds` | yes | Positive integer matching the task revision |
| `history` | yes | At least one timestamp and one declared feature |
| `future_covariates` | task-dependent | Required for tasks that declare future-known inputs; otherwise omitted |
| `static_features` | no | Only features declared by the task; empty object is allowed |
| `quality` | yes | Aggregate quality calculated by Aether |
| `provenance` | yes | Exactly one entry for every populated `(segment, feature)` pair; only `source_ref` may be omitted by egress policy |

The generic contract includes `static_features`, but the current opt-in
`aether-api` runtime YAML loader cannot bind static values. They are available
only to a custom composition using `DataProcessingBinding` until that loader is
extended and tested. The shipped load/PV runtime route declares none.

### Segment schema

Both `history` and `future_covariates` use the same structural schema:

```json
{
  "timestamps": ["2026-07-11T12:15:00Z"],
  "features": {
    "feature_name": {
      "value_type": "number",
      "unit": "kW",
      "values": [123.4],
      "quality": ["good"]
    }
  }
}
```

`value_type` is one of `number`, `string`, or `boolean`. Every series in a
segment MUST have exactly as many values and quality entries as the segment has
timestamps. Sample quality is one of:

| Quality | Meaning |
|---------|---------|
| `good` | Value in the declared task representation accepted by the source adapter and task policy |
| `uncertain` | Source supplied a value but marked its confidence as reduced |
| `substituted` | Aether filled the sample using the task's declared method |
| `missing` | No usable value; the corresponding value is `null` |

For numeric series, `unit` is required. For string and boolean series, `unit`
MUST be omitted. `null` is permitted only when the task's missing policy allows
it. A processor MUST NOT convert a missing value to zero unless the task
explicitly declares that exact substitution before the frame is built.

### Time invariants

- Version 1 uses interval-end labels. For cadence `c`, a historical label `t`
  represents the raw interval `(t-c, t]`.
- A history grid with `N` steps MUST be
  `as_of-(N-1)c, ..., as_of`; its final label is exactly `as_of`, and source
  reads MUST NOT advance beyond that cutoff.
- A future-covariate grid MUST begin at `as_of+c` and advance by exact cadence.
- Version 1 frames use an exact `cadence_seconds` grid. A source gap is retained
  as an explicit missing sample or rejected by task policy; timestamps are not
  silently removed or retimed.
- Future target values MUST NOT appear in `future_covariates`. A feature is
  allowed there only when the task marks it as known ahead of time.
- `input_watermark` MUST be less than or equal to `as_of` and represent the
  newest source observation considered, not the request creation time.

### Aggregate quality

| Field | Required | Meaning |
|-------|----------|---------|
| `input_watermark` | yes | Newest actual observation considered by assembly |
| `missing_ratio` | yes | Missing cells divided by all required cells, in `[0, 1]` |
| `max_gap_seconds` | yes | Largest gap among required historical observations |
| `live_tail_included` | yes | Whether read-only live state contributed after stored history |
| `substituted_samples` | yes | Count of values labeled `substituted` |

The aggregate does not replace per-sample quality. A processor validates both
against the selected task and optional processor artifact manifest.

The v1 wire contract can carry per-sample quality, but the current production
sources do not preserve device-origin quality end to end. The embedded history
table stores numeric observations without device quality, and the current SHM
bridge labels accepted finite live values as `good`. Current commissioning can
therefore enforce freshness, gaps, missingness, numeric constraints,
provenance, and issue time, but deployments that require original device
quality MUST add a quality-bearing source adapter.

Live tail is permitted only for a history feature whose commissioned
aggregation is `Last`, where one instantaneous SHM value can validly replace
the final cell. Version 1 rejects live tail for `Mean`, `Sum`, `Min`, and `Max`.
The current energy load and PV target histories use `Mean`, so their frames set
`live_tail_included: false`.

### Provenance

`segment` is `history`, `future_covariates`, or `static_features` and identifies
which occurrence of a feature the entry describes. `source_kind` is one of
`history`, `live`, `history_and_live`, `covariate`, `calendar`, or `constant`.
`source_ref` is a semantic identifier, never a SQL query, database credential,
SHM path, channel address, or model filesystem path. When a remote data-egress
policy removes `source_ref`, it MUST retain the segment, feature name, source
kind, and watermark.

`issued_at` is optional and records when a versioned external forecast, such
as an NWP run, was issued. When present, it MUST satisfy
`issued_at <= watermark <= frame.as_of`. Valid times in
`future_covariates.timestamps` may be later than `as_of`; the issue cut may
not. This prevents assembly from selecting a covariate forecast unavailable at
the requested event-time cut. It does not make the current history store or
artifact selector point-in-time safe for backtesting.

Every populated `(segment, feature)` pair MUST have exactly one provenance
entry. Missing entries, duplicate keys with different metadata, and entries
for absent features are invalid. Deterministically generated calendar values
and commissioned constants are not exceptions: they use `calendar` and
`constant` provenance respectively. Their watermark records the data cut used
to derive the value, but they do not advance aggregate `input_watermark`, which
means the newest actual observation considered by assembly.

### Implemented history adapters

The production `aether-api` composition uses `SqliteHistoryQuery` by default.
It opens the existing `aether-history.db` schema lazily and read-only, never
creates or migrates it, and performs all feature reads for one logical request
inside one SQLite read transaction. A task-scoped route fixes each feature's
physical series, unit, cadence, aggregation, and duplicate policy. Raw numeric
rows are reduced into interval-end buckets; empty buckets remain explicit
missing values, and the watermark is the newest numeric raw observation that
actually participated rather than the bucket label.

`aether-history` remains the schema, write, retention, and file-lifecycle owner.
The query adapter relies on SQLite snapshot/WAL behavior and deployment file
permissions; a missing or inaccessible file makes only the processing request
typed-unavailable and can recover on a later call. Selecting an external
historian does not silently redirect this adapter—the current runtime must be
recomposed with a conforming `HistoryQuery` implementation.

`mode=ro` and `query_only=ON` constrain SQLite operations but do not replace
OS isolation. Production MUST give the API principal only read permission to a
dedicated historian directory containing the database/WAL/SHM family, while
keeping its own configuration/audit database separately writable. The base
Compose currently mounts `/app/data` read-write into `aether-api`; it is a
development baseline, not a completed production permission boundary for
direct historian reads.

The transaction snapshot is read-consistent when the invocation runs, but the
schema is not bitemporal. Rows have event `time_ms` only: no `ingested_at` and
no source, binding, or configuration epoch. Therefore `as_of` is not a
point-in-time database reconstruction. Late backfills with old event times can
change a later replay, and remapping a device behind the same logical
`(series_key, point_id)` can splice source epochs. Expected task/binding
revisions validate current commissioning but cannot filter metadata absent
from stored rows. Leakage-safe offline evaluation MUST use a frozen historian
snapshot/export captured at the evaluation cut or another adapter with
ingestion-time and source-epoch filtering.

The composition currently validates its path against persisted
`history_config.storage_*`, but those values are saved intent rather than an
attestation of the active writer. `PUT /hisApi/storage` does not reconnect the
historian. Across storage changes, disable Data Processing, reconnect or
restart `aether-history`, verify the active SQLite backend and a commissioned
sentinel series, then restart `aether-api` with the matching path.

`HttpHistoryQuery` is an optional loopback adapter for an upstream service that
already materializes the exact cadence grid. It accepts only
`aggregation=last` and `duplicate_policy=reject`; it is not a substitute for
raw SQLite aggregation.

## `ProcessTaskRequest`

Application callers select a commissioned task and an event-time data cut. They do
not submit a frame, endpoint, processor ID, artifact selector, credentials, or
source query.

```json
{
  "task_id": "energy.site-load-forecast",
  "expected_task_revision": 1,
  "binding_id": "site-a",
  "expected_binding_revision": 7,
  "as_of": "2026-07-11T12:00:00Z",
  "options": {
    "kind": "forecast",
    "horizon_steps": 2
  }
}
```

The transport supplies request identity and actor data through the common
`RequestContext`; they are not duplicated in this typed request.
`expected_task_revision` and `expected_binding_revision` make an invocation fail
closed when configuration changed. `binding_id` names an enabled, commissioned
binding; the application resolves its points, covariates, processor route,
artifact policy, and egress policy atomically.

## `DataProcessingRequest`

This is the processor-facing request assembled by Aether. It is never the
public application input.

### Schema

```json
{
  "schema": "aether.data-processing.request.v1",
  "request_id": "0190aee6-2139-7a87-8448-806f1b843201",
  "submitted_at": "2026-07-11T12:00:01Z",
  "deadline": "2026-07-11T12:00:06Z",
  "task": {
    "id": "energy.site-load-forecast",
    "revision": 1,
    "kind": "forecast"
  },
  "binding": {
    "id": "site-a",
    "revision": 7
  },
  "processor_contract": "aether.data-processing.forecast.v1",
  "artifact": {
    "kind": "model",
    "family": "site-load",
    "version": "v3",
    "artifact_digest": "sha256:98967bdedc60b8ab555e596516eb272063c139ccf3a3112fb29a46ab0610f270"
  },
  "frame": {
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
        },
        "temp_avg": {
          "value_type": "number",
          "unit": "Cel",
          "values": [31.0, 31.2],
          "quality": ["good", "good"]
        },
        "humidity": {
          "value_type": "number",
          "unit": "%",
          "values": [64.0, 63.0],
          "quality": ["good", "good"]
        },
        "rain": {
          "value_type": "number",
          "unit": "mm",
          "values": [0.0, 0.0],
          "quality": ["good", "good"]
        },
        "quarter_hour": {
          "value_type": "number",
          "unit": "1",
          "values": [47, 48],
          "quality": ["good", "good"]
        }
      }
    },
    "future_covariates": {
      "timestamps": ["2026-07-11T12:15:00Z", "2026-07-11T12:30:00Z"],
      "features": {
        "temp_avg": {
          "value_type": "number",
          "unit": "Cel",
          "values": [32.1, 32.0],
          "quality": ["good", "good"]
        },
        "humidity": {
          "value_type": "number",
          "unit": "%",
          "values": [61.0, 62.0],
          "quality": ["good", "good"]
        },
        "rain": {
          "value_type": "number",
          "unit": "mm",
          "values": [0.0, 0.0],
          "quality": ["good", "good"]
        },
        "quarter_hour": {
          "value_type": "number",
          "unit": "1",
          "values": [49, 50],
          "quality": ["good", "good"]
        }
      }
    },
    "static_features": {},
    "quality": {
      "input_watermark": "2026-07-11T12:00:00Z",
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
        "watermark": "2026-07-11T12:00:00Z"
      },
      {
        "segment": "history",
        "feature": "temp_avg",
        "source_kind": "history",
        "source_ref": "weather.observed.air_temperature",
        "watermark": "2026-07-11T12:00:00Z"
      },
      {
        "segment": "history",
        "feature": "humidity",
        "source_kind": "history",
        "source_ref": "weather.observed.relative_humidity",
        "watermark": "2026-07-11T12:00:00Z"
      },
      {
        "segment": "history",
        "feature": "rain",
        "source_kind": "history",
        "source_ref": "weather.observed.precipitation",
        "watermark": "2026-07-11T12:00:00Z"
      },
      {
        "segment": "history",
        "feature": "quarter_hour",
        "source_kind": "calendar",
        "source_ref": "calendar.utc.quarter_hour",
        "watermark": "2026-07-11T12:00:00Z"
      },
      {
        "segment": "future_covariates",
        "feature": "temp_avg",
        "source_kind": "covariate",
        "source_ref": "weather.nwp.air_temperature",
        "watermark": "2026-07-11T11:50:00Z",
        "issued_at": "2026-07-11T11:40:00Z"
      },
      {
        "segment": "future_covariates",
        "feature": "humidity",
        "source_kind": "covariate",
        "source_ref": "weather.nwp.relative_humidity",
        "watermark": "2026-07-11T11:50:00Z",
        "issued_at": "2026-07-11T11:40:00Z"
      },
      {
        "segment": "future_covariates",
        "feature": "rain",
        "source_kind": "covariate",
        "source_ref": "weather.nwp.precipitation",
        "watermark": "2026-07-11T11:50:00Z",
        "issued_at": "2026-07-11T11:40:00Z"
      },
      {
        "segment": "future_covariates",
        "feature": "quarter_hour",
        "source_kind": "calendar",
        "source_ref": "calendar.utc.quarter_hour",
        "watermark": "2026-07-11T12:00:00Z"
      }
    ]
  },
  "options": {
    "kind": "forecast",
    "horizon_steps": 2
  },
  "input_digest": "sha256:8b227777d4dd1fc61c6f884f48641d02b50a8a461a77f8fae7f48e32fbd8c372"
}
```

The digest above is illustrative rather than a digest of the abbreviated
example.

### Request fields

| Field | Required | Validation |
|-------|----------|------------|
| `schema` | yes | Exactly `aether.data-processing.request.v1` |
| `request_id` | yes | Correlation identity for this invocation; v1 provides no replay or de-duplication semantics |
| `submitted_at` | yes | UTC time the application created the request |
| `deadline` | yes | UTC time after which the processor must not start work or return an accepted result |
| `task` | yes | ID, positive revision, and typed kind matching a loaded task |
| `binding` | yes | Resolved commissioned binding ID and positive revision |
| `processor_contract` | yes | Contract advertised by both task and processor |
| `artifact` | no | Generic kind, family, and optional requested version; no path or URL |
| `frame` | yes | A valid `ProcessingFrame` |
| `options` | yes | A typed options object whose `kind` matches `task.kind` |
| `input_digest` | yes | Canonical content identity used for correlation, audit, and offline comparison; not an operation idempotency key |

`options.kind = forecast` requires positive `horizon_steps`. `quantiles` is
optional; if present it contains unique finite numbers strictly between zero
and one in increasing order, and its count MUST NOT exceed the commissioned
task's `max_quantiles`. The current energy load and PV task revisions set that
limit to zero, so their requests omit `quantiles`. A processor must reject
unknown options rather than silently ignore behavior-changing fields.

`input_digest` is SHA-256 over the RFC 8785 canonical JSON representation of:

```json
{
  "task": "<the exact versioned task identity object>",
  "binding": "<the exact versioned binding identity object>",
  "processor_contract": "<contract identifier>",
  "artifact": "<the artifact object or null>",
  "frame": "<the complete frame object>",
  "options": "<the complete typed options object>"
}
```

The strings in this illustration stand for the corresponding JSON values, not
literal strings in the digest input. The task object includes ID, revision,
and kind; the binding object includes ID and revision. The revisions make
changes to their governed definitions content-distinct without copying site
configuration into the processor request. Processor endpoint and identity,
correlation times, and `request_id` are excluded, so independent invocations of
the exact same normalized governed content retain the same digest. Repeating
only `as_of` does not guarantee that content when sources are mutable. Version
1 does not provide a built-in result cache or replay store.

The actor and the actor's permissions are intentionally absent. Aether
authorizes the application call and audits the actor; it does not disclose
identity to a processor unless a separate, explicit service-authentication
protocol requires it.

Artifact identity is not artifact chronology. The selector/result provenance
can carry kind, family, version, and digest, but version 1 has no
`trained_through` or `available_at` field and does not compare either with
`frame.as_of`. A digest-pinned artifact is reproducible once supplied, yet a
model trained or published later can still be selected for an old frame.
Rigorous historical model evaluation MUST freeze the artifact registry at the
evaluation cut until training and availability cuts become normative contract
fields.

## `ProcessingResult`

### Successful forecast

```json
{
  "schema": "aether.data-processing.result.v1",
  "request_id": "0190aee6-2139-7a87-8448-806f1b843201",
  "task": {
    "id": "energy.site-load-forecast",
    "revision": 1,
    "kind": "forecast"
  },
  "binding": {
    "id": "site-a",
    "revision": 7
  },
  "input_digest": "sha256:8b227777d4dd1fc61c6f884f48641d02b50a8a461a77f8fae7f48e32fbd8c372",
  "status": "produced",
  "issued_at": "2026-07-11T12:00:02Z",
  "expires_at": "2026-07-11T13:00:00Z",
  "input_watermark": "2026-07-11T12:00:00Z",
  "processor": {
    "id": "load-forecasting-edge",
    "version": "0.1.0",
    "contract": "aether.data-processing.forecast.v1"
  },
  "artifact": {
    "kind": "model",
    "family": "site-load",
    "version": "v3",
    "artifact_digest": "sha256:f04c532f2f814a3690f0f40e6f26fa82b0d69b9c510e7c0bb9f9f4de35b5a882"
  },
  "output": {
    "schema": "aether.data-processing.output.forecast.v1",
    "kind": "forecast",
    "target": "load",
    "unit": "kW",
    "sign_convention": "positive_consumption",
    "cadence_seconds": 900,
    "timestamp_semantics": "interval_end",
    "points": [
      {
        "timestamp": "2026-07-11T12:15:00Z",
        "value": 846.2
      },
      {
        "timestamp": "2026-07-11T12:30:00Z",
        "value": 852.7
      }
    ]
  },
  "warnings": []
}
```

### Common result fields

| Field | Required | Validation |
|-------|----------|------------|
| `schema` | yes | Exactly `aether.data-processing.result.v1` |
| `request_id` | yes | Exact request correlation |
| `task` | yes | Exact task ID, revision, and kind from the request |
| `binding` | yes | Exact binding ID and revision from the request |
| `input_digest` | yes | Exact digest from the request |
| `status` | yes | `produced`, `fallback`, or `unavailable` |
| `issued_at` | yes | UTC time the result was completed |
| `expires_at` | produced/fallback | After `issued_at`, bounded by task policy |
| `input_watermark` | yes | Exact accepted frame watermark |
| `processor` | yes | Stable processor identity, version, and contract |
| `artifact` | no | Actual generic artifact kind, family, version, and digest when one was used |
| `output` | produced/fallback | Typed processor output; forbidden for `unavailable` |
| `fallback` | fallback | Strategy, reason code, and data basis |
| `unavailable` | unavailable | Reason code and retry guidance |
| `warnings` | yes | Array of stable warning codes; empty when there are none |

### Forecast output

Version 1 initially defines the typed forecast output schema. Estimate,
detection, and classification tasks should add their own versioned output
schemas instead of placing arbitrary JSON under `output`.

Version 1 fixes `timestamp_semantics` to `interval_end`: every point timestamp
identifies the end of the interval it forecasts. `interval_start`, `instant`,
or any other interpretation requires a future contract version and MUST be
rejected by a v1 decoder.

A forecast MUST satisfy all of the following:

- `target`, `unit`, and `sign_convention` exactly match the task declaration;
- point timestamps exactly match the requested future horizon and cadence;
- the number of points equals `options.horizon_steps`;
- timestamps are strictly increasing and use v1 `interval_end` semantics;
- values and quantile values are finite;
- returned quantile probabilities exactly match the requested set;
- quantile values are nondecreasing by probability at each timestamp; and
- when probability `0.5` is returned, the task declares whether it must equal
  the primary `value` or may be a separate estimator.

Aether must reject a syntactically successful processor response that violates
these invariants.

## Fallback semantics

Fallback is usable processor output produced by a named, approved strategy. It
becomes derived data only after Aether validates and stamps it; fallback is not
a way to hide a processor failure.

```json
{
  "schema": "aether.data-processing.result.v1",
  "request_id": "0190aee6-2139-7a87-8448-806f1b843201",
  "task": {
    "id": "energy.site-load-forecast",
    "revision": 1,
    "kind": "forecast"
  },
  "binding": {
    "id": "site-a",
    "revision": 7
  },
  "input_digest": "sha256:8b227777d4dd1fc61c6f884f48641d02b50a8a461a77f8fae7f48e32fbd8c372",
  "status": "fallback",
  "issued_at": "2026-07-11T12:00:02Z",
  "expires_at": "2026-07-11T12:30:00Z",
  "input_watermark": "2026-07-11T12:00:00Z",
  "processor": {
    "id": "load-forecasting-edge",
    "version": "0.1.0",
    "contract": "aether.data-processing.forecast.v1"
  },
  "fallback": {
    "strategy": "persistence",
    "strategy_version": "1",
    "reason_code": "MODEL_UNAVAILABLE",
    "source_feature": "load",
    "based_on_data_through": "2026-07-11T11:45:00Z"
  },
  "output": {
    "schema": "aether.data-processing.output.forecast.v1",
    "kind": "forecast",
    "target": "load",
    "unit": "kW",
    "sign_convention": "positive_consumption",
    "cadence_seconds": 900,
    "timestamp_semantics": "interval_end",
    "points": [
      {"timestamp": "2026-07-11T12:15:00Z", "value": 835.0},
      {"timestamp": "2026-07-11T12:30:00Z", "value": 835.0}
    ]
  },
  "warnings": ["MODEL_FALLBACK_USED"]
}
```

Fallback rules:

- the task MUST explicitly allow the named strategy;
- the response MUST use `status: fallback` and include the reason;
- `expires_at` SHOULD be shorter than for normal model output;
- a persistence or historical-average strategy MUST use actual request data;
- a zero series is valid only when the task explicitly defines zero as its
  baseline and the response still labels it as fallback; and
- consumers decide whether a particular fallback is acceptable. A processor
  cannot silently promote fallback to `produced`.

For power data, zero is a plausible physical value. Returning zeros after a
technical failure without a fallback label is therefore especially dangerous.

## Unavailable semantics

Use `status: unavailable` when the processor handled a valid request but
cannot produce any result that satisfies the task policy—for example, no
approved model exists or the allowed fallback lacks enough observations.

```json
{
  "schema": "aether.data-processing.result.v1",
  "request_id": "0190aee6-2139-7a87-8448-806f1b843201",
  "task": {
    "id": "energy.site-load-forecast",
    "revision": 1,
    "kind": "forecast"
  },
  "binding": {
    "id": "site-a",
    "revision": 7
  },
  "input_digest": "sha256:8b227777d4dd1fc61c6f884f48641d02b50a8a461a77f8fae7f48e32fbd8c372",
  "status": "unavailable",
  "issued_at": "2026-07-11T12:00:02Z",
  "input_watermark": "2026-07-11T12:00:00Z",
  "processor": {
    "id": "load-forecasting-edge",
    "version": "0.1.0",
    "contract": "aether.data-processing.forecast.v1"
  },
  "unavailable": {
    "reason_code": "INSUFFICIENT_HISTORY",
    "retryable": true,
    "retry_after_seconds": 900
  },
  "warnings": []
}
```

`output`, `expires_at`, and an apparently usable default value are
forbidden in this status.

## Accepted `DerivedData`

`ProcessingResult` is untrusted processor output. After all correlation,
schema, range, provenance, and expiry checks pass, Aether stamps the accepted
output as `DerivedData`:

```json
{
  "schema": "aether.derived-data.v1",
  "result_id": "0190aee6-22ac-72da-b214-629a31ccb99c",
  "request_id": "0190aee6-2139-7a87-8448-806f1b843201",
  "task": {
    "id": "energy.site-load-forecast",
    "revision": 1,
    "kind": "forecast"
  },
  "binding": {
    "id": "site-a",
    "revision": 7
  },
  "accepted_at": "2026-07-11T12:00:03Z",
  "expires_at": "2026-07-11T13:00:00Z",
  "input_digest": "sha256:8b227777d4dd1fc61c6f884f48641d02b50a8a461a77f8fae7f48e32fbd8c372",
  "processing_status": "produced",
  "processor": {
    "id": "load-forecasting-edge",
    "version": "0.1.0"
  },
  "artifact": {
    "kind": "model",
    "family": "site-load",
    "version": "v3",
    "artifact_digest": "sha256:f04c532f2f814a3690f0f40e6f26fa82b0d69b9c510e7c0bb9f9f4de35b5a882"
  },
  "quality": {
    "input_watermark": "2026-07-11T12:00:00Z",
    "missing_ratio": 0.0,
    "fallback_used": false
  },
  "data": {
    "schema": "aether.data-processing.output.forecast.v1",
    "kind": "forecast",
    "target": "load",
    "unit": "kW",
    "sign_convention": "positive_consumption",
    "cadence_seconds": 900,
    "timestamp_semantics": "interval_end",
    "points": [
      {"timestamp": "2026-07-11T12:15:00Z", "value": 846.2},
      {"timestamp": "2026-07-11T12:30:00Z", "value": 852.7}
    ]
  }
}
```

If validation fails, the application records a `rejected` outcome and does not
create `DerivedData`. `rejected` is therefore an application outcome, not a
status a processor may assert about its own response.

## Error envelope and HTTP mapping

Contract, transport, capacity, and unexpected processor failures use a non-2xx
response with a typed error. They are distinct from a completed
`ProcessingResult` whose status is `unavailable`.

```json
{
  "schema": "aether.data-processing.error.v1",
  "request_id": "0190aee6-2139-7a87-8448-806f1b843201",
  "code": "FRAME_INVALID",
  "category": "invalid_data",
  "message": "history.load contains a missing sample",
  "retryable": false,
  "details": {
    "path": "/frame/history/features/load/values/41",
    "rule": "task missing_policy is reject"
  }
}
```

| HTTP | Category | Example codes | Retry |
|------|----------|---------------|-------|
| 400 | `invalid_request` | `SCHEMA_UNSUPPORTED`, `OPTION_UNKNOWN` | no |
| 401/403 | `authorization` | `PROCESSOR_AUTH_REQUIRED`, `PROCESSOR_AUTH_DENIED` | only after credentials/policy change |
| 404 | `not_found` | `MODEL_NOT_FOUND`, `TASK_NOT_SUPPORTED` | no unless deployment changes |
| 413 | `resource_limit` | `FRAME_TOO_LARGE` | only after reducing the request |
| 422 | `invalid_data` | `FRAME_INVALID`, `QUALITY_REJECTED`, `UNIT_UNSUPPORTED` | only after data changes |
| 429 | `capacity` | `PROCESSOR_BUSY` | yes, honor `retry_after_seconds` |
| 500 | `internal` | `PROCESSOR_INTERNAL` | policy-dependent |
| 503 | `unavailable` | `MODEL_RUNTIME_UNAVAILABLE` | yes when marked retryable |
| 504 | `timeout` | `DEADLINE_EXCEEDED` | yes with a fresh deadline |

Error messages and details MUST NOT expose credentials, model filesystem paths,
SQL statements, raw stack traces, or undeclared source data. Aether adapters
map these errors into typed port errors and retain the processor's stable code
for diagnostics.

## Validation order

A conforming application/adapter should validate in this order:

1. size, media type, JSON syntax, and contract major version;
2. request identity, deadline, and canonical digest;
3. configured task ID, revision, kind, and processor route;
4. feature set, value types, units, and sign conventions;
5. timestamp order, cadence, window boundaries, and array lengths;
6. sample and aggregate quality against task policy;
7. optional artifact selector against the processor descriptor; and
8. result correlation, provenance, typed output schema, horizon, and expiry.

Validation failures never trigger device writes and never mutate SHM. A failed
processor call also cannot make history, acquisition, alarms, or deterministic
safety behavior unavailable.

## Non-idempotent execution and retry

`data_processing.process` is declared `idempotent: false`. Although the query
does not mutate Aether state or control a device, invoking it may execute local
or remote processor work and create a new required audit record. Version 1 has
no replay store, request de-duplication contract, exact-result guarantee, or
special `409` behavior for reused request IDs.

The input digest is exact content identity, not operation identity. A caller
may retry only when a typed error marks the failure retryable, and should honor
retry metadata, use a fresh deadline, and assume that prior processor work may
already have run. Deterministic implementations can use a pinned frame and
artifact in offline golden tests; that property does not turn the public
operation into an exact-replay API.

## Capability metadata

Every transport that wires this capability MUST consume and expose the same
application metadata. Version 1 currently exposes the application through the
authenticated HTTP routes on `aether-api`; CLI and MCP bindings remain future
work. The implemented baseline descriptors in the application catalog are:

| Capability | Kind | Risk | Permission | Confirmation | Audit | Idempotent |
|------------|------|------|------------|--------------|-------|------------|
| `data_processing.tasks.list` | Query | Low | `data_processing.read` | Never | Not required | yes |
| `data_processing.processors.health` | Query | Low | `data_processing.read` | Never | Not required | yes |
| `data_processing.process` | Query | Medium | `data_processing.run` | Policy | Required | no |

`data_processing.process` remains a query because it produces derived data and
does not mutate device or Aether state. It is conservatively Medium risk with
policy-driven confirmation because a configured remote processor may cause
telemetry to leave the edge host. A deployment can approve a local-only route
without per-call confirmation; the route's data boundary remains discoverable.
Task and health discovery do not include observation values and do not require
durable audit records. Processing does read task-scoped operational data, so it
fails closed when the required audit sink cannot record the invocation.

The application-facing routes are:

- `GET /api/v1/data-processing/tasks`;
- `GET /api/v1/data-processing/processors/health`; and
- `POST /api/v1/data-processing/process`.

They are mounted only when Data Processing is explicitly enabled. JWT role
mapping grants discovery to Viewer, Engineer, and Admin; process execution is
limited to Engineer and Admin. The processor-facing sidecar route remains the
separate `POST /v1/process` boundary.

Task bindings, routes, and approved artifacts change only through the existing
governed configuration path. A processing request MUST NOT activate an
artifact as a side effect. Device control is also separate and remains a
High-risk command through `ControlApplication`.

Task discovery returns the actual commissioned route policy. A representative
entry (with the full `features` array abbreviated here) has this nested shape:

```json
{
  "task": {"id": "energy.site-load-forecast", "revision": 1},
  "binding": {"id": "energy.example-site", "revision": 1},
  "kind": "forecast",
  "processor_contract": "aether.data-processing.forecast.v1",
  "features": [
    {
      "name": "load",
      "role": "history",
      "value_type": "number",
      "unit": "kW",
      "integer": false
    }
  ],
  "forecast": {
    "target": {
      "name": "load",
      "unit": "kW",
      "sign_convention": "positive_consumption"
    },
    "cadence_ms": 900000,
    "history_aggregation": "mean",
    "history_duplicate_policy": "latest",
    "history_feature_policies": [
      {"feature": "load", "aggregation": "mean", "duplicate_policy": "latest"},
      {"feature": "temp_avg", "aggregation": "mean", "duplicate_policy": "latest"},
      {"feature": "humidity", "aggregation": "mean", "duplicate_policy": "latest"},
      {"feature": "rain", "aggregation": "sum", "duplicate_policy": "latest"},
      {"feature": "quarter_hour", "aggregation": "last", "duplicate_policy": "reject"}
    ],
    "history_steps": 672,
    "max_horizon_steps": 288,
    "max_quantiles": 0,
    "max_output_age_ms": 3600000,
    "max_missing_ratio": 0.0,
    "max_input_age_ms": 900000,
    "max_gap_ms": 1800000,
    "require_future_issue_time": true,
    "allowed_fallbacks": ["persistence"],
    "fallback_policies": [
      {
        "strategy": "persistence",
        "version": "1",
        "source_feature": "load",
        "max_output_age_ms": 1800000
      }
    ]
  },
  "artifact": {
    "kind": "model",
    "family": "site-load",
    "version": "v3",
    "digest": "sha256:98967bdedc60b8ab555e596516eb272063c139ccf3a3112fb29a46ab0610f270"
  },
  "processor_id": "load-forecasting-edge",
  "processor_version": "0.1.0",
  "data_boundary": "local",
  "deadline_ms": 5000,
  "audit_finalization_timeout_ms": 1000,
  "max_concurrency": 1,
  "max_frame_samples": 5000,
  "max_request_bytes": 4194304
}
```

`deadline_ms` is the hard budget for frame assembly plus processor work, not
the complete HTTP response SLA. `audit_finalization_timeout_ms` publishes the
separate mandatory terminal-audit allowance, so an observed API call can
complete up to the sum of those two fields. Audit failure still fails the
request closed.

An AI client can then distinguish observing or explaining a forecast from
activating a model or dispatching a control plan.

## Compatibility rules

- A processor may support multiple major contracts concurrently, but each
  request selects exactly one.
- A new task kind receives a typed options schema and a typed output schema.
  Do not expand Forecast fields until they become a generic blob.
- Renaming a feature, changing a unit or sign convention, changing timestamp
  semantics, or changing a missing-data rule requires a new task revision.
- Removing a compatibility adapter requires conformance tests and a stated
  migration criterion.
- A domain pack may require a minimum processor contract, but only a
  composition root selects the actual adapter or endpoint.

## Related pages

- [Connect Data Processors](../guides/data-processors.md) — declare a task and route a processor
- [AetherEMS Power Forecasting](https://github.com/EvanL1/AetherEMS/blob/main/packs/energy/knowledge/power-forecasting.md) — first downstream forecast contract
- [JSON Schemas](../../contracts/data-processing/README.md) — strict machine-readable v1 wire guards
- [Load-Forecasting Processor](https://github.com/EvanL1/AetherEMS/tree/main/processors/load-forecasting) — downstream energy-domain implementation
- [Data Flow](../concepts/data-flow.md) — SHM and history authority
- [HTTP Data Processor](../../extensions/http-data-processor/README.md) — bounded optional implementation of the v1 processor transport
- [HTTP API](http-api.md) — service-envelope conventions for Aether's application-facing APIs
