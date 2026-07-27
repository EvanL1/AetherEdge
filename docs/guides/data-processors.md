---
title: Connect Data Processors
description: Declare data-processing tasks in domain packs and connect request-driven local or remote processors
updated: 2026-07-11
---

# Connect Data Processors

This page explains the implemented integration pattern for **Aether Data
Processing**. The core types, application orchestration, v1 codec, local test
adapters, bounded HTTP adapter, schemas, and energy-pack examples are present
in this repository. The opt-in `aether-api` composition reads a strict runtime
configuration; the complete synthetic template is
[`packs/energy/data-processing/runtime.example.yaml`](../../packs/energy/data-processing/runtime.example.yaml).

A data processor receives a complete, governed input frame and returns derived
data. It does not reach back into Aether to discover its own inputs. This makes
the same processor usable as an in-process adapter, a local sidecar, or an
approved remote service without changing data ownership.

```text
caller
  │
  ▼
DataProcessingApplication
  ├─ task declaration from a domain pack
  ├─ HistoryQuery
  ├─ LiveState (read-only)
  ├─ CovariateSource
  └─ deterministic transforms
            │
            ▼
      ProcessingFrame
            │ request
            ▼
       DataProcessor
            │
            ▼
 ProcessingResult (untrusted)
            │ validate and stamp
            ▼
        DerivedData
```

The split is deliberate:

- Aether owns point identity, source selection, time alignment, unit/sign
  contract validation, quality policy, authorization, and auditing.
- A `DataProcessor` owns processor-specific feature ordering, scalers, tensor
  construction, model execution, and output post-processing.
- A domain pack declares the semantics of a task. It does not select a
  concrete endpoint or contain credentials.
- Derived data is not live device state. A processor cannot write SHM, history
  storage, or device commands.

## Declare a task in a domain pack

Place task declarations with the industry knowledge that defines their input
and output semantics. The repository convention is
`packs/<domain>/data-processing/tasks/*.yaml`; the energy pack ships complete,
disabled-by-default load and PV examples there. A composition root or
commissioning loader translates validated assets into enabled routes.

The following load-forecast task is illustrative:

```yaml
schema: aether.data-processing-task.v1
id: energy.site-load-forecast
revision: 1
kind: forecast
processor_contract: aether.data-processing.forecast.v1

target:
  name: load
  semantic_point: site.load.active_power
  unit: kW
  sign_convention: positive_consumption

frame:
  cadence_seconds: 900
  history_steps: 672
  horizon_steps: 288
  timezone: UTC
  live_tail: forbidden

inputs:
  history:
    - name: load
      unit: kW
      source:
        kind: measurement
        instance_ref: site_load
        point_ref: active_power
    - name: temp_avg
      unit: Cel
      source:
        kind: covariate
        dataset_ref: weather.observed
        field: air_temperature
    - name: humidity
      unit: "%"
      source:
        kind: covariate
        dataset_ref: weather.observed
        field: relative_humidity
    - name: rain
      unit: mm
      source:
        kind: covariate
        dataset_ref: weather.observed
        field: precipitation
    - name: quarter_hour
      unit: "1"
      source:
        kind: calendar
        transform: quarter_hour_of_day_zero_based

  future_covariates:
    - name: temp_avg
      unit: Cel
      source:
        kind: covariate
        dataset_ref: weather.nwp
        field: air_temperature
    - name: humidity
      unit: "%"
      source:
        kind: covariate
        dataset_ref: weather.nwp
        field: relative_humidity
    - name: rain
      unit: mm
      source:
        kind: covariate
        dataset_ref: weather.nwp
        field: precipitation
    - name: quarter_hour
      unit: "1"
      source:
        kind: calendar
        transform: quarter_hour_of_day_zero_based

alignment:
  aggregation: mean
  timestamp_semantics: interval_end
  duplicate_policy: latest
  feature_policies:
    - { name: load, aggregation: mean, duplicate_policy: latest }
    - { name: temp_avg, aggregation: mean, duplicate_policy: latest }
    - { name: humidity, aggregation: mean, duplicate_policy: latest }
    - { name: rain, aggregation: sum, duplicate_policy: latest }
    - { name: quarter_hour, aggregation: last, duplicate_policy: reject }
  missing_policy: reject

quality:
  max_input_age_seconds: 900
  max_gap_seconds: 1800
  max_missing_ratio: 0.0
  require_input_watermark: true
  require_covariate_issue_time_not_after_as_of: true

output:
  unit: kW
  sign_convention: positive_consumption
  max_quantiles: 0
  expires_after_seconds: 3600
```

