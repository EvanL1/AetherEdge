# ADR-0029: Keep the remote application boundary headless

## Status

Accepted and implemented on 2026-07-28.

## Context

`aether-api` combined the authenticated application gateway with an inherited
browser backend: direct SHM projection, PointWatch, WebSocket subscriptions,
arbitrary broadcast, calculated homepage points, host-network inspection,
configuration ZIP transfer, process restart, and firmware-upgrade placeholders.
The default AetherEdge product has no browser application, and most mutation
handlers were intentionally disabled because they lacked governed application
capabilities.

This gave the API a second live-state projection and unnecessary SHM,
filesystem, host-network, archive, and process-management authority.

## Decision

1. `aether-api:6005` remains the only authenticated remote entry point, owning
   JWT sessions and fixed routes to the six-process application interfaces.
2. The kernel removes WebSocket/broadcast, homepage, host-network,
   configuration archive, service restart, and remote upgrade routes.
3. `aether-api` no longer attaches to SHM, PointWatch, or local routing/storage
   adapters. Remote observations use authenticated application query routes.
4. Browser consoles, live dashboards, host administration, and release
   orchestration belong downstream or in deployment tooling.
5. Query-string access tokens are retired; HTTP authentication uses the Bearer
   header only.
6. Architecture tests prevent restoration of the retired modules and concrete
   live-state adapter dependencies.

## Consequences

The six processes and API port remain unchanged. JWT and application gateway
clients remain supported. Clients of the unreleased browser compatibility
routes must move to REST query polling or a downstream console backend. API
failure still cannot affect acquisition, deterministic rules, alarms, history,
or local control.
