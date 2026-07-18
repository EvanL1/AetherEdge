# aether-data-processing crate

Strict, transport-neutral JSON codec for the Aether Data Processing v1
processor boundary. It converts validated domain values to and from the
versioned RFC 3339/JSON DTOs, encodes Aether-accepted `DerivedData`, and
computes RFC 8785 input digests.

The v1 wire format uses UTC `Z` timestamps with no finer than millisecond
precision, preserves optional external-forecast `SourceProvenance.issued_at`,
and accepts only `interval_end` forecast timestamp semantics. Contract-invalid
or unknown fields fail closed before a transport exposes domain values.
`DerivedDataDto` and `encode_derived_data` are intentionally encode-only: an
external JSON payload cannot cross the boundary by claiming it was accepted
by Aether. The accepted envelope retains the processor contract, immutable
artifact provenance, fallback metadata, warning codes, and Aether-computed
frame quality.

Artifact version/digest is identity, not chronology: v1 has no
`trained_through` or `available_at`. The codec also cannot turn an event-time
`as_of` into a bitemporal historian cut; those guarantees belong to future
source and commissioning contracts.

This crate contains no HTTP client, storage access, callbacks, or model
runtime. Protocol adapters depend on it; the domain does not depend on any
adapter.

See the [normative contract reference](../../docs/reference/data-processing-contracts.md),
[JSON Schemas](../../contracts/data-processing/README.md), and optional
[HTTP adapter](../../extensions/http-data-processor/README.md).

```bash
cargo test -p aether-data-processing
```