### What belongs in the task

The declaration should contain only portable domain semantics:

- a stable task ID, revision, and typed task kind;
- semantic instance and point references rather than channel IDs, SHM slots,
  SQL names, or vendor register addresses;
- canonical units and sign conventions;
- historical and known-future features;
- cadence, window, alignment, and missing-data rules;
- the expected derived-data shape and freshness limit; and
- the processor contract the task requires.

Site commissioning resolves `instance_ref` and `point_ref` to the actual
instances and routes. Pack validation must fail closed if a required reference
cannot be resolved or its physical unit, scale, offset, point kind, or target
sign convention does not exactly match the commissioned task. The current v1
runtime validates those facts; it does not perform engineering-unit or sign
conversion.

The declaration must not contain:

- a processor URL, process name, API token, or TLS key;
- a concrete history database query;
- a SHM path or layout assumption;
- an ONNX/RKNN input node name; or
- a device action to execute from the result.

Those details belong to composition, an adapter, or a separate control use
case.

## Implement a `DataProcessor`

The implemented port is intentionally narrow. In Rust-like pseudocode:

```rust
#[async_trait]
pub trait DataProcessor: Send + Sync {
    fn descriptor(&self) -> &DataProcessorDescriptor;

    async fn health(&self) -> PortResult<ProcessorHealth>;

    async fn process(
        &self,
        request: DataProcessingRequest,
    ) -> PortResult<ProcessingResult>;
}
```

The request and result are typed contracts documented in
[Data Processing Contracts](../reference/data-processing-contracts.md). A
processor descriptor declares supported contract versions, task kinds, the
local/remote data boundary, and finite frame/request limits. It must not expose
a generic vendor command set or an unvalidated `run(json)` escape hatch.

A processor is responsible for:

- rejecting unsupported contract versions, task kinds, features, and shapes;
- applying the feature order and normalization owned by the selected model;
- executing its deterministic algorithm or model runtime;
- returning the actual model version and artifact digest;
- distinguishing a normal result, an explicit fallback, and no usable result;
  and
- honoring deadlines and bounded resource limits.

A processor must not:

- query Aether SHM, SQLite, the history service, or a domain-pack directory;
- accept only a `plant_id` and then discover the corresponding data itself;
- infer units or power direction from a site name;
- publish derived values as ordinary live measurements;
- dispatch a device command; or
- turn a processing failure into an apparently normal zero-valued result.

For model-backed processing, keep model internals on the processor side. The
processor may own feature order, scaler statistics, sequence length, model
artifacts, ONNX/RKNN selection, and de-normalization. Aether sends named,
unit-bearing observations instead of model tensors.

## Configure local and remote processors

Only a composition root chooses a concrete processor. The strict runtime YAML
contains the full task, binding, history route, covariate source, and processor
descriptor. This abbreviated fragment shows only the processor portion; do not
use it as a complete configuration. `HttpDataProcessorConfig` receives a
validated endpoint and derives the fixed `/v1/process` and `/v1/health` routes:

```yaml
processor:
  endpoint: http://127.0.0.1:8989/
  id: example-forecast-processor
  version: 0.1.0
  contract: aether.data-processing.forecast.v1
  requires_artifact: true
  boundary: local
  max_frame_cells: 5000
  max_request_bytes: 4194304
  connect_timeout_ms: 500
  request_timeout_ms: 4500
  max_response_bytes: 4194304
  bearer_token_env: AETHER_PROCESSOR_BEARER_TOKEN
```

