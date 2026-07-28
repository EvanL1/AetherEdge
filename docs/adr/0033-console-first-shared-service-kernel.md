# ADR-0033: Keep the shared service kernel console-first

## Status

Accepted and implemented on 2026-07-28.

## Context

The unpublished `common` crate had become a compatibility container rather than
a narrow shared service kernel. It combined reusable response/configuration
DTOs with host log-file browsing, application-owned log rotation and
compression, an unused generic hot-reload framework, dormant CLI feature
re-exports, and a production SQLite schema hidden below a module named
`test_utils`.

The six runtime processes already run under Docker Compose or systemd. Those
supervisors own process log capture, rotation, retention, and retrieval. The
runtime had no first-party consumer for the generic automation reload trait,
and the retired instance reload endpoint was its only intended trigger.

IO channel diagnostics are different from process logs: they are explicit,
per-channel protocol evidence selected by governed channel configuration and
remain owned by `aether-io`.

## Decision

1. Shared process tracing writes to the console stream. `RUST_LOG` and the
   authenticated dynamic log-filter endpoint control verbosity; Docker or
   systemd owns capture, rotation, retention, and retrieval.
2. Process log-file list/view endpoints, common file writers, compression,
   SIGHUP reopen tasks, global logging YAML, and non-IO log volume mounts are
   removed.
3. IO per-channel protocol diagnostic files remain an explicit IO capability.
   `AETHER_LOG_DIR` now refers only to that capability.
4. The zero-consumer `ReloadableService` framework and automation reload
   implementation are removed. Startup and governed application commands own
   reconciliation.
5. The deployed SQLite schema is exposed truthfully as `common::site_schema`.
   Production bootstrap, offline commissioning, and conformance tests use the
   same authority; production code must not import a module named
   `test_utils`.
6. Dormant common CLI feature re-exports, placeholder service arguments,
   unused API filter DTOs, and unused compatibility deserializers are removed.
7. Shared code remains limited to capabilities used by multiple composition
   roots. Single-consumer helpers should move to their owning service or tool.

## Consequences

Normal process logs are available through `journalctl` or container log tools
rather than an application filesystem API. This removes a host-filesystem
surface from every service and avoids a second retention owner. Operators that
need raw protocol evidence can still enable governed IO channel diagnostics.

The `common` crate remains unpublished and still owns shared configuration and
site-schema DTOs for now. Moving SQLite schema ownership into
`aether-store-local` may be considered separately; this decision does not add a
new generic platform layer.
