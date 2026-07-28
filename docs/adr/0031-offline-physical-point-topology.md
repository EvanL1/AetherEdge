# ADR-0031: Commission physical point topology offline

## Status

Accepted and implemented on 2026-07-28.

## Context

IO exposed a second online configuration system for physical point CRUD,
batched mapping changes, channel templates, snapshot application, and optional
runtime reload. It duplicated the offline configuration import, embedded SQL
and protocol validation in HTTP handlers, and introduced an IO-local topology
application that was not a published application capability shared by CLI,
MCP, and HTTP.

Channel lifecycle already has a governed capability and immutable desired-state
revision. Physical points and protocol mappings are part of a channel's
commissioned definition and are loaded when that desired state is reconciled.

## Decision

1. Physical point definitions and protocol mappings are authored in reviewed
   configuration and applied by `aether sync --confirmed` while runtime owners
   are stopped.
2. IO retains read-only channel point and mapping queries and authoritative live
   point reads.
3. Online point create/update/delete/batch, mapping update, template CRUD,
   channel snapshot/apply, direct T/S simulation injection, the legacy
   `/api/channels/reload` and `/api/routing/reload` aliases, and the private
   topology application are removed.
4. Channel create/update/enable/disable/delete and explicit reconciliation
   remain governed by `io.channel.manage` and `io.channel.reconcile` through
   the canonical channel endpoints.
5. CLI and MCP template commands are removed with the server surface.
6. Architecture tests prevent restoration of the retired modules, routes, and
   mutation symbols.

## Consequences

The acquisition runtime, protocol adapters, C2C mapping execution, SHM layout,
C2M/M2C routing, and governed channel lifecycle remain unchanged. Commissioning
is less interactive but atomic: a reviewed channel topology cannot be partly
applied through a sequence of private HTTP mutations. Existing clients of the
unreleased compatibility endpoints must use offline import and reconciliation.
