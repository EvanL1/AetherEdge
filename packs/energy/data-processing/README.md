# Energy data-processing assets

The two task declarations under `tasks/` are formal Pack assets.
`tasks/index.yaml` is the sole task inventory: its IDs must match
`pack.yaml` `capabilities.data_processing_tasks` and the actual regular files
exactly. The generic kernel exposes those task identities only when this Energy
Pack is explicitly active; activation still leaves both declarations disabled.

This directory lands load and photovoltaic forecasting on the industry-neutral
Aether Data Processing contract. The normative task and binding assets are safe
by default:

- both task declarations have `enabled: false`;
- the example site binding has `enabled: false` and `commissioned: false`;
- no processor endpoint, credential, database query, channel address, SHM
  path, or model filesystem path is stored in those task/binding assets; and
- installing the energy pack does not start a processor or send site data.

`runtime.example.yaml` is different: it is a deployment-owned, synthetic
commissioning template and intentionally contains an enabled route, physical
history mappings, a loopback endpoint, and a non-deployable artifact digest.
It MUST be copied outside the pack, replaced with site-specific values, and
validated against the site's Aether database before use. It is not safe to run
unchanged and does not alter the disabled defaults above.

The task and binding files describe data semantics. A composition root
separately selects a `DataProcessor` implementation and supplies any endpoint
or secret from deployment-owned configuration.

The site binding therefore contains no processor reference, route, data
boundary, or artifact selection. Deployment configuration binds an enabled,
commissioned task binding to a processor route as a separate composition step.

## Data path

The application request and processor request are deliberately different:

```text
ProcessTaskRequest
  task_id + expected revision + binding_id + as_of + typed options
                         │
                         ▼
             DataProcessingApplication
        task + commissioned semantic binding
        HistoryQuery + optional Last-only LiveState tail
        CovariateSource + calendar transforms
        unit/sign contract checks + alignment + quality policy
                         │
                         ▼
       DataProcessingRequest with complete ProcessingFrame
                         │
                         ▼
                    DataProcessor
                         │
                         ▼
           ProcessingResult (untrusted output)
                         │ Aether validates and stamps
                         ▼
                  DerivedData
```

An application caller cannot supply a frame, endpoint, processor identity, or
artifact selector. Aether resolves those values from a commissioned task and
binding. Conversely, the processor receives the complete frame in one request
and MUST NOT call back into Aether, query SHM or history, inspect this pack, or
use a site identifier to discover more data.

`ProcessingResult.output` is untrusted. It is not a live measurement and MUST
NOT be written into IO-owned T/S shared memory. Aether creates `DerivedData`
only after correlation, digest, task, binding, feature, unit, timestamp,
horizon, finite-value, status, provenance, and expiry validation succeeds.

Version 1 uses interval-end timestamps. For cadence `c`, a history label `t`
aggregates raw observations in `(t-c, t]`; the history grid ends at `as_of`,
and future covariates and forecast points begin at `as_of+c`. The default
runtime queries the existing `aether-history` SQLite file through the read-only
`SqliteHistoryQuery` adapter. All feature reads for one request share one
SQLite transaction/snapshot. The optional HTTP history adapter accepts only an
already materialized cadence grid with `aggregation: last` and
`duplicate_policy: reject`; it is not a raw-history aggregation service.

An instantaneous live value cannot replace a `Mean`, `Sum`, `Min`, or `Max`
bucket. The load and PV target histories use `Mean`, so both task declarations
forbid live tail and the runtime template sets `live_tail: false`. LiveState
replacement remains available only to other features/tasks commissioned with
`Last`.

The current runtime validates that commissioned physical unit, scale, offset,
point kind, and target sign metadata exactly match the task. It does not perform
runtime engineering-unit or sign conversion. Commission a normalized source
or add and test an explicit transform before enabling a route that needs
conversion.

Version 1 also does not preserve device-origin sample quality end to end: the
current SQLite history schema stores numeric observations without their device
quality, and the current SHM bridge labels accepted finite live values as
`good`. Freshness, gaps, missingness, numeric ranges, provenance, and issue-time
rules are enforced, but deployments that require source quality fidelity must
add a quality-bearing adapter before production commissioning.

The same history schema is not bitemporal and stores no source/binding epoch.
`as_of` excludes later event timestamps, but it cannot exclude a row backfilled
later with an old timestamp or separate data collected before and after a
physical remap hidden behind one logical series. A task/binding revision only
guards current commissioning. Use a frozen history export for an offline
energy backtest or add ingestion-time and source-epoch filtering. Likewise,
artifact version/digest identifies a selected model but carries no
`trained_through` or `available_at`; freeze the artifact set at the evaluation
cut before claiming historical model results are leakage-safe.

The task `execution.correlation_key: input_digest` names content identity for
audit and comparison. `data_processing.process` is non-idempotent and provides
no exact replay, de-duplication, cache, or `409` request-reuse contract. Both
tasks set `max_attempts: 1`; any caller retry is a new bounded invocation.

## Task inventory

| Task | Cadence | Historical inputs | Future inputs | Output |
|------|---------|-------------------|---------------|--------|
| `energy.site-load-forecast` | 15 minutes | `load`, `temp_avg`, `humidity`, `rain`, `quarter_hour` | weather plus `quarter_hour` | import-positive load in kW |
| `energy.site-pv-forecast` | 30 minutes | historical `pv` plus all 19 weather fields | all 19 weather fields; never future `pv` | generation-positive PV power in kW |

