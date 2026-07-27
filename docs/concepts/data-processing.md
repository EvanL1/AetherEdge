---
title: Aether Data Processing
description: Industry-neutral, request-driven processing of governed IoT data into validated derived data
updated: 2026-07-11
---

# Aether Data Processing

Aether Data Processing is the industry-neutral application capability for
turning governed IoT observations into validated derived data. It assembles a
bounded input frame from Aether-owned data sources, invokes a local or remote
processor, validates the response, and makes the result available through one
transport-neutral application boundary. Version 1 exposes authenticated HTTP
routes in `aether-api`; CLI and MCP bindings are not implemented yet. Any
future transport, scheduler, or automation integration must use the same
application boundary.

Forecasting is the first data-processing task. A load-forecast model endpoint
is not a second data platform and does not own the site's data path: it is one
request-driven `DataProcessor` that receives a complete `ProcessingFrame` and
returns a `ProcessingResult`.

This implemented kernel capability does not make data processing a required
seventh process, nor does it require a model, cloud connection, or external
database in the default distribution. A composition root may run the
application in an existing process or isolate it in an optional, independently
supervised process.

## Vocabulary

| Concept | Name |
|---------|------|
| Architecture capability | Aether Data Processing |
| Application use case | `DataProcessingApplication` |
| Configured unit of work | `DataProcessingTask` |
| Processing implementation port | `DataProcessor` |
| Governed, time-aligned input | `ProcessingFrame` |
| Untrusted processor response | `ProcessingResult` |
| Validated Aether result | `DerivedData` |

The module is called **Data Processing** rather than Forecasting because future
industry packs may need windowed estimation, feature derivation, scoring, or
classification as well as forecasts. It is not called Analytics because it is
not a reporting or BI surface, and it is not named after a model technology:
deterministic algorithms and model endpoints implement the same processor
contract.

## What belongs here

A data-processing task has all of these properties:

- it starts from already decoded, semantically identified Aether data;
- it operates on a bounded frame and explicit `as_of` time;
- its required fields, units, time policy, quality limits, and output schema
  are declared before execution;
- it is invoked through a request/response application use case;
- it produces derived data with input, processor, quality, and expiry
  provenance;
- it has no direct device side effects.

Examples include a 24-hour site-load forecast, a remaining-useful-life
estimate over a vibration window, a comfort score from building measurements,
or a quality classification for a completed production cycle. The first
standardized task is `Forecast`; later task kinds require their own typed input
and output contracts rather than being hidden inside a universal JSON API.

## Core invariants

Every implementation must preserve these properties:

1. **Data requests the processor.** `DataProcessingApplication` assembles and
   sends the complete input. A processor does not call back into Aether to
   discover a site, read SHM, open the history database, or fetch arbitrary
   points.
2. **Data authority does not move.** SHM remains authoritative for current
   live point state, the history capability remains authoritative for stored
   observations, and configured covariate sources own their data. Processing
   only creates `DerivedData`.
3. **Inputs are task-scoped.** A task and commissioned binding determine which
   fields may be read. A caller or processor cannot turn one processing request
   into an unrestricted site-data query.
4. **Processors are computational adapters.** They may own algorithm code,
   model artifacts, feature order, scalers, tensor construction, or a remote
   API client. They do not own Aether point bindings, live-state writes, alarm
   lifecycle, planning, or device control.
5. **Failure is explicit.** Missing inputs, stale data, processor errors,
   invalid output, and approved fallbacks remain distinguishable. A technical
   failure must never be represented as a normal value such as zero.
6. **Derived data is not measured state.** Results retain their input digest,
   processor provenance, quality, and expiry. They are never written into
   IO-owned T/S slots or silently promoted to live-state authority.
7. **The hot path is independent.** Acquisition, deterministic protection,
   alarms, and essential control continue when a processor or AI client is
   unavailable.
8. **The standalone profile stays standalone.** Redis, PostgreSQL, a cloud
   processor and model service remain optional downstream components, not startup
   dependencies of the Aether kernel.

## Architecture boundary

