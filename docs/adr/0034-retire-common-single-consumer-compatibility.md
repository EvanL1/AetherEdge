# ADR-0034: Retire common single-consumer compatibility

## Status

Accepted and implemented on 2026-07-28.

## Context

After the shared service kernel became console-first, `common` still owned
several helpers with exactly one production consumer and several compatibility
contracts with no reachable capability:

- CSV header validation was used only by offline `aether sync`.
- Generic configuration fallback was used only by `aether-io` bootstrap.
- HTTP dependency polling was used only by the automation composition root.
- Host CPU/memory checks only logged warnings and duplicated installer and
  supervisor ownership.
- `FourRemote` duplicated the canonical storage/wire `PointType`.
- `channel_templates` had no online API, no offline import path, and no runtime
  reader. Exporting rows from an old database could not produce configuration
  that the current CLI could apply.
- Standalone `rules.yaml` validation/export was unreachable from the site-level
  CLI. Rules are commissioned under `automation/rules` as part of the atomic
  automation configuration transaction.

## Decision

1. Single-consumer helpers move to their owners: CSV validation to the CLI,
   bind fallback to IO, and IO health polling to automation.
2. The warning-only runtime host requirements checker is removed. Deployment
   prerequisites belong to installers and supervisors; process health metrics
   remain available through service health APIs.
3. All physical configuration DTOs use `PointType` directly. The `FourRemote`
   Rust alias is removed without changing the established T/S/C/A wire values.
4. Fresh and migrated schemas no longer create, index, rebuild, read, or export
   `channel_templates`. An existing legacy table may remain inert in an old
   database; no runtime owner reads it.
5. Standalone rules validation/export DTOs and branches are removed. Atomic
   automation sync/export remains the single rules commissioning path.
6. The common SQLite DDL is explicit in `site_schema`; the now-zero-consumer
   `aether-schema-macro` crate and the unused common Schemars feature are retired.
7. Architecture tests prevent these single-consumer and zero-consumer surfaces
   from returning to `common`.

## Consequences

`common` contains only genuinely shared service/configuration capabilities and
the current local site-schema authority. The CLI and composition roots own
their concrete behavior without adding another generic abstraction layer.
Existing databases are not destructively rewritten merely to remove an inert
legacy template table.
