# Add a data processor

1. Start from a domain question and choose the narrowest typed
   `DataProcessingTask`; do not expose an arbitrary `run(json)` capability.
2. Declare semantic inputs, units, history window, live-tail policy, request
   context, output schema, size limits, timeout, expiry, and failure behavior in
   the owning industry pack. Permit live tail only for `Last`; an instantaneous
   SHM value cannot replace a `Mean`, `Sum`, `Min`, or `Max` bucket. For a
   forecast, forbid the target from its future covariates.
3. Add behavior tests for frame assembly and result validation before changing
   application code.
4. Reuse `LiveState` and `HistoryQuery`. Never give a processor
   `LiveStateWriter`, database credentials, SHM paths, or internal service URLs.
   The current API composition uses a transitional read-only SQLite adapter;
   do not add another direct database reader.
5. Implement the smallest `DataProcessor` adapter in the owning service or a
   downstream distribution. Keep HTTP, Python, ONNX, cloud, and vendor
   dependencies out of core crates.
6. Pass the `aether-testkit` data-processor conformance suite, including request
   identity, input digest, finite output, timeout, unavailable, and malformed
   result cases.
7. Add a composition test with a deterministic local processor. A domain pack
   example remains disabled until site commissioning explicitly binds its
   semantic inputs and selects a processor. Historical evaluations must freeze
   history/source epochs and the artifact registry, or use bitemporal history
   plus validated artifact availability/training cuts; event-time `as_of`
   alone is insufficient.
8. Declare AI-facing kind, risk, permission, idempotency, timeout,
   confirmation, and audit policy. `data_processing.process` is a Medium-risk
   non-idempotent query with policy confirmation because an approved route may
   cross a remote egress boundary, and its audit is required. Do not add replay,
   de-duplication, cache, or `409` guarantees unless those mechanisms are
   implemented and tested. Task/processor discovery is
   Low-risk with `confirmation: never` and `audit: not_required`; task/route
   changes and publishing into automatic plans are separate commands.
9. Update `llms.txt`, the Data Processing concept/reference pages,
   `ai/catalog.yaml`, and generated capability indexes.
10. Run the narrow tests, workspace formatting/clippy/tests, and
    `./scripts/check-architecture.sh`.
11. If commissioning reads the embedded historian directly, treat storage
    changes as maintenance: disable processing, reconnect/restart history,
    verify the active SQLite backend and a sentinel series, then restart the
    composition on the same path. Saved `history_config.storage_*` is not an
    active-writer attestation. Give the API only independently permissioned
    read access to the historian database/WAL/SHM directory; do not treat
    SQLite read-only flags or a shared writable data-directory mount as the
    production boundary.
