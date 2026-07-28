# ADR-0028: Move derived-data processing downstream

## Status

Accepted and implemented on 2026-07-28. Supersedes ADR-0009.

## Context

ADR-0009 introduced an optional generic processor contract, domain model,
ports, orchestration, codec, API routes, storage adapters, Pack assets, and
forecasting examples. The six-process runtime did not require them, and their
only concrete product use was energy forecasting owned by AetherEMS.

This vertical slice added a second history-query path, processor transport,
covariate storage, derived-data contracts, and more than ten thousand lines of
production Rust without participating in acquisition, deterministic rules,
alarms, history, uplink, or governed device control.

## Decision

1. Derived-data processing, forecasting, covariates, model artifacts, processor
   transports, and task declarations are downstream application concerns.
   AetherEMS owns the energy implementation.
2. AetherEdge removes the Data Processing domain types, ports, application use
   cases, codec crate, API composition/routes, SQLite query adapter, covariate
   adapters, Pack category, examples, fixtures, and safety capabilities.
3. The default runtime remains exactly six processes. No reserved seventh
   processor service or hidden processor endpoint remains.
4. Downstream applications consume published point, history, command, and Pack
   boundaries as appropriate. Static Pack metadata cannot grant SHM, storage,
   credential, or device-control authority.
5. Architecture tests forbid restoration of the retired crates and source
   paths. A new kernel processing capability requires a later accepted ADR and
   a concrete industry-neutral runtime need.

## Consequences

The kernel is smaller and has one history writer/read API rather than a second
processor-specific query stack. Acquisition, SHM authority, C2M/M2C routing,
rules, alarms, history, API, and CloudLink behavior are unchanged. Existing
AetherEdge Data Processing clients are not compatibility-supported because the
surface was opt-in and unreleased; deployments move that concern to AetherEMS
or another downstream application.
