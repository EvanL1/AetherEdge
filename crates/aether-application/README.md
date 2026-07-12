# aether-application

Transport-neutral command and query use cases for the Aether edge kernel.

`EdgeApplication` is shared by CLI, MCP, and optional network transports. It
authorizes point reads, validates device commands, requires an audit sink, and
dispatches control through capability ports. Infrastructure choices stay
outside this crate, so its default graph contains no Redis, PostgreSQL, SQLx,
MQTT client, or web framework.

Control, manual rule execution, rule/alarm policy changes, alert resolution,
I/O channel commissioning, and physical action-routing mutations persist an
`Attempted` audit event before calling a non-idempotent port. Channel audit
details identify changed fields but never include protocol parameter or
per-channel logging values. If the pre-execution event cannot be stored,
execution fails closed. Once the port accepts an operation, failure to append
the terminal `Succeeded` event is returned as an
`AcceptedOutcome` with `CompletionAuditStatus::Incomplete`, the request and
command/rule correlation IDs, and `is_retryable() == false`; it is never turned
into a retryable error that could execute the operation twice. Audit details
include operation-specific command targets, rule identifiers, action counts,
or routing keys.

`DataProcessingApplication` is the transport-neutral query facade for Aether
Data Processing. A composition root registers a declarative task, a
`DataProcessingBinding`, and a `DataProcessor` route. The binding resolves
task-local measurement names to read-only `PointAddress` values and may pin
static features and an artifact selector; processor routes are never selected
by API callers.

The landed external binding is the authenticated
`/api/v1/data-processing/*` HTTP surface in `aether-api`. Data Processing CLI
and MCP bindings are not implemented in version 1.

For each processing request the application authorizes before reading data,
queries bounded history and covariates, optionally merges an exactly aligned
read-only live tail only for `Last`-aggregated features, assembles a complete
frame with one provenance entry per
feature, computes the shared canonical digest, applies exact frame and payload
limits, and treats the processor response as untrusted. Only a correlated,
policy-compatible result becomes `DerivedData`. The facade has no SHM writer,
history sink, or command dispatcher, and an empty route set remains a valid
default configuration. Transport request and result IDs are stable UUIDv5
derivations, while the caller's original request ID remains on audit records.
The query is non-idempotent: repeated content can retain stable correlation IDs
but still executes the processor and required audit for every invocation.

The application bounds source event time; it cannot manufacture chronology
that adapters do not provide. With the current SQLite schema and artifact
selector, an old `as_of` is not proof of an ingestion-time/source-epoch or
model-availability cut. Historical evaluation must supply frozen inputs or
ports whose contracts carry and validate those cuts.

```bash
cargo test -p aether-application
```

Licensed under either MIT or Apache-2.0, at your option.