```
authenticated HTTP / in-process application work
                    │
                    ▼
       DataProcessingApplication
        ├─ task + commissioned binding
        ├─ HistoryQuery
        ├─ read-only LiveState
        ├─ CovariateSource
        └─ frame and quality policy
                    │
                    ▼
            ProcessingFrame
                    │ complete request
                    ▼
             DataProcessor
          local algorithm / model
           sidecar / remote API
                    │
                    ▼
        untrusted ProcessingResult
                    │ validate and stamp
                    ▼
               DerivedData
        └─ v1 direct query response
```

The capability is a vertical slice through Aether's existing layers:

- The domain layer owns stable value types for task identifiers, time ranges,
  units, data quality, result status, processor provenance, and `DerivedData`.
- Ports describe narrow capabilities such as `HistoryQuery`,
  `CovariateSource`, and `DataProcessor`. They contain no SQL, HTTP, vendor,
  protocol, or tensor-runtime vocabulary.
- The application layer resolves task bindings, assembles and validates a
  `ProcessingFrame`, invokes a processor, validates its result, and applies
  authorization, egress, deadline, and audit policy.
- Extensions adapt those ports to a local algorithm, an HTTP model sidecar, a
  remote service, weather data, or an optional derived-data store.
- Industry packs declare task semantics and feature roles. Site commissioning
  binds those roles to concrete instances and points.
- Composition roots choose concrete adapters and decide whether processing
  needs process isolation. Core crates never depend on an industry pack or
  downstream processor.

See [Data Processing Flow](data-processing-flow.md) for the detailed assembly,
request, validation, and failure sequence, including which optional result
paths remain future work.

### Implemented history and time semantics

The current production composition uses a task-scoped, read-only
`SqliteHistoryQuery` over the existing `aether-history.db`. It never creates or
migrates the historian schema. All feature reads for one processing request
share one SQLite transaction/snapshot. For interval-end cadence `c`, a label
`t` aggregates raw observations in `(t-c, t]`; the history grid ends at
`as_of`, and future covariates and forecast output begin at `as_of+c`.

In production, read-only SQLite connection flags must be backed by OS
permissions: expose the historian database/WAL/SHM directory to the API through
an independent read-only mount or identity, separate from the API's writable
configuration/audit store. The base Compose-wide `/app/data:rw` mount does not
meet this direct-history commissioning gate.

The optional HTTP history adapter is narrower: it accepts only an upstream
already aligned to the exact cadence grid with `aggregation=last` and
`duplicate_policy=reject`. It does not aggregate raw history.

Here `as_of` is an event-time upper bound, not a bitemporal database cut. The
current SQLite rows have no ingestion/system timestamp and no source,
binding, or configuration epoch. One transaction makes an invocation
internally consistent, but a later backfill at an older `time_ms` can change a
replay and a remap behind the same logical series can splice two physical
sources. Current task/binding revisions do not repair missing row epochs.
Rigorous historical evaluation therefore requires a frozen historian snapshot
captured at the evaluation cut, or a future `HistoryQuery` implementation that
filters both event time and ingestion/source epoch.

The artifact selector likewise records identity, version, and digest, but not
`trained_through` or `available_at`. A pinned digest makes an execution
identifiable; it does not prove that the artifact existed at a historical
`as_of`. Until those availability/training cuts are added, Data Processing can
assemble bounded online frames and deterministic goldens, but it does not by
itself provide leakage-safe historical model backtesting.

### Current quality limitation

The domain and wire contracts carry sample quality, but the current production
sources do not preserve device-origin quality end to end. The history schema
stores numeric observations without device quality, and the SHM bridge marks
accepted finite live values as `good`. Version 1 enforces freshness, gaps,
missingness, numeric constraints, provenance, and covariate issue time. A site
that requires original device quality must commission a quality-bearing source
adapter before enabling processing.

## The task, binding, and processor split

A `DataProcessingTask` describes a stable operational question. For a forecast
it includes the target semantics, observation and future-covariate fields,
  lookback, horizon, resolution, engineering units, timezone and sign contract,
