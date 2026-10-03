# Aether Data Processing JSON Schemas

This directory publishes the Draft 2020-12 transport schemas for version 1 of
the Aether Data Processing contract family.

| Schema | Boundary |
|---|---|
| `process-task-request.v1.schema.json` | Application caller to `DataProcessingApplication` |
| `processing-frame.v1.schema.json` | Complete input frame assembled by Aether |
| `data-processing-request.v1.schema.json` | Aether to a selected `DataProcessor` |
| `forecast-output.v1.schema.json` | Typed interval-end forecast output |
| `processing-result.v1.schema.json` | Untrusted processor response |
| `derived-data.v1.schema.json` | Aether-validated and stamped output |
| `error.v1.schema.json` | Typed non-2xx processor or transport failure |

The normative prose is
[`docs/reference/data-processing-contracts.md`](../../docs/reference/data-processing-contracts.md).
These schemas are strict wire guards, not a second domain model.

## Authority and trust boundary

Rust domain types, constructors, and validation remain authoritative. JSON
Schema cannot prove arbitrary-array length equality, value/quality null
coupling, strict timestamp order, cadence, time ordering across fields,
provenance target existence, canonical digest equality, correlation with the
originating request, commissioned task policy, unit agreement, or quantile
ordering. The Rust domain and application layers must enforce those invariants
after schema validation.

They also enforce a one-to-one mapping between every populated
`(segment, feature)` key and its provenance record. Calendar-derived and
constant features still require explicit `calendar` or `constant` provenance;
aggregate `input_watermark` tracks actual observations rather than being
advanced by those deterministic sources.

Version 1 timestamps use the RFC 3339 UTC `Z` form with no finer than
millisecond resolution because the authoritative Rust value is `TimestampMs`.
A provenance entry may include `issued_at` for a versioned external forecast;
Rust enforces `issued_at <= watermark <= frame.as_of`.

Forecast grids use interval-end semantics. For cadence `c`, history ends at
`as_of` and a label `t` represents raw interval `(t-c, t]`; future covariates
and forecast output begin at `as_of+c`. Live tail can replace only a
`Last`-aggregated final cell, never a `Mean`, `Sum`, `Min`, or `Max` bucket.
The current energy load/PV tasks therefore disable it. Their non-calendar
future covariates also require `issued_at`, even though the generic wire field
is optional for task kinds that do not impose that policy.

The schemas can carry per-sample quality, but the current SQLite history schema
does not store device quality and the current SHM bridge labels accepted finite
values as `good`. That implementation limitation is not evidence of original
device-quality fidelity.

Nor do these schemas make a live historical query point-in-time safe. The
current history rows have neither ingestion time nor source/binding epoch, so
`as_of` is an event-time bound and cannot exclude later backfills or separate
physical-source epochs. Artifact objects identify version/digest but carry no
`trained_through` or `available_at`. Leakage-safe historical evaluation needs
frozen history and artifact cuts, or future contracts/adapters that validate
those missing chronology fields.

For the embedded adapter, schema validation also does not replace deployment
permissions: production gives the API independently read-only access to the
historian database/WAL/SHM directory, not the base shared writable data mount.

In particular, validating a `DataProcessingRequest` does not authorize an
external caller to submit one to Aether. The landed authenticated HTTP surface
sends only `ProcessTaskRequest`; future CLI or MCP bindings must do the same.
Its schema deliberately rejects `frame`,
`processor`, `processor_id`, `processor_contract`, `endpoint`, and `artifact`.
`DataProcessingApplication` resolves the commissioned binding and route,
assembles the complete frame, and then creates the processor-facing request.
Using a schema directly must never bypass that application boundary.

`input_digest` is content identity for correlation and audit, not operation
identity. `data_processing.process` is non-idempotent and version 1 provides no
request replay, de-duplication, cache, or request-ID reuse conflict guarantee.

`binding` is mandatory on application, processor, result, and derived-data
contracts. A generic `artifact` selector or provenance object is optional at
the processor boundary, but it is never legal in the application request.

`ProcessingResult` uses disjoint `oneOf` branches:

- `produced` requires `output` and `expires_at`, and forbids `fallback` and
  `unavailable`;
- `fallback` requires `output`, `expires_at`, and fallback provenance, and
  forbids `unavailable`; and
- `unavailable` requires retry guidance and forbids `output`, `expires_at`,
  and `fallback`.

For `unavailable`, `retryable` is always required. `retry_after_seconds` is
optional when `retryable` is true and forbidden when it is false, matching the
Rust `UnavailableInfo` contract.

Every fixed object uses `additionalProperties: false`. The only open-name
objects are the explicitly dynamic `features` and `static_features` maps in
`ProcessingFrame`; their values remain schema constrained. Error `details`
also uses a closed set of diagnostic fields. None is a generic command or
callback channel.

Keep this contract family together when copying it because the processor
request references the frame schema and derived data reuses result definitions.

## Validation

No repository dependency is required. From the repository root, use the
ephemeral [`check-jsonschema`](https://github.com/python-jsonschema/check-jsonschema)
tool through `uv`:

```bash
uvx --from check-jsonschema check-jsonschema \
  --check-metaschema contracts/data-processing/*.schema.json

SCHEMA_BASE="file://$(pwd)/contracts/data-processing/"

jq '{task_id: .task.id, expected_task_revision: .task.revision,
     binding_id: .binding.id, expected_binding_revision: .binding.revision,
     as_of: .frame.as_of, options: .options}' \
  crates/aether-data-processing/tests/fixtures/forecast-processing-request.json | \
  uvx --from check-jsonschema check-jsonschema \
    --schemafile contracts/data-processing/process-task-request.v1.schema.json -

uvx --from check-jsonschema check-jsonschema --base-uri "$SCHEMA_BASE" \
  --schemafile contracts/data-processing/data-processing-request.v1.schema.json \
  crates/aether-data-processing/tests/fixtures/forecast-processing-request.json

uvx --from check-jsonschema check-jsonschema --base-uri "$SCHEMA_BASE" \
  --schemafile contracts/data-processing/processing-result.v1.schema.json \
  crates/aether-data-processing/tests/fixtures/forecast-processing-result.json

uvx --from check-jsonschema check-jsonschema --base-uri "$SCHEMA_BASE" \
  --schemafile contracts/data-processing/derived-data.v1.schema.json \
  crates/aether-data-processing/tests/fixtures/forecast-derived-data.json

jq '.frame' crates/aether-data-processing/tests/fixtures/forecast-processing-request.json | \
  uvx --from check-jsonschema check-jsonschema --base-uri "$SCHEMA_BASE" \
    --schemafile contracts/data-processing/processing-frame.v1.schema.json -

jq '.output' crates/aether-data-processing/tests/fixtures/forecast-processing-result.json | \
  uvx --from check-jsonschema check-jsonschema --base-uri "$SCHEMA_BASE" \
    --schemafile contracts/data-processing/forecast-output.v1.schema.json -
```

The generic request, result, and derived-data fixtures map directly to their
schemas. The application request, `ProcessingFrame`, and `ForecastOutput`
commands validate projections from the processor request and result fixtures.
There is currently no standalone error-envelope fixture;
`error.v1.schema.json` is still checked against the Draft 2020-12 metaschema.

Schema validation is the first wire check. It does not replace request byte
limits, media-type checks, deadlines, authorization, egress policy, typed Rust
validation, or processor conformance tests.
