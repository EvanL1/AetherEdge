---
title: Power Forecasting
description: Map AetherEMS load and PV data into the request-driven Load-Forecasting processor without creating a second data plane
updated: 2026-07-11
---

# Power Forecasting

Power forecasting is the first implemented task family for **Aether Data
Processing**. AetherEMS owns the observations and their energy semantics; a
`DataProcessor` receives a complete `ProcessingFrame` and returns an untrusted
`ProcessingResult`; after validation Aether stamps the forecast as
`DerivedData`.

The first compatibility target is the Edge-Platform in
[`panskai/Load-Forecasting`](https://github.com/panskai/Load-Forecasting/tree/4956ec33cdaa1191e8db8d4aabbb581fa8602d10/Edge-Platform),
inspected at commit `4956ec33`. It already contains load and PV predictors,
ONNX/RKNN engines, model/scaler management, and a FastAPI boundary. The
repository integration wraps only the load predictor in v1 while replacing its
reverse data reads with the Aether Data Processing contract. The PV task and
mapping are documented but remain disabled until their separate processor path
passes the same gates.

The repository now contains the core ports and application flow, strict v1
codec and schemas, bounded HTTP adapter, disabled load/PV task assets, a local
energy-gateway composition proof, and the opt-in Load-Forecasting processor.
The bundled binding remains disabled and uncommissioned; PV model routing,
real-site bindings, and shadow cutover are deployment work, not default-runtime
dependencies.

## Target data path

```text
device
  │
  ▼
aether-io ──────────────► SHM (current measured state authority)
                              │
                              ├────────► aether-history
                              │             (stored history authority)
                              │
authenticated HTTP caller     │
          │                   │
          ▼                   │
 DataProcessingApplication ◄──┘
   ├─ energy-pack task and semantic point bindings
   ├─ HistoryQuery
   ├─ read-only LiveState tail
   ├─ weather CovariateSource
   └─ alignment, unit/sign contract checks, and quality policy
                    │
                    ▼
              ProcessingFrame
                    │ HTTP or in-process request
                    ▼
       Load-Forecasting DataProcessor
          ├─ model feature order
          ├─ scaler and tensor construction
          ├─ ONNX/RKNN execution
          └─ de-normalization
                    │
                    ▼
       ProcessingResult (untrusted)
                    │ validate and stamp
                    ▼
             forecast DerivedData
                    │
          ▼
 authenticated HTTP response
          │ future separate, governed planning use case
          ▼
 economic optimizer → ControlApplication → device
```

The important direction is **data requests the processor**. The processor does
not receive a `plant_id` and then come back to Aether for data.

## Data authority

| Data | Authority | Forecasting use |
|------|-----------|-----------------|
| Current T/S point state | aether-io-owned SHM | Available only to tasks/features using `Last`; disabled for mean-aggregated load/PV targets |
| Historical measurements | aether-history through `HistoryQuery` | Historical target and observed covariates |
| Future weather/NWP | Configured `CovariateSource` | Known-future model features |
| Task semantics and point bindings | AetherEMS energy pack plus site commissioning | Source resolution, units, sign, cadence, and quality policy |
| Model artifacts, feature order, scaler, tensors | Selected `DataProcessor` | Model execution only |
| Forecast output | Aether-stamped `DerivedData` after validating `ProcessingResult` | Query or optimization input; never live state authority |
| Device commands | `ControlApplication` and the existing downlink | Not a processor responsibility |

Forecasts MUST NOT be written into the existing T/S SHM as if they were
measurements. They are time-indexed, model-derived values with an input
watermark, model provenance, and expiry. Optional result caching or persistence
can be added behind a derived-data port later; it is not required for the
request-driven path.

The production composition reads raw history from the existing
`aether-history.db` through a lazy read-only `SqliteHistoryQuery`; all features
for one request share one SQLite transaction. The optional HTTP history adapter
is only for an upstream pre-aligned `last/reject` grid. Neither path gives the
forecast processor database access.

For direct SQLite reads, application-level read-only mode is not sufficient
production isolation. The API principal must receive the historian
database/WAL/SHM directory through an independent read-only mount or ACL while
its own configuration/audit database remains separately writable. The current
base Compose read-write `/app/data` mount does not satisfy that gate.

The v1 quality envelope is richer than the current source storage. History rows
do not carry original device quality, and the SHM bridge synthesizes `good` for
accepted finite values. Forecast commissioning can enforce freshness, gaps,
missingness, ranges, issue time, and provenance, but a site requiring original
quality fidelity must add a quality-bearing adapter before cutover.

Current history and artifact metadata also do not prove a historical
point-in-time cut. SQLite rows contain event time but no ingestion time or
source/binding epoch; later backfills and physical remaps behind one logical
series can change or splice an old window. Artifact provenance pins version
and digest but has no `trained_through` or `available_at`, so a later model can
be run against an earlier `as_of`. Use frozen historian and artifact snapshots
for shadow/backtest evidence, or add bitemporal source epochs and artifact
availability cuts before calling an evaluation leakage-safe.

## What the current Edge-Platform does

The current
[`app.py`](https://github.com/panskai/Load-Forecasting/blob/4956ec33cdaa1191e8db8d4aabbb581fa8602d10/Edge-Platform/app.py)
exposes `/predict` and `/forecast`. A request supplies `plant_id`,
`forecast_type`, `horizon`, and `model_version`; the service creates its own
preprocessing and model-execution services.

The current preprocessing path is coupled to InfluxDB3:

- [`preprocess_service.py`](https://github.com/panskai/Load-Forecasting/blob/4956ec33cdaa1191e8db8d4aabbb581fa8602d10/Edge-Platform/data_processing/preprocess_service.py)
  calls the optimized InfluxDB path and explicitly rejects filesystem fallback.
- [`influxdb_reader.py`](https://github.com/panskai/Load-Forecasting/blob/4956ec33cdaa1191e8db8d4aabbb581fa8602d10/Edge-Platform/data_processing/influxdb_reader.py)
  queries `load_power`, `pv_power`, and `weather_history`, plus `nwp_weather`
  in the separate `forecast_nwp_cache` database.
- [`edge_preprocess_influxdb.py`](https://github.com/panskai/Load-Forecasting/blob/4956ec33cdaa1191e8db8d4aabbb581fa8602d10/Edge-Platform/data_processing/edge_preprocess_influxdb.py)
  joins those rows and passes in-memory `history` and `forecast` lists to the
  model layer.

That ownership is appropriate for a standalone forecasting platform, but not
for an Aether processor. It would make InfluxDB a required second source of
device truth and would duplicate Aether's point mapping, history, and data
quality logic.

The reusable part begins after data assembly:

- [`inference_service.py`](https://github.com/panskai/Load-Forecasting/blob/4956ec33cdaa1191e8db8d4aabbb581fa8602d10/Edge-Platform/inference/inference_service.py)
  resolves model files and scaler statistics, selects a load or PV predictor,
  and can fall back from RKNN to ONNX.
- [`load_predictor.py`](https://github.com/panskai/Load-Forecasting/blob/4956ec33cdaa1191e8db8d4aabbb581fa8602d10/Edge-Platform/inference/predictors/load_predictor.py)
  owns load feature order and autoregressive model execution.
- [`pv_predictor.py`](https://github.com/panskai/Load-Forecasting/blob/4956ec33cdaa1191e8db8d4aabbb581fa8602d10/Edge-Platform/inference/predictors/pv_predictor.py)
  owns PV feature order and four-input encoder/decoder construction.
- [`model_manager.py`](https://github.com/panskai/Load-Forecasting/blob/4956ec33cdaa1191e8db8d4aabbb581fa8602d10/Edge-Platform/inference/models/model_manager.py)
  manages model configuration, scaler statistics, and ONNX/RKNN artifacts.

The legacy source class may retain its current `InferenceService` name during
migration. Its Aether-facing role is the generic `DataProcessor` port; the
repository's compatibility adapter is `LoadForecastProcessor`.

## Load forecast task

The current `LoadPredictor` consumes these features in this exact order:

```text
load, temp_avg, humidity, rain, quarter_hour
```

Their Aether sources and ownership are:

| Processor feature | Aether source | Assembly rule | Owner of model transform |
|-------------------|---------------|---------------|--------------------------|
| `load` history | Site load active-power semantic point through `HistoryQuery` | Require the commissioned source already to use task unit `kW` and `positive_consumption`; aggregate raw rows with `Mean`; live tail is forbidden | Processor places it in feature position 0 and applies scaler statistics |
| `temp_avg` history | Observed-weather `CovariateSource`, or a commissioned weather-station measurement queried from Aether history | Require the declared temperature unit; aggregate by task rule | Processor places it in feature position 1 |
| `humidity` history | Observed weather or a commissioned measurement | Validate declared relative-humidity unit and range | Processor places it in feature position 2 |
| `rain` history | Observed weather or a commissioned measurement | Sum only after a site golden fixture proves the source is accumulation over cadence, not a rolling total or rate | Processor places it in feature position 3 |
| `temp_avg`, `humidity`, `rain` future | NWP `CovariateSource` | Select the model run valid at `as_of`; align valid times to forecast timestamps | Processor uses them during autoregressive steps |
| `quarter_hour` history/future | Deterministic calendar transform from each UTC timestamp | `hour * 4 + minute / 15`, with the task declaring whether indexing is zero- or one-based | Processor places it in feature position 4 |

The task declaration, model manifest, and conformance fixture MUST agree on
the `quarter_hour` convention. The current source uses the named field but does
not by itself provide a cross-system semantic guarantee.

The runtime validates exact physical unit, scale, offset, point kind, and
target sign metadata; it does not perform engineering-unit/sign conversion or
prove interval semantics. Load history uses interval-end labels: a 15-minute
label `t` aggregates raw rows in `(t-15m, t]`, the history grid ends at
`as_of`, and the first future row is `as_of+15m`.

An instantaneous SHM value cannot replace a mean-aggregated load bucket. The
load task and runtime route therefore set `live_tail: forbidden/false`; recent
data arrives only after the historian has persisted enough raw samples to form
the final interval.

An energy-pack declaration should use semantic references rather than an
Influx measurement:

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
  live_tail: forbidden
inputs:
  history:
    - {name: load, source: {kind: measurement, instance_ref: site_load, point_ref: active_power}}
    - {name: temp_avg, source: {kind: covariate, dataset_ref: weather.observed, field: air_temperature}}
    - {name: humidity, source: {kind: covariate, dataset_ref: weather.observed, field: relative_humidity}}
    - {name: rain, source: {kind: covariate, dataset_ref: weather.observed, field: precipitation}}
    - {name: quarter_hour, source: {kind: calendar, transform: quarter_hour_of_day_zero_based}}
  future_covariates:
    - {name: temp_avg, source: {kind: covariate, dataset_ref: weather.nwp, field: air_temperature}}
    - {name: humidity, source: {kind: covariate, dataset_ref: weather.nwp, field: relative_humidity}}
    - {name: rain, source: {kind: covariate, dataset_ref: weather.nwp, field: precipitation}}
    - {name: quarter_hour, source: {kind: calendar, transform: quarter_hour_of_day_zero_based}}
```

The production declaration must also include the history length, horizon,
alignment, missing-data, and freshness fields described in
[Connect Data Processors](../guides/data-processors.md).

## PV forecast task

The current `PVPredictor` declares 19 weather features:

1. `DHI`
2. `DNI`
3. `GHI`
4. `Clearsky DHI`
5. `Clearsky DNI`
6. `Clearsky GHI`
7. `Cloud Type`
8. `Dew Point`
9. `Solar Zenith Angle`
10. `Fill Flag`
11. `Surface Albedo`
12. `Wind Speed`
13. `Precipitable`
14. `Wind Direction`
15. `Relative Humidity`
16. `Temperature`
17. `Pressure`
18. `Global Horizontal UV Irradiance 280-440`
19. `Global Horizontal UV Irradiance 295-385`

Models whose `input_dim` is 20 append historical `pv` as the twentieth
feature. The processor currently constructs four model inputs—encoder values,
encoder time marks, decoder values, and decoder time marks—and fills the future
PV part of the decoder internally. Aether must send future weather timestamps
and features, not future target values and not model tensors.

| Data | Aether source | Boundary rule |
|------|---------------|---------------|
| Historical `pv` | Commissioned PV generation active-power point via `HistoryQuery` | Require the source already to match the task unit and `positive_generation`; aggregate with `Mean`; live tail is forbidden |
| Historical weather | Observed-weather covariate adapter or commissioned weather measurements | Map canonical weather fields and validate freshness/ranges; current adapters do not retain device-origin quality |
| Future weather | NWP `CovariateSource` | Select by forecast issue time and valid time; never join a run published after `as_of` |
| Calendar/time marks | Selected `DataProcessor` from supplied timestamps | Aether supplies UTC timestamps; processor builds the model-specific 4D/5D marks |
| Missing weather | Aether task policy | Do not let the processor silently turn missing weather into zero unless an explicit, tested substitution declares that meaning |

The current Influx reader extracts only a subset of aliases such as `t2m`,
`rh`, `ws10`, `wd10`, `sp`, `ghi`, `dhi`, and `dni` on some paths. Before
cutover, a conformance fixture must prove the complete mapping into all 19
features required by each deployed model. A field's presence in JSON is not
enough; unit, accumulation period, and valid-time semantics must match its
training data.

An abbreviated task declaration could begin:

```yaml
schema: aether.data-processing-task.v1
id: energy.site-pv-forecast
revision: 1
kind: forecast
processor_contract: aether.data-processing.forecast.v1
target:
  name: pv
  semantic_point: site.pv.active_power
  unit: kW
  sign_convention: positive_generation
frame:
  cadence_seconds: 1800
  live_tail: forbidden
inputs:
  history:
    - {name: pv, source: {kind: measurement, instance_ref: site_pv, point_ref: active_power}}
    - {name: GHI, source: {kind: covariate, dataset_ref: weather.observed, field: global_horizontal_irradiance}}
    # Declare every additional weather feature required by the model manifest.
  future_covariates:
    - {name: GHI, source: {kind: covariate, dataset_ref: weather.nwp, field: global_horizontal_irradiance}}
    # Declare the same complete future-weather set.
```

Do not treat this abbreviated example as a usable PV task. Pack validation must
reject it until every required model feature and its unit is declared.

## Cadence and horizon compatibility

The current baseline generator encodes the following compatibility defaults:

| Task | Cadence | Ultra-short | Short-term | Medium-term |
|------|---------|-------------|------------|-------------|
| Load | 15 minutes | 16 points / 4 hours | 288 points / 72 hours | 960 points / 240 hours |
| PV | 30 minutes | 8 points / 4 hours | 144 points / 72 hours | 480 points / 240 hours |

These values come from the current fallback implementation; they are not a
universal Aether rule. Each migrated model's `config.json`, training contract,
and actual forecast timestamps must be checked. A task revision pins the
validated cadence, history length, label length where relevant, and maximum
horizon. A request outside that manifest is rejected rather than truncated.

## Processor-facing request

The repository's compatibility package implements `POST /v1/process` with a
complete `aether.data-processing.request.v1` envelope. That route is additive:
it is not present in the pinned upstream commit, and it does not reuse the
legacy identifier-only request path.

The adapter converts named frame series into the two in-memory structures the
current predictors already accept:

```text
frame.history            → history_data: List[Dict]
frame.future_covariates  → forecast_data: List[Dict]
```

It delegates to the bounded processor wrapper and legacy model execution path.
The wrapper may be long-lived, but the upstream model-loading and serial
autoregressive cost is not assumed away and remains subject to the p95 gate.
The conversion layer must not instantiate an Influx reader, open an Aether
database, inspect SHM, or derive a site mapping from `plant_id`.

The boundary of responsibility is:

| AetherEMS | Load-Forecasting `DataProcessor` |
|-----------|-----------------------|
| Resolve commissioned semantic points | Validate the selected model family/version |
| Read and aggregate stored history; load/PV live tail is disabled | Apply model feature order |
| Obtain observed and forecast weather | Load scaler statistics |
| Align timestamps and remove duplicates | Build ONNX/RKNN tensor shapes |
| Reject mismatched physical units/sign metadata; no v1 conversion | Execute the model engine |
| Apply declared missing/staleness policy | De-normalize outputs |
| Calculate input quality and digest | Return actual model/artifact provenance |
| Validate result horizon, unit, expiry, and status | Label an approved fallback or unavailable result |

Model artifacts may still be synchronized or loaded by the processor. They are
processor implementation assets, not Aether observations. Weather snapshots,
by contrast, become a `CovariateSource` because they are request data and must
be assembled under the same task policy as measurements.

## Result mapping

The current successful response contains `plant_id`, `target_type`, `horizon`,
`run_time`, `lead_hours`, `time_resolution`, `model_version`, and a list of
`{ts, value}` predictions. The v1 adapter maps that into
`aether.data-processing.result.v1`:

- task ID and revision replace `plant_id` as the source of semantics;
- `target_type` maps to the typed forecast target;
- timestamps and values map to `ProcessingResult.output.points`, which become
  `DerivedData.data.points` after Aether validation;
- `time_resolution` maps to validated `cadence_seconds`;
- the actual model version is accompanied by an artifact digest;
- request ID, input digest, input watermark, issue time, and expiry are added;
  and
- status is explicitly `produced`, `fallback`, or `unavailable`.

The current
[`fallback_engine.py`](https://github.com/panskai/Load-Forecasting/blob/4956ec33cdaa1191e8db8d4aabbb581fa8602d10/Edge-Platform/inference/engines/fallback_engine.py)
generates zero values even for its placeholder persistence and
historical-average branches in `BaselineForecastGenerator`. Those branches
must not enter AetherEMS as ordinary successful forecasts. Migration requires
one of these outcomes:

- implement the named strategy using actual frame observations and return
  `status: fallback`;
- keep a zero baseline only where the task explicitly approves that physical
  meaning, still labeled `fallback`; or
- return `status: unavailable` with no derived data.

## Staged migration

### 1. Freeze the compatibility baseline

The source commit and repository fixtures establish the code baseline. A real
deployment must additionally pin its exact artifacts and close the following
operational items:

- Pin the source commit and deployed model artifacts.
- For historical evaluation, freeze both the historian export and artifact
  registry at the evaluation cut. Current `as_of`, binding revision, and
  artifact digest do not exclude late history rows, remapped source epochs, or
  a model published after that cut.
- Create fixtures containing the exact historical rows, future-weather rows,
  `as_of`, model selector, and expected output for load and PV.
- Fix the pinned load predictor's `forecast_sorted[step+1]` indexing. It skips
  the first future-covariate row. A golden test MUST prove forecast step zero
  uses `as_of+cadence` and step `n` uses `as_of+(n+1)cadence` before cutover.
- Record cadence, sequence length, feature order, units, sign conventions,
  and timezone assumptions for every deployed model.
- Remove legacy stdout/stderr prints of model paths, scaler data, normalized or
  real predictions, and next-step feature vectors. Redacted HTTP envelopes do
  not make raw process output safe.
- Implement the deployment-owned artifact resolver with the same `ModelManager`
  selection used by execution, and pin every actual model/scaler/config file
  for the duration of a call.
- Benchmark a real commissioned artifact on target hardware. The frame-and-processor work deadline and
  concurrency may be enabled only after measured p95 completes within the
  configured budget; the pinned legacy model-loading/autoregressive path has no
  acceptable p95 evidence yet.
- Establish a license for code that will be copied or distributed with
  AetherEMS; the inspected repository does not contain a license file.
- Rotate the repository-tracked Influx credential before migration and replace
  it with external secret references. Do not copy it into AetherEMS history.

### 2. Request-driven compatibility endpoint (implemented)

- The opt-in integration supplies a strict `ProcessingFrame` parser and typed
  forecast request/result adapter around the legacy `InferenceService`.
- Feature ordering, model manager, scalers, predictors, and ONNX/RKNN execution
  remain processor-owned.
- The adapter bounds processor execution and keeps its wrapper objects alive;
  the legacy model-loading and autoregressive execution cost still requires the
  target-hardware p95 gate above.
- The model selector identifies an already approved artifact; a processing
  request never activates or downloads one as a side effect.
- Upstream `/predict` and `/forecast` routes may remain for old non-Aether
  clients during migration. AetherEMS uses only `/v1/process`.

### 3. Aether-owned assembly (implemented baseline)

- The energy pack defines disabled load and PV `DataProcessingTask` revisions
  plus an uncommissioned example binding and contract fixtures.
- `HistoryQuery` and `CovariateSource` expose bounded logical data to the
  application. The default history extension owns a read-only SQLite handle;
  the application contract and processor see only the port.
- Production commissioning binds observed weather through `HistoryQuery` and
  future NWP through `CovariateSource`; the zero-service example uses
  deterministic memory data.
- `DataProcessingApplication` validates the assembled stored-history frame.
  Live tail remains available only to other tasks/features using `Last`; the
  mean-aggregated load/PV targets forbid it.
- `HttpDataProcessor` sends the complete request through the same
  `DataProcessor` port used by local implementations.

### 4. Run shadow comparison

For identical `as_of`, source rows, model artifacts, and horizon:

- run the legacy Influx-owned path and the request-driven path;
- compare aligned input rows before comparing model output;
- measure maximum absolute difference, WAPE or nMAE as appropriate, horizon
  coverage, input completeness, and processing latency;
- exercise DST boundaries even though the contract uses UTC, missing NWP
  fields, stale weather, rejection of live tail for aggregate buckets,
  processor timeout, and RKNN-to-ONNX
  engine fallback; and
- retain request/result digests so a mismatch is reproducible.

"Identical source rows and model artifacts" is an explicit fixture condition,
not something the current live historian can reconstruct for an old `as_of`.
Capture immutable inputs before comparison; do not query today's mutable
history and artifact registry and label the result point-in-time.

Cutover requires agreed numerical tolerances and zero unexplained row-alignment
differences, not merely similar plots.

### 5. Remove reverse data reads

After both task conformance suites and shadow criteria pass:

- stop routing AetherEMS requests through `PreprocessService` and
  `EdgePreprocessInfluxDB`;
- remove InfluxDB credentials and client dependencies from the processor image
  used by AetherEMS;
- move weather/NWP synchronization behind the configured Aether covariate
  adapter;
- retain model-artifact synchronization only as an optional processor concern;
  and
- remove the legacy endpoints when no deployed client calls them and the
  rollback window has closed.

These are the removal criteria for the compatibility shim. Until then, the two
paths must be clearly named and never silently fall through from one to the
other.

## Automatic control boundary

Forecasting is outside hard real-time and safety loops. A future economic
optimizer may request a forecast through the same application API, validate the
result, and produce a proposed dispatch plan. Any device action still enters
`ControlApplication`, with its own permission, confirmation, deadline, audit,
offline gate, and command validation.

If processing times out, returns `unavailable`, expires, or fails quality
policy:

- the economic planning cycle skips or uses only a separately approved
  fallback;
- stale setpoints are not replayed when the processor returns;
- SOC, over-current, temperature, breaker, and equipment protection behavior
  continues from current measured state; and
- acquisition, history, alarms, and deterministic rules remain independent.

Neither processor output nor accepted `DerivedData` may contain a hidden
device command.

## AI-native surface

AI-native operation means the task is discoverable and explainable, not that an
LLM sits in the numerical path. The authenticated v1 HTTP/application API
exposes:

- task revision, target semantics, cadence, and horizon;
- local versus remote data boundary;
- input watermark, gaps, substitutions, and missing ratio;
- processor and model artifact provenance;
- normal, fallback, or unavailable status;
- expiry and whether Aether accepted or rejected the processor result.

Model-card and evaluation-summary discovery are not implemented in v1 and must
not be inferred from the task endpoint.

The processing capability is read-only derived computation, but each process
call is non-idempotent and durably audited. Model activation is a separate
configuration command, and dispatch is a separate high-risk
control command. Version 1 exposes authenticated HTTP and the application API;
CLI and MCP bindings remain future work. Any such future client must not bypass
`DataProcessingApplication` by calling the model sidecar directly in normal
operation.

## Production commissioning and cutover criteria

The repository implementation is safe-disabled by default. A site may enable
the commissioned load route only when:

- the enabled task declaration resolves entirely through semantic bindings;
- no processor code or container can read Aether SHM, history
  storage, site SQLite, or InfluxDB;
- local and remote processor adapters pass the same request/result conformance
  suite;
- load fixtures prove all five feature semantics and order;
- the load future-covariate off-by-one is fixed and its step-to-row golden test
  passes;
- site golden fixtures prove each raw source's interval meaning, especially
  that `rain` is cadence accumulation rather than a rolling total or rate;
- any backtest or historical shadow comparison uses a historian snapshot with
  a frozen source epoch and an artifact set frozen at the evaluation cut;
- historian storage changes occur with processing disabled; `aether-history`
  is reconnected or restarted, its active SQLite backend and a commissioned
  sentinel series are verified, and `aether-api` restarts on the same path
  because persisted `history_config.storage_*` alone describes saved intent;
- the API's historian database/WAL/SHM directory is separately mounted or
  permissioned read-only; the base Compose-wide read-write `/app/data` mount is
  not accepted as the production boundary;
- authenticated actor/IP rate limits and an in-flight ceiling protect the
  non-idempotent process endpoint, while `command_audit_events` has monitored
  capacity and an evidence-preserving retention/export policy;
- legacy verbose output is removed, actual artifact files are pinned by the
  execution resolver, upstream licensing permits the deployment, and a real
  artifact meets the target-hardware p95 deadline;
- result validation rejects wrong horizons, units, signs, timestamps, digests,
  expired data, and unlabeled fallback;
- processor loss does not affect the default Aether runtime or deterministic
  safety behavior;
- no external database is required by the AetherEMS default distribution; and
- any control based on forecast output still passes through the existing
  application control boundary and audit policy.

PV remains disabled until its complete 19/20-dimensional mapping, units,
valid-time semantics, artifact behavior, and equivalent processor/golden tests
pass. The presence of its task YAML is not production readiness.

## Related pages

- [Connect Data Processors](../guides/data-processors.md) — task declarations and processor adapters
- [Data Processing Contracts](../reference/data-processing-contracts.md) — complete request/result and failure semantics
- [Energy Data Processing Assets](../../packs/energy/data-processing/README.md) — disabled load/PV tasks, binding, and conformance fixtures
- [Load-Forecasting Processor](../../integrations/load-forecasting/README.md) — implemented request-driven compatibility endpoint
- [HTTP Data Processor](../../extensions/http-data-processor/README.md) — bounded Rust transport adapter
- [Data Flow](../concepts/data-flow.md) — current SHM, history, and command paths
- [Control Strategies](control-strategies.md) — deterministic energy control behavior
- [Safe Operations for AI Agents](safe-operations.md) — permission and confirmation boundaries
