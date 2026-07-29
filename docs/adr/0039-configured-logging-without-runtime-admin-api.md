# ADR-0039: Configure logging without a runtime admin API

## Status

Accepted and implemented on 2026-08-20.

## Context

Every service exposed `GET/POST /api/admin/logs/level` through a shared
`common::admin_api` implementation. The mutation changed only an in-memory
tracing filter, was lost on restart, had no capability-catalog declaration or
audit contract, and had no CLI or MCP consumer. Host operators already own
process logging through `RUST_LOG`, Docker Compose or systemd, and the host log
collector.

IO additionally exposed `PUT /api/channels/{id}/logging`. That endpoint sent a
private task command which changed protocol and file-log verbosity without
updating the authoritative channel desired state, revision, confirmation, or
audit record. Governed channel configuration already contains the persistent
per-channel logging policy and applies it whenever the channel is composed or
reconciled.

The retired endpoints kept reload handles, mutable log-level state, forwarding
handler modules, protocol commands, and hot-reload methods alive across the
shared kernel and IO runtime.

## Decision

1. Process tracing filters are selected at process startup from `RUST_LOG`.
   Changing them is a deployment operation followed by a supervised restart.
2. Remove `/api/admin/logs/level` from API, IO, Automation, History, Uplink,
   and Alarm, together with the shared request/response handlers and OpenAPI
   schemas.
3. Remove IO `/api/channels/{id}/logging`. Per-channel diagnostic verbosity is
   changed through the governed channel update contract and reconciliation.
4. Remove the `SetLogLevel` task command, channel-entry mutation, protocol
   hot-reload hook, composite-handler mutation, and atomic file-log level.
5. Keep per-channel protocol diagnostic files. Their configured level is fixed
   for a channel generation and is reapplied from desired state on rebuild.
6. Remove IO forwarding modules that exposed shared SQLite DDL through the
   runtime configuration namespace. Tests import the authoritative
   `common::site_schema` directly.
7. Add source and OpenAPI checks that reject restoration of the ephemeral
   administration surfaces.

## Consequences

- Runtime logging has no unaudited or restart-volatile HTTP mutation.
- Process log policy has one owner: deployment configuration.
- Channel diagnostic policy has one owner: governed channel desired state.
- Protocol packet evidence, tracing output, SHM behavior, physical protocols,
  and automatic reconciliation remain available.
- The internal service OpenAPI documents contain only service capabilities,
  not host-process administration aliases.