The generic domain/processor contract supports static features, and custom
in-process compositions can bind them through `DataProcessingBinding`. The
current `aether-api` runtime YAML loader has no static-value binding field, so a
runtime-configured v1 route must not declare static features. Add loader support
and tests before commissioning one; do not assume the wire schema alone makes
it available.

Local and remote adapters use the same request/result contract. A remote route
adds an explicit data-egress boundary and must be preapproved by both task and
route policy. Secrets stay in environment-owned secret injection. The current
configuration has no custom CA-file setting; HTTPS uses the HTTP client's
configured trust roots. A deployment needing private trust material must
provide it through a supported transport composition rather than inventing a
YAML key.

The default Aether distribution must not require either processor. With no
route configured, the task is unavailable and the rest of acquisition,
history, alarms, rules, and device control continue independently.

## How Aether assembles a request

`DataProcessingApplication` owns the following steps for each request, with
task-specific transforms supplied by the commissioned assembly composition:

1. Authorize the advertised processing capability and load the task revision.
2. Resolve semantic source references through commissioned instance mappings.
3. Query the required historical range through `HistoryQuery`.
4. Read the latest values through the read-only `LiveState` port when the task
   allows a live tail and that feature uses `aggregation: last`, then replace
   only its final interval cell without changing SHM authority. Version 1
   rejects live tail for `mean`, `sum`, `min`, or `max` because an instantaneous
   value cannot represent an aggregate bucket. The load/PV tasks forbid it.
5. Obtain future-known inputs through a typed `CovariateSource`.
6. Generate deterministic calendar features locally.
7. Verify exact commissioned unit/sign metadata, align timestamps, aggregate
   raw observations, resolve duplicates, and apply the declared missing-data
   policy. Version 1 performs no runtime unit/sign conversion.
8. Calculate frame quality and a canonical input digest.
9. Select the configured processor and submit one complete `ProcessingFrame`.
10. Validate the returned task ID, request ID, input digest, timestamps, units,
    status, model provenance, and expiry before exposing `DerivedData`.

For interval-end cadence `c`, the historical labels are
`as_of-(history_steps-1)c, ..., as_of`. A label `t` aggregates raw observations
in `(t-c, t]`, and no source read may advance beyond `as_of`. Future covariates
and forecast output start at `as_of+c`. The default runtime implements this
against `aether-history.db` with the read-only SQLite adapter; all feature reads
for a request share one transaction. The optional HTTP history adapter is only
for an upstream service that already materializes the exact cadence grid and
supports the restricted `last/reject` policy.

Metadata checks do not prove interval meaning. Before enabling a physical
route, use a site golden fixture to verify every raw source's aggregation and
alignment semantics. This is mandatory for `rain`: `aggregation: sum` is valid
only when each source value is incremental precipitation for its interval, not
a rolling accumulation or rate.

Nor does current SQLite history provide a point-in-time backtest cut. Rows
have no ingestion timestamp or source/configuration epoch, so late backfills
and physical remaps behind an unchanged logical series can alter or splice an
old event-time window. Binding revisions validate the current route only.
Offline evaluations need a frozen historian export captured at the evaluation
cut, or a bitemporal, epoch-bearing history adapter. Freeze the artifact
registry too: v1 artifact identity has version and digest but no
`trained_through` or `available_at`, so an old `as_of` alone cannot exclude a
model released later.

Treat historian storage changes as maintenance. The `history_config.storage_*`
rows are saved intent, and `PUT /hisApi/storage` does not reconnect the active
writer. Disable processing, apply a reconnect or history-service restart,
verify the active SQLite backend plus a commissioned sentinel series, and only
then restart `aether-api` against the same path.

Back this with filesystem isolation: give the API independently permissioned
read-only access to the historian database/WAL/SHM directory, separate from
its writable configuration/audit database. The base Compose `/app/data:rw`
mount and SQLite read-only flags alone are not a production authority boundary.

The processor gets no Aether database credentials and no reverse callback URL.
If it needs another observation, the task declaration is incomplete and must
be revised rather than allowing an undeclared read.