freshness limits, missing-data policy, and typed output contract. It does not
contain a channel address, SHM slot, SQL query, tensor node, processor URL, or
secret.

An industry pack supplies that semantic task definition. Site commissioning
binds its fields to concrete Aether instances and points. A `DataProcessor`
separately owns the algorithm-specific representation. This separation allows
a device to move to another channel without rebuilding a processor, and a
model version to change without teaching Aether about tensor layouts.

For example, an energy pack can declare a `site-load-forecast` task whose
target is active import power. A commissioned site binds `load` to an instance
measurement and `temperature` to a weather measurement. A load-forecast HTTP
processor then maps those named columns into the feature order and scaling
required by its model. The model endpoint receives the data in the request; it
does not receive a `plant_id` and query a second time-series database.

The conceptual processor contract is:

```
DataProcessingRequest {
  request context + deadline,
  task and output contract,
  processor selection policy,
  ProcessingFrame,
  input digest
}
          │
          ▼
DataProcessor.process(...)
          │
          ▼
ProcessingResult {
  request and input identity,
  processor/artifact provenance,
  status + quality + expiry,
  task-typed processor output
}
          │ validate and Aether-stamp
          ▼
DerivedData {
  result and acceptance identity,
  validated task output + provenance
}
```

The frame carries named, typed fields instead of a processor-specific tensor
or unstructured payload. The untrusted result envelope provides correlation,
provenance, fallback status, and expiry. Aether validates its typed output and
then creates `DerivedData`. This keeps the data path inspectable and testable,
and lets a pinned frame be reused in offline conformance or golden tests without
coupling the kernel to one algorithm runtime. It does not promise API replay.

## Forecast is the first task

Forecasting exercises the complete cross-industry processing path:

- a historical observation window;
- an optional current SHM tail only for features aggregated with `Last`;
- future or known covariates such as weather, calendar, tariff, or schedule;
- unit/sign compatibility, timezone, alignment, and resampling policy;
- a multi-step output with explicit horizon and expiry;
- optional quantiles or confidence bounds;
- content identity based on the exact assembled frame and governed context,
  without implying that mutable sources can reconstruct it from `as_of` alone.

A forecast result contains ordered future timestamps, target semantics, units,
and one or more values or quantiles. The common envelope also identifies the
task, input digest, processor and artifact version, issue time, expiry, quality,
and whether an approved fallback was used.

The same `Forecast` contract can support different packs:

| Industry pack | Example forecast | Typical observations and covariates |
|---------------|------------------|--------------------------------------|
| Energy | Site load, PV generation, or demand | Power history, weather, calendar, tariff |
| Manufacturing | Production demand or remaining useful life | Cycle count, vibration, temperature, schedule |
| Buildings | Cooling load or occupancy | Zone sensors, weather, calendar, booking schedule |
| Agriculture | Irrigation demand or soil moisture | Soil sensors, weather forecast, crop schedule |
| Transport | Traffic flow or arrival time | Counts, speed, route schedule, weather |

Additional processing task kinds may follow, but each must define typed
semantics and conformance tests. A generic `process(json)` escape hatch must
not become the public application contract.

## Boundary with neighboring subsystems

The word "processing" is intentionally broad, so its limits must be explicit.

| Neighbor | Its responsibility | Data Processing boundary |
|----------|--------------------|--------------------------|
| IO decoding | Read protocol frames, validate wire values, apply channel-point decoding, and publish canonical T/S values | Starts only after a value has an Aether point identity and engineering meaning; never decodes Modbus registers, IEC messages, or raw frames |
| SHM dataplane | Authoritative current point values and the existing command transport | Read-only input through `LiveState`; no derived result or processor writes IO-owned T/S slots |
| History | Sample, persist, retain, aggregate, and query observations | The application reads `HistoryQuery`; the default adapter opens the existing history SQLite file read-only, while processors never receive database access |
| Rule engine | Deterministic event/tick evaluation and action execution | Does not replace rules or synchronously call a network processor in a rule hot path |
| Alarm service | Alarm policy, lifecycle, severity, acknowledgement, and notification | May produce a score or detection as `DerivedData`; it does not create or acknowledge an alarm by itself |
| Bulk ETL / BI | Large scans, arbitrary joins, warehouses, dashboards, and offline reporting | Handles bounded operational frames for declared tasks, not general data pipelines or analytical warehouses |
| Planning / optimization | Turn observations and derived data into a proposed operating plan | Stops at validated derived data; it does not choose or schedule device actions |
| Control | Authorize, confirm, audit, route, and dispatch device commands | A later caller must enter `ControlApplication`; processor output grants no control permission |