The PV declaration contains the complete 19-field weather contract used by
the compatibility processor. A model artifact with an input dimension of 20
also consumes the historical `pv` target as its twentieth feature. A
19-dimensional artifact may use the weather fields only. Feature ordering,
scaling, tensors, ONNX/RKNN execution, and inverse transforms stay inside the
processor; the Aether frame remains named and unit-bearing.

A model is only one optional artifact kind. The task can also be implemented
by a deterministic algorithm. Selecting an artifact in a request never
activates, downloads, or changes it.

The generic task therefore keeps `artifact_policy.required: false`, while the
commissioned Load-Forecasting runtime route sets `requires_artifact: true` and
requires a digest-pinned bundle. These are different layers, not conflicting
policies.

The generic contract also supports static features, but the current
`aether-api` runtime YAML loader cannot bind static values. The shipped load/PV
tasks declare none; a site must not add one until loader support and composition
tests exist.

## Files

- `tasks/site-load-forecast.yaml` declares the five-feature load task.
- `tasks/site-pv-forecast.yaml` declares all 19 weather fields and historical
  PV, covering both 19- and 20-dimensional processor artifacts.
- `bindings/example-site.yaml` shows semantic site commissioning without
  physical channel or endpoint details.
- `runtime.example.yaml` is the enabled synthetic deployment template described
  above; its processor ID matches the adapter default `load-forecasting-edge`.
- `covariates.example.json` is a synthetic snapshot input, not a weather feed.
- `fixtures/load-process-task-request.json` is the minimal application-facing
  `ProcessTaskRequest`.
- `fixtures/load-processing-request.json` is the processor-facing request with
  a complete frame.
- `fixtures/load-processing-result.json` is untrusted processor output with
  `status: produced`.
- `fixtures/load-derived-data.json` is the corresponding Aether-validated and
  stamped value.

All identifiers, values, UUIDs, and digests in the fixtures are synthetic.
The artifact digest identifies the literal fixture artifact label
`aether-example-site-load-model-v3`; it is not a deployable model.

## Status and fallback rules

A processor result has exactly one of these statuses:

- `produced`: usable output from the declared processor and optional artifact;
- `fallback`: usable output from a task-approved named strategy based on real
  request data; or
- `unavailable`: no policy-compliant output, with no `output` or `expires_at`.

Technical failure MUST NOT be converted into an ordinary all-zero forecast.
Zero is a valid physical value, so a zero-valued fallback is accepted only if
a task explicitly defines that strategy and the result remains labeled
`fallback`. These tasks permit only persistence from actual frame data and
forbid a synthetic zero baseline.

## Commissioning

Before changing either safe default, a composition root must:

1. resolve every symbolic instance and point reference to an existing,
   exactly unit/scale/offset-compatible Aether point, with the target sign
   convention explicitly matching the commissioned source;
2. bind observed weather to commissioned history routes, and configure the
   future NWP `CovariateSource` with valid-time and issue-time provenance;
3. prove the task cadence, feature meanings, ranges, sign convention, history
   window, and horizon against processor conformance fixtures, and use site
   golden rows to prove each physical source's interval meaning,
   especially that `rain` is per-cadence accumulation rather than a rolling
   total or rate—the runtime does not infer or convert this;
4. select a processor route outside this pack, keep remote egress denied unless
   separately approved, and configure bounded payloads and deadlines;
5. validate explicit `produced`, `fallback`, and `unavailable` behavior;
6. freeze history and artifact cuts for any offline backtest; do not treat
   event-time `as_of`, current binding revision, or an artifact digest as a
   substitute for ingestion/source epochs and model availability metadata;
7. after any historian storage change, keep processing disabled until
   `aether-history` has reconnected or restarted, its active SQLite backend and
   a commissioned sentinel series are verified, and `aether-api` restarts on
   the same path—persisted `history_config.storage_*` is only saved intent;
8. expose the historian database/WAL/SHM directory to `aether-api` through an
   independently permissioned read-only mount or ACL, separate from the API's
   writable configuration/audit database; the base Compose `/app/data:rw`
   mount does not satisfy this production gate;
9. set `commissioned: true`, then enable only the intended task binding; and
10. enable the task declaration only after the binding and processor health
   checks pass.

For the Load-Forecasting compatibility processor, health is not sufficient for
production. The pinned upstream future-covariate off-by-one must be fixed with
a golden step-to-row test, verbose sensitive output removed, actual artifact
files resolved and pinned, upstream licensing cleared, and a real artifact
benchmarked below the frame-and-processor work deadline at target-hardware p95. See the
[`integrations/load-forecasting` readiness gates](../../../integrations/load-forecasting/README.md#production-cutover-blockers).

Processor loss or network failure must leave acquisition, SHM, history,
alarms, deterministic rules, and device control available.

## Local syntax validation

From the repository root, parse every YAML and JSON file without contacting
any external service:

```bash
ruby -e 'require "yaml"; Dir["packs/energy/data-processing/**/*.yaml"].sort.each { |p| YAML.safe_load(File.read(p), [], [], false, p); puts p }'
ruby -e 'require "json"; Dir["packs/energy/data-processing/**/*.json"].sort.each { |p| JSON.parse(File.read(p)); puts p }'
```

The normative wire rules are documented in
[`docs/reference/data-processing-contracts.md`](../../../docs/reference/data-processing-contracts.md).