## Invoke the application API

Version 1 is exposed by the opt-in, JWT-protected `aether-api` routes:

- `GET /api/v1/data-processing/tasks`;
- `GET /api/v1/data-processing/processors/health`; and
- `POST /api/v1/data-processing/process`.

Viewer, Engineer, and Admin roles may use discovery; processing requires
Engineer or Admin. The process body is the strict application request, not a
complete frame or processor endpoint. `x-request-id` is optional and
`x-aether-confirmed: true` supplies explicit confirmation when route policy
requires it. CLI and MCP bindings for these capabilities are not implemented
in v1; future transports must call the same application API rather than the
sidecar.

## Request a processor directly during development

The optional HTTP adapter maps `DataProcessor::process` to the versioned
processor-facing `POST /v1/process` endpoint. The downstream AetherEMS
Load-Forecasting processor implements that endpoint; it is a processor boundary,
not an application-facing endpoint on the default Aether services.

```bash
curl --fail-with-body \
  --request POST http://127.0.0.1:8989/v1/process \
  --header 'Content-Type: application/vnd.aether.data-processing+json;version=1' \
  --data @processing-request.json
```

Direct calls are useful for processor conformance tests. Production callers
should enter through `DataProcessingApplication` so source resolution, data
quality, policy, and result validation cannot be bypassed.

## Retry and content identity

`data_processing.process` is a read-only query but is deliberately declared
`idempotent: false`. Version 1 has no request replay store, de-duplication
contract, exact-result guarantee, or `409 REQUEST_ID_REUSED` behavior. Each
accepted invocation receives its own audit record and may execute the processor
again.

`input_digest` identifies the exact task/binding revision, frame, artifact
selector, contract, and options. It supports correlation and offline comparison;
it is not an idempotency key for the public operation. A caller may retry a
typed retryable `429`, `503`, or `504` response only under its own bounded
policy, with a fresh deadline and awareness that work may already have run.

## Conformance checklist

Before routing a task to a processor, verify that it:

- accepts a complete frame and makes no reverse data read;
- validates every contract invariant in the reference page;
- rejects NaN, infinity, timestamp disorder, unit mismatch, and undeclared
  features;
- can reproduce a pinned frame in an offline golden test where the algorithm is
  deterministic, without treating that test property as an API replay promise;
- does not call a current historian plus current model selection a
  point-in-time backtest; frozen history and artifact cuts, or equivalent
  bitemporal/availability metadata, are required;
- handles timeout, disconnect, overload, and malformed responses as typed
  failures;
- labels fallback output and never disguises unavailable data as a prediction;
- returns model and processor provenance without local filesystem paths or
  secrets;
- passes task-specific fixtures for normal, missing, stale, and boundary data;
  and
- proves that processor loss cannot block acquisition or deterministic safety
  behavior.

The current SQLite history schema does not retain device-origin sample quality,
and the current SHM bridge labels accepted finite live values as `good`.
Freshness, gaps, missingness, numeric constraints, issue time, and provenance
are enforced, but a deployment requiring end-to-end source-quality fidelity
must commission a quality-bearing source adapter before production.

## Related pages

- [Data Processing Contracts](../reference/data-processing-contracts.md) — v1 wire contracts and validation rules
- [HTTP Data Processor](../../services/api/adapters/http-data-processor/README.md) — bounded local/remote adapter and composition API
- [AetherEMS Power Forecasting](https://github.com/EvanL1/AetherEMS/blob/main/packs/energy/knowledge/power-forecasting.md) — the first downstream task and processor
- [Load-Forecasting Processor](https://github.com/EvanL1/AetherEMS/tree/main/processors/load-forecasting) — downstream energy-domain `/v1/process` implementation
- [JSON Schemas](../../contracts/data-processing/README.md) — strict v1 transport validation
- [Data Flow](../concepts/data-flow.md) — authoritative live and historical paths
- [System Architecture](../concepts/architecture.md) — core layers and service boundaries
- [Safe Operations for Applications and Agents](safe-operations.md) — why derived data does not bypass control policy
