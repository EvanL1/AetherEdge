---
title: Power Forecasting
description: Map AetherEMS load and PV data into the request-driven Load-Forecasting processor without creating a second data plane
updated: 2026-08-14
---

# Power Forecasting

Power forecasting is the first implemented task family for **Aether Data
Processing**. AetherEMS owns the observations and their energy semantics; a
`DataProcessor` receives a complete `ProcessingFrame` and returns an untrusted
`ProcessingResult`; after validation Aether stamps the forecast as
`DerivedData`.

The downstream
[`Load-Forecasting` processor](https://github.com/EvanL1/AetherEMS/tree/main/processors/load-forecasting)
owns load-model execution, ONNX/RKNN engines, model/scaler management, and its
HTTP boundary. Aether supplies the complete input frame through the sole
`POST /process` operation. The PV task and mapping remain disabled until their
separate processor path passes the same gates.

The repository now contains the core ports and application flow, strict
codec and schemas, bounded HTTP adapter, disabled load/PV task assets, a local
energy-gateway composition proof, and the opt-in Load-Forecasting processor.
The bundled binding remains disabled and uncommissioned; PV model routing,
real-site bindings, and site validation are deployment work, not default-runtime
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
accepts only a pre-aligned `last/reject` grid. Neither path gives the
forecast processor database access.

For direct SQLite reads, application-level read-only mode is not sufficient
production isolation. The API principal must receive the historian
database/WAL/SHM directory through an independent read-only mount or ACL while
its own configuration/audit database remains separately writable. The current
base Compose read-write `/app/data` mount does not satisfy that gate.

The quality envelope is richer than the current source storage. History rows
do not carry original device quality, and the SHM bridge synthesizes `good` for
accepted finite values. Forecast commissioning can enforce freshness, gaps,
missingness, ranges, issue time, and provenance, but a site requiring original
quality fidelity must add a quality-bearing adapter before enablement.

Current history and artifact metadata also do not prove a historical
point-in-time cut. SQLite rows contain event time but no ingestion time or
source/binding epoch; later backfills and physical remaps behind one logical
series can change or splice an old window. Artifact provenance pins version
and digest but has no `trained_through` or `available_at`, so a later model can
be run against an earlier `as_of`. Use frozen historian and artifact snapshots
for shadow/backtest evidence, or add bitemporal source epochs and artifact
availability cuts before calling an evaluation leakage-safe.

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
schema: aether.data-processing-task
id: energy.site-load-forecast
revision: 1
kind: forecast
processor_contract: aether.data-processing.forecast
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
[Connect Data Processors](../../../docs/guides/data-processors.md).

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

The commissioned binding and conformance fixture must prove the complete
mapping into all 19 features required by each deployed model. A field's
presence in JSON is not enough; unit, accumulation period, and valid-time
semantics must match its training data.

An abbreviated task declaration could begin:

```yaml
schema: aether.data-processing-task
id: energy.site-pv-forecast
revision: 1
kind: forecast
processor_contract: aether.data-processing.forecast
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

## Cadence and horizon policy

The disabled Pack task declarations pin these execution limits:

| Task | Cadence | History | Horizon |
|------|---------|---------|---------|
| Load | 15 minutes | 672 points | 288 points / 72 hours |
| PV | 30 minutes | 128 points | 144 points / 72 hours |

These values are Pack task policy, not a universal Aether rule. Commissioning
must prove that the selected model artifact uses the declared cadence, history
length, and horizon. A request outside the task declaration is rejected rather
than truncated.

## Processor-facing request

The processor exposes one operation: `POST /process` with a complete
`aether.data-processing.request` envelope.

The adapter converts named frame series into the two in-memory structures the
current predictors already accept:

```text
frame.history            → history_data: List[Dict]
frame.future_covariates  → forecast_data: List[Dict]
```

It delegates to the bounded model execution engine. The wrapper may be
long-lived, but model-loading and serial autoregressive cost remain subject to
the p95 gate.
The conversion layer must not instantiate an Influx reader, open an Aether
database, inspect SHM, or derive a site mapping from `plant_id`.

The boundary of responsibility is:

| AetherEMS | Load-Forecasting `DataProcessor` |
|-----------|-----------------------|
| Resolve commissioned semantic points | Validate the selected model artifact |
| Read and aggregate stored history; load/PV live tail is disabled | Apply model feature order |
| Obtain observed and forecast weather | Load scaler statistics |
| Align timestamps and remove duplicates | Build ONNX/RKNN tensor shapes |
| Reject mismatched physical units/sign metadata; no runtime conversion | Execute the model engine |
| Apply declared missing/staleness policy | De-normalize outputs |
| Calculate input quality and digest | Return actual model/artifact provenance |
| Validate result horizon, unit, expiry, and status | Label an approved fallback or unavailable result |

Model artifacts may still be synchronized or loaded by the processor. They are
processor implementation assets, not Aether observations. Weather snapshots,
by contrast, become a `CovariateSource` because they are request data and must
be assembled under the same task policy as measurements.

## Result contract

The processor returns exactly `aether.data-processing.result`. It echoes the
request, task, binding, and input identities; reports issue time, expiry,
processor identity, and model artifact digest; and places timestamped forecast
points in `output.points`. Aether validates the target, unit, sign, cadence,
horizon, timestamps, digest, and expiry before stamping `DerivedData`.

Result status is exactly `produced`, `fallback`, or `unavailable`. A synthetic
zero series must not enter AetherEMS as an ordinary successful forecast. The
processor must choose one of these outcomes:

- implement the named strategy using actual frame observations and return
  `status: fallback`;
- keep a zero baseline only where the task explicitly approves that physical
  meaning, still labeled `fallback`; or
- return `status: unavailable` with no derived data.

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
LLM sits in the numerical path. The authenticated HTTP/application API
exposes:

- task revision, target semantics, cadence, and horizon;
- local versus remote data boundary;
- input watermark, gaps, substitutions, and missing ratio;
- processor and model artifact provenance;
- normal, fallback, or unavailable status;
- expiry and whether Aether accepted or rejected the processor result.

Model-card and evaluation-summary discovery are not implemented and must
not be inferred from the task endpoint.

The processing capability is read-only derived computation, but each process
call is non-idempotent and durably audited. Model activation is a separate
configuration command, and dispatch is a separate high-risk
control command. The runtime exposes authenticated HTTP and the application API;
CLI and MCP bindings remain future work. Any such future client must not bypass
`DataProcessingApplication` by calling the model sidecar directly in normal
operation.

## Production commissioning criteria

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
- processor output is redacted, actual artifact files are pinned by the
  execution resolver, all processor and model licenses permit deployment, and
  a real artifact meets the target-hardware p95 deadline;
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

- [Connect Data Processors](../../../docs/guides/data-processors.md) — task declarations and processor adapters
- [Data Processing Contracts](../../../docs/reference/data-processing-contracts.md) — complete request/result and failure semantics
- [Energy Data Processing Assets](../data-processing/README.md) — disabled load/PV tasks, binding, and conformance fixtures
- [Load-Forecasting Processor](https://github.com/EvanL1/AetherEMS/tree/main/processors/load-forecasting) — downstream processor implementation
- [HTTP Data Processor](../../../services/api/adapters/http-data-processor/README.md) — bounded Rust transport adapter
- [Data Flow](../../../docs/concepts/data-flow.md) — current SHM, history, and command paths
- [Control Strategies](control-strategies.md) — deterministic energy control behavior
- [Safe Operations for AI Agents](safe-operations.md) — permission and confirmation boundaries