The current v1 composition requires commissioned physical unit, scale, offset,
and target sign metadata to match the task exactly. It performs alignment and
aggregation but no runtime unit/sign conversion. A future explicit transform
would create a task-specific view without changing the authoritative
measurement and would require its own tests and provenance.

## Data authority

| Data | Authority | Processing treatment |
|------|-----------|----------------------|
| Current T/S point value | IO-owned SHM | Read through `LiveState`; never written by processing |
| Historical point observations | `HistoryQuery` implementation | Read through a port; processors never open the store directly |
| External or future covariates | Configured `CovariateSource` | Copied with event-time and issue-time provenance |
| Task and point binding | Aether configuration after sync | Resolved before the processor request |
| Algorithm/model artifact and private transforms | Selected `DataProcessor` | Returned as processor and artifact provenance |
| Processing output | Validated `DerivedData` | Derived, quality-bounded, expiring, and returned directly by the v1 API |
| Device action | Existing control application and dispatcher | Created only by a separate decision and control use case |

`ProcessingResult` must identify the request, task and binding revision, input
digest, processor and artifact version, issue time, expiry, status, and quality.
Consumers reject unsupported contract versions, non-finite values, mismatched
shapes or units, timestamps outside the task range, expired results, and
results that cannot be tied to the submitted frame.

## AI-native governance

Data Processing is AI-native because task contracts, allowed inputs, quality,
processor provenance, permissions, egress, and failure states are
machine-readable—not because an LLM sits in the data path. The v1 authenticated
HTTP surface invokes `DataProcessingApplication`; future CLI, MCP, and scheduled
interfaces must invoke the same use cases.

A side-effect-free processing invocation is a Medium-risk query because its
configured route may cross a remote data-egress boundary; deployment policy
decides confirmation for that approved route, durable audit is required, and
the capability is marked non-idempotent because each call may execute processor
work again.
Low-risk task and processor-health discovery requires no confirmation or
durable audit because it carries no observation values. Changing a task,
route, artifact selection, egress policy, or fallback policy is a separate
configuration command with its own permission, confirmation, and audit
metadata. A processing result never inherits permission to operate a device.

Remote processors create an explicit data-egress boundary. The application
must allowlist the processor, minimize the request to task-declared fields,
enforce payload and deadline limits, avoid unrelated identifiers and secrets,
and retain enough provenance to explain what data cut and processor produced a
result.

## Non-goals

Aether Data Processing is not:

- protocol decoding, acquisition, SHM replication, or a second live-state
  plane;
- a time-series historian, feature store, data lake, ETL engine, BI platform,
  or arbitrary query service;
- a training pipeline, experiment tracker, or mandatory model registry;
- a replacement for deterministic rules, alarm lifecycle, planning,
  optimization, or `ControlApplication`;
- permission for a processor to discover arbitrary site data or query Aether
  databases;
- a generic tensor transport or untyped plugin API;
- a requirement that every deployment run a data-processing process;
- part of acquisition or hard real-time safety loops.

## Related pages

- [Data Processing Flow](data-processing-flow.md) — frame assembly, processor request, validation, caching, and failures
- [System Architecture](architecture.md) — core services and dependency boundaries
- [Data Flow](data-flow.md) — authoritative uplink and control paths
- [Shared Memory](shared-memory.md) — live-state authority and writer ownership
- [Rule Engine](rule-engine.md) — deterministic automation behavior
- [Safe Operations for Applications and Agents](../guides/safe-operations.md) — permission, confirmation, audit, and control policy
