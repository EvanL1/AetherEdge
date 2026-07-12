# ADR-0011: Govern channel desired state and runtime projection

## Status

Accepted on 2026-07-12. The application, default SQLite/runtime adapter, and
HTTP/CLI/MCP CRUD, lifecycle, and runtime-reconciliation migrations are
implemented as one staged change. Offline bulk configuration import remains an
explicitly tracked compatibility exception below; it is not an online command
surface.

## Context

The I/O HTTP handlers previously coordinated SQLite and `ChannelManager`
directly. Create, update, enable, disable, and delete used different ordering
rules. Several paths could commit `enabled=true` after runtime creation failed,
remove a runtime before a database write failed, swallow dependent-delete
errors, or return HTTP 200 while describing a runtime that did not exist.

Those handlers were also a separate command plane. CLI and test-only MCP
wrappers called the HTTP endpoints, but there was no transport-neutral
capability, permission, confirmation policy, audit flow, compare-and-set token,
or typed distinction between durable intent and the active protocol runtime.
The legacy update body could additionally migrate a channel ID and all of its
references as an incidental side effect.

SHM remains authoritative for live point values. This ADR concerns the
authority for commissioned channel configuration and its runtime projection;
it does not move live-state authority into SQLite.

## Decision

1. Channel create, partial update, enable, disable, and delete are the
   `io.channel.manage` application command. It is high risk, requires the
   `io.channel.manage` permission and explicit confirmation, requires durable
   audit, and is never advertised as idempotent or safe for automatic retry.
2. `aether-ports` owns the transport-neutral `ChannelMutator` contract.
   Protocol parameters use a recursively typed ordered value tree. Per-channel
   logging is a separate typed policy and is not hidden inside protocol
   parameters. Parameter and logging values are redacted from debug and audit
   records.
3. SQLite is authoritative for desired channel configuration in the default
   composition. A `revision` column starts at one. Explicit revisions use SQL
   compare-and-set; revisionless compatibility calls are serialized by channel
   and the schema trigger increments revisions for remaining legacy writers.
   Deletion persists a per-identity revision high-water mark, so recreating the
   same numeric ID advances beyond every token issued to the deleted entity and
   cannot suffer an ABA match. Automatic allocation never reuses a tombstoned
   ID because history, alarms, logs, and external integrations still identify
   channels by the bare number; an operator may explicitly reuse one, in which
   case its revision advances past the high-water mark. A compatibility trigger
   also advances staged direct inserts within their transaction.
4. The active `ChannelManager` entry is a rebuildable projection. Receipts
   report the resulting revision, desired enabled state, and one of `stopped`,
   `activation_pending`, `active`, `degraded`, or `removed`. A desired-state
   commit followed by runtime failure is an accepted degraded result with
   `reconciliation_required=true`, not an ambiguous retryable error.
   Reconciling an unchanged enabled/disabled value does not increment the
   desired-content revision. A present runtime is fenced and rebuilt from the
   authoritative configuration; if a compatibility writer changes SQLite
   while reconciliation is in flight, the receipt reports the latest observed
   revision and a degraded projection rather than claiming stale state is
   active.
5. Create, update, and enable validate before committing. Once desired state is
   committed they reconcile the runtime. Disable and delete fence the runtime
   before committing; a failed database commit attempts to restore the prior
   enabled projection. Runtime and storage diagnostics exposed to callers are
   categorized and sanitized.
6. Delete is transactional for measurement-owned channel records. It returns
   conflict while a governed action route references the channel and never
   cascades or nulls that command topology as an incidental I/O mutation.
7. Ordinary update cannot change channel identity. A body `channel_id` may
   echo the path ID during migration, but a different ID is rejected before
   the application port runs. A future identity migration, if needed, is a
   separate high-risk use case that coordinates every referencing aggregate.
