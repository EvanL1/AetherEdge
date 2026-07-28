# ADR-0036: Retire zero-consumer service aliases

## Status

Accepted and implemented on 2026-08-12.

## Context

After IO composition was aligned with the runtime manifest, several internal
service APIs still advertised behavior that was either impossible or duplicated
a canonical query:

- History exposed `influxdb` as a selectable storage backend even though every
  backend operation returned an unimplemented error.
- Automation exposed browser-oriented instance list, search, and export routes
  with no production consumer. CloudLink instance synchronization already reads
  the canonical paginated `/api/instances` query.
- Uplink exposed a local HTTP trigger for instance synchronization even though
  the owned CloudLink `inst-sync` topic drives the same operation.
- IO, Automation, History, Uplink, and Alarm retained a `swagger-ui` feature
  alias even though only the authenticated `aether-api` process owns Swagger
  UI.

These surfaces increased the apparent capability set, generated OpenAPI
contracts, and compatibility burden without adding deployable behavior.

## Decision

1. Remove the unimplemented InfluxDB history backend and every selectable,
   probe, model, and documentation branch that advertised it.
2. Preserve embedded SQLite and the real optional PostgreSQL/TimescaleDB
   adapters. Their future distribution boundary is a separate decision.
3. Remove `/api/instances/list`, `/api/instances/search`, and
   `/api/instances/export`. Callers use `/api/instances` and specific instance
   queries instead.
4. Remove `POST /netApi/inst-sync`. CloudLink-triggered instance sync and its
   durable MQTT reply behavior remain unchanged.
5. Remove service-local `swagger-ui` feature aliases. Internal services may
   emit OpenAPI documents; only `aether-api` may host Swagger UI.
6. Keep an architecture test that rejects restoration of these files, routes,
   feature aliases, and fake backend selectors.

## Consequences

- History no longer reports an adapter that can never start successfully.
- Automation has one canonical instance collection query.
- Uplink retains one owned instance-sync trigger: the CloudLink protocol.
- Service feature sets match their actual UI ownership.
- PostgreSQL/TimescaleDB, SQLite history collection, CloudLink transport,
  authentication, and protocol-specific tests are unaffected.