8. HTTP, CLI, and MCP use the same application commands. HTTP independently
   verifies the signed access token and forwards the generated or validated
   request ID, explicit confirmation, and optional expected revision. Swagger
   UI documents the exact security headers, typed accepted receipt, degraded
   semantics, and 400/403/404/409/422/503/504 failure categories. CLI and MCP
   attach Bearer credentials only to loopback HTTP or certificate-validated
   HTTPS service URLs; remote plaintext HTTP fails before token selection.
   Runtime convergence is the separately discoverable `io.channel.reconcile`
   command. `POST /api/channels/reconcile` is canonical; the historical
   `POST /api/channels/reload` path is a deprecated alias over that same
   authenticated, confirmed, audited application boundary.
9. The default distribution adds no external dependency. Command audit and
   desired configuration remain in the local SQLite database; Redis and
   PostgreSQL are not required.

## Compatibility and removal criteria

- The historical `PUT /api/channels/{id}` path is retained with its existing
  partial-update semantics. Top-level protocol parameter keys are merged.
  Rename it or change its merge behavior only after every supported client
  consumes a versioned replacement.
- Expected revision is optional while existing clients migrate. Remove the
  revisionless constructors only after the web client, CLI, MCP catalog, sync,
  import/export, and supported downstream SDKs all read and send revisions,
  and the compatibility matrix rejects older clients explicitly.
- Legacy response aliases may remain alongside the typed receipt until those
  same consumers use `runtime_projection`, `desired_enabled`, and
  `resulting_revision`.
- The old route-factory signature installs an unavailable command boundary and
  therefore fails closed. Remove it after every production and test
  composition root injects the governed application explicitly.
- Direct lifecycle and ID-migration handler modules have no accepted fallback
  role and are removed in this change.
- `POST /api/channels/reload` remains only as a deprecated compatibility alias
  over `io.channel.reconcile`; it uses the same lifecycle serialization,
  authentication, confirmation, audit, and typed receipt as the canonical
  `/api/channels/reconcile` route. Remove the alias after the browser client,
  supported SDKs, deployment automation, and downstream integrations consume
  the canonical route and the compatibility matrix rejects older clients.
- `aether sync` remains an offline desired-state import rather than an online
  runtime command. It requires explicit operator confirmation and refuses to
  apply while the configuration-owning services are running; imported state is
  activated on the next supervised service start. A future online site-level
  apply command, if required, must declare its own capability and coordinate
  authentication, confirmation, audit, revisions, and cross-service receipts.

## Consequences

### Positive

- All supported CRUD/lifecycle transports enforce one permission,
  confirmation, audit, CAS, and recovery contract.
- A 200 response no longer falsely equates durable intent with a connected
  protocol runtime.
- Concurrent updates are observable conflicts instead of silent lost writes.
- Deleting a measurement channel cannot silently rewrite future device-command
  destinations.
- The default single-host Kernel remains independent of external services.

### Negative

- Commissioning mutations now require a signed Admin/Engineer access token and
  explicit confirmation even on loopback.
- Runtime projection can legitimately be degraded after desired state commits;
  operators and clients must reconcile by request ID rather than blindly
  retrying.
- The staged optional-revision form remains less strict than the final CAS-only
  API and must be tracked against the removal criteria above.
- Offline sync remains a separate desired-state import and must not be
  advertised as an online application command or run beside configuration
  owners.
- The deprecated `/api/channels/reload` alias temporarily enlarges the HTTP
  compatibility surface even though it no longer creates a second runtime
  owner.
- Deleted numeric IDs are not returned to the automatic-allocation pool. Sites
  with unusually high commissioning churn must explicitly reuse an ID (and
  inherit its revision high-water mark) or migrate to a future wider identity
  contract.

## Verification

```bash
cargo test -p aether-ports --test channel_management_contract
cargo test -p aether-application --test channel_management_application
cargo test -p aether-io --test channel_mutator_contract
cargo test -p aether-io --features swagger-ui --lib --bins openapi
cargo test -p aether --bin aether channels
cargo test -p aether --bin aether mcp::tests
./scripts/check-openapi-contracts.sh
./scripts/check-architecture.sh
```
