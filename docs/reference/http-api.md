---
title: HTTP API
description: Response envelope conventions, authentication, and service endpoint overview
updated: 2026-07-10
---

# HTTP API

Aether's HTTP surface spans six services. Browsers and external clients go
through the API gateway (port 6005), which enforces JWT authentication; the
other five services listen on their own ports without authentication and are
intended for intra-host use. This page documents the response envelopes, how
authentication works, and an endpoint overview per service.

## Envelope conventions

**Success** responses are uniform across services
(`SuccessResponse` in `libs/common/src/api_types.rs`):

```json
{ "success": true, "data": { ... }, "metadata": { ... } }
```

`metadata` is omitted when empty.

**Error** responses come in three shapes that coexist for historical reasons.
A client must branch on the shape of the body, not assume one format.

1. **Typed errors** — io handlers return `AppError` and automation's
   `AutomationError` converts into it (`libs/common/src/api_types.rs`,
   `services/automation/src/error.rs`). The body wraps an `ErrorInfo`:

   ```json
   {
     "success": false,
     "error": {
       "code": 404,
       "message": "Instance not found: pcs_01",
       "details": "...",
       "suggestion": "...",
       "field_errors": { "name": ["must not be empty"] }
     }
   }
   ```

   `details`, `suggestion`, and `field_errors` are optional and omitted when
   absent.

2. **Inline handler errors** — many handlers in history, uplink, alarm,
   and the gateway build the body directly with `json!` (for example
   `services/history/src/routes.rs` and the gateway's
   `services/api/src/routes_auth.rs`):

   ```json
   { "success": false, "message": "No data available" }
   ```

3. **`AetherError` mapping** — the shared errors library defines a third
   shape, a flat object with no `success` field, produced by
   `AetherErrorTrait::into_http_response` (`libs/errors/src/lib.rs`). No
   route currently emits it — the gateway's own handlers use shape 2 — but
   it is the format any endpoint adopting the trait's HTTP mapping would
   return:

   ```json
   {
     "error_code": "NOT_FOUND",
     "message": "resource not found",
     "category": "NotFound",
     "retryable": false,
     "retry_delay_ms": 0,
     "suggestion": "..."
   }
   ```

   `suggestion` appears only when the error has one.

A client talking to the current services will encounter shapes 1 and 2; a
robust client should branch on the presence of the `error` object versus a
top-level `message` (and tolerate shape 3). In every shape the HTTP status
code carries the primary signal; the body adds machine-readable detail.

## Authentication

The API gateway enforces JWT authentication through the `require_jwt`
middleware (`services/api/src/middleware_auth.rs`). It accepts a
Bearer token in the `Authorization` header, with `?token=...` as a
query-string fallback because browsers cannot set custom headers on WebSocket
upgrades. Missing or invalid tokens get `401 Unauthorized`.

Coverage (wired in `build_router` in `services/api/src/main.rs`):

- **Protected:** everything under `/api/v1` except `/api/v1/auth`
  (homepage, network, config, broadcast), all of `/api/admin`, and `/ws`.
- **Public transport surface:** `/api/v1/auth/register`, `/login`, and
  `/refresh`, plus `/` and `/health`. Public registration is deny-by-default:
  it returns `403` unless `AETHER_ALLOW_PUBLIC_REGISTRATION=true`, and an
  opted-in registration always creates the least-privileged Viewer role.
  Other auth handlers perform their own token/Admin checks.

Obtain a token with `POST /api/v1/auth/login`; refresh it with
`POST /api/v1/auth/refresh`. Access tokens expire after 30 minutes and
refresh tokens after 7 days by default (`ACCESS_TOKEN_EXPIRE_MINUTES`,
`REFRESH_TOKEN_EXPIRE_DAYS`); the signing secret comes from the required
`JWT_SECRET_KEY` environment variable. It must contain at least 32 bytes;
generate a unique value with `openssl rand -hex 32` and keep it outside source
control.
(`services/api/src/config.rs`).

On a database whose `users` table is completely empty, `aether-api` also
requires `AETHER_BOOTSTRAP_ADMIN_PASSWORD` before it will start. The value is
the raw password an operator will enter in the login UI, must contain at least
16 characters without surrounding whitespace or control characters, and must
not be a documented/common default. It is converted to the existing
browser-login digest format and bcrypt-hashed; `aether-api` never logs it. Once
any user exists, the bootstrap variable is not read and cannot silently
recreate a deleted administrator.

Every `/api/v1/config/*` endpoint additionally requires the Admin role.
Configuration export and denied remote-upgrade attempts emit structured
authorization events to the service log; a durable audit-ledger adapter is
still required before these logs can be treated as tamper-evident audit proof.

`GET /api/v1/network` remains an authenticated read-only view. Network
mutation is not part of the remote management surface: `PUT /api/v1/network`
and `POST /api/v1/network/apply` require Admin and return `501`. The API's
systemd-networkd mount is intentionally read-only; use an on-device
commissioning workflow with recovery access to change network settings.

The direct service ports — io 6001, automation 6002, history 6004, uplink
6006, alarm 6007 — remain an intra-host surface and must not be exposed beyond
the device. The automation device-action endpoint additionally verifies a
signed access JWT or the dedicated uplink service credential; forwarded
identity headers and loopback reachability are never sufficient. Other local
management endpoints still rely on the intra-host boundary.

## Endpoint overview

This is an overview derived from each service's route registrations, not an
exhaustive parameter reference. All six services have a `swagger-ui` cargo
feature; installer builds made with `./scripts/build-installer.sh
--enable-swagger` enable it for every service, exposing an interactive
OpenAPI UI at `/docs` and the spec at `/openapi.json` on each service port.

Point-type shorthand: T (telemetry), S (signal), C (control), A (adjustment).

### aether-io (port 6001)

Routes from `services/io/src/api/routes.rs`.

| Method | Path | Purpose |
|--------|------|---------|
| GET | `/health` | Health check |
| GET | `/api/status` | Service status |
| GET | `/api/protocols` | List protocols compiled into this binary |
| GET, POST | `/api/channels` | List all channels / create a channel |
| GET | `/api/channels/list` | Slim channel list |
| GET | `/api/channels/search` | Search channels |
| GET | `/api/points` | List all points across channels |
| GET, PUT, DELETE | `/api/channels/{id}` | Channel detail / update / delete |
| GET | `/api/channels/{id}/status` | Channel runtime status |
| POST | `/api/channels/{id}/control` | Start or stop a channel |
| PUT | `/api/channels/{id}/enabled` | Enable or disable a channel |
| PUT | `/api/channels/{id}/logging` | Set per-channel log level |
| GET | `/api/channels/{id}/points` | List a channel's points |
| GET | `/api/channels/{id}/unmapped-points` | Points without protocol mappings |
| GET, PUT | `/api/channels/{id}/mappings` | Read / replace protocol mappings |
| GET | `/api/channels/{channel_id}/{type}/points/{point_id}/mapping` | Single point's mapping |
| POST | `/api/channels/reload` | Reload channel configuration |
| POST | `/api/routing/reload` | Reload routing tables |
| GET, POST, PUT, DELETE | `/api/channels/{channel_id}/{T\|S\|C\|A}/points/{point_id}` | Point CRUD, one route per point type |
| POST | `/api/channels/{channel_id}/points/batch` | Batch point create/update/delete |
| POST | `/api/channels/{channel_id}/provision` | Provision a channel |
| POST | `/api/channels/{channel_id}/write` | Inject a T/S simulation value; direct C/A commands are rejected |
| GET | `/api/channels/{channel_id}/{telemetry_type}/{point_id}` | Point info with current value |
| GET, POST | `/api/admin/logs/level` | Read / set runtime log level |
| GET | `/api/admin/logs/files`, `/api/admin/logs/view` | List / view log files |
| GET, POST | `/api/templates` | List / create channel templates |
| POST | `/api/templates/from-channel/{channel_id}` | Create template from an existing channel |
| GET, PUT, DELETE | `/api/templates/{id}` | Template detail / update / delete |
| POST | `/api/templates/{id}/apply/{channel_id}` | Apply a template to a channel |
| GET | `/api/network/interfaces` | List network interfaces |
| GET, PUT | `/api/network/interfaces/{name}` | Read / update one interface |
| POST | `/api/network/apply` | Apply pending network changes |

### aether-automation (port 6002)

Routes from `services/automation/src/routes.rs` and
`services/automation/src/rule_routes.rs`.

| Method | Path | Purpose |
|--------|------|---------|
| GET | `/health` | Health check |
| GET, POST | `/api/instances` | List instances / create an instance |
| GET | `/api/instances/list` | Slim instance list |
| GET | `/api/instances/search` | Search instances |
| GET, PUT, DELETE | `/api/instances/{id}` | Instance detail / update / delete |
| GET | `/api/instances/{id}/data` | Instance current values |
| GET | `/api/instances/{id}/points` | Instance point definitions |
| POST | `/api/instances/{id}/sync` | Sync one instance's measurements |
| POST | `/api/instances/{id}/action` | Execute an action (control write) |
| POST | `/api/instances/{id}/measurement` | Set a measurement value directly |
| GET | `/api/instances/{id}/children` | Child instances in the topology |
| GET | `/api/topology` | Full instance topology tree |
| POST | `/api/instances/sync/all` | Sync all instances |
| POST | `/api/instances/reload` | Reload instances from the database |
| GET, POST, PUT, DELETE | `/api/instances/{id}/routing` | Instance-level routing CRUD |
| POST | `/api/instances/{id}/routing/validate` | Validate routing before saving |
| GET | `/api/instances/{id}/measurements/{point_id}` | Single measurement point |
| PUT, DELETE, PATCH | `/api/instances/{id}/measurements/{point_id}/routing` | Upsert / delete / toggle measurement routing |
| GET | `/api/instances/{id}/actions/{point_id}` | Single action point |
| PUT, DELETE, PATCH | `/api/instances/{id}/actions/{point_id}/routing` | Upsert / delete / toggle action routing |
| PUT, DELETE | `/api/instances/{id}/properties/{property_id}` | Upsert / delete an instance property |
| GET, DELETE | `/api/routing` | All routing entries / delete all |
| GET | `/api/routing/by-channel/{channel_id}` | Routing entries for one channel |
| DELETE | `/api/routing/instances/{id}` | Delete an instance's routing |
| DELETE | `/api/routing/channels/{channel_id}` | Delete a channel's routing |
| GET | `/api/products` | List product definitions |
| GET | `/api/products/{product_name}/points` | A product's point definitions |
| GET | `/api/instances/export` | Export instances (cloud sync) |
| GET, POST | `/api/rules` | List rules / create a rule |
| GET, PUT, DELETE | `/api/rules/{id}` | Rule detail / update / delete |
| POST | `/api/rules/{id}/enable`, `/api/rules/{id}/disable` | Enable / disable a rule |
| POST | `/api/rules/{id}/execute` | Execute a rule immediately |
| GET | `/api/rules/{id}/variables` | Variables referenced by a rule |
| GET | `/api/scheduler/status` | Rule scheduler status |
| POST | `/api/scheduler/reload` | Hot-reload rules into the scheduler |
| GET, POST | `/api/admin/logs/level` | Read / set runtime log level |
| GET | `/api/admin/logs/files`, `/api/admin/logs/view` | List / view log files |

### aether-history (port 6004)

Routes from `services/history/src/routes.rs`.

| Method | Path | Purpose |
|--------|------|---------|
| GET | `/`, `/ping`, `/hisApi/health` | Service info / liveness / health |
| GET | `/hisApi/data/query` | Query a time range of history data |
| GET | `/hisApi/data/latest` | Latest stored values |
| GET | `/hisApi/data/range` | Available data time range |
| POST | `/hisApi/data/batch-query` | Query multiple series in one request |
| GET | `/hisApi/channels` | Channels with stored history |
| GET | `/hisApi/metrics` | Storage metrics |
| GET, PUT | `/hisApi/config` | Read / update history configuration |
| GET, PUT | `/hisApi/storage` | Read / switch the storage backend |
| POST | `/hisApi/storage/test` | Test a storage backend connection |
| POST | `/hisApi/storage/reconnect` | Reconnect the storage backend |
| GET, POST | `/api/admin/logs/level` | Read / set runtime log level |
| GET | `/api/admin/logs/files`, `/api/admin/logs/view` | List / view log files |

`/hisApi/health` reports both `backend` (the configured intent) and
`active_backend` (the adapter actually serving requests), together with
`storage_enabled` and `storage_healthy`. The default embedded SQLite backend
is part of the core acceptance contract: if it is enabled but cannot start,
history exits and `aether doctor` reports an error. An explicitly selected
external backend may fail into a visible disabled/degraded state without
turning PostgreSQL or another external service into a core runtime dependency.

### aether-api (port 6005)

Routes from `services/api/src/main.rs`. Everything below except
`/api/v1/auth/*`, `/`, and `/health` requires a JWT.

| Method | Path | Purpose |
|--------|------|---------|
| GET | `/`, `/health` | Service banner / health |
| GET | `/ws` | WebSocket upgrade (see below) |
| POST | `/api/v1/auth/register`, `/login`, `/refresh`, `/logout` | Account and session lifecycle |
| GET, PUT | `/api/v1/auth/me` | Current user profile |
| PUT | `/api/v1/auth/me/password` | Change own password |
| GET | `/api/v1/auth/roles` | List roles |
| GET | `/api/v1/auth/users` | List users (admin) |
| GET, PUT, DELETE | `/api/v1/auth/users/{id}` | User admin CRUD |
| GET | `/api/v1/auth/stats` | Auth statistics |
| POST | `/api/v1/auth/cleanup-tokens` | Purge expired tokens |
| GET | `/api/v1/auth/validate` | Validate a token |
| POST | `/api/v1/broadcast` | Push a message to WebSocket clients |
| GET | `/api/v1/broadcast/status` | Broadcast hub status |
| GET, POST | `/api/v1/homepage`, `/api/v1/homepage/reset` | Homepage point list / reset |
| GET, PUT | `/api/v1/homepage/{id}` | Homepage point detail / update |
| GET | `/api/v1/network` | Read network configuration (authenticated) |
| PUT | `/api/v1/network` | Disabled (`501`, Admin-only) |
| POST | `/api/v1/network/apply` | Disabled (`501`, Admin-only) |
| GET | `/api/v1/config/check`, `/api/v1/config/export` | Validate / export configuration |
| POST | `/api/v1/config/import` | Disabled (`501`) pending staged validation and atomic rollback |
| POST | `/api/v1/config/restart-services` | Disabled (`501`); use the local deployment-aware CLI |
| POST | `/api/v1/config/upgrade`, `/upgrade/abort` | Disabled (`501`); remote and in-place runtime upgrades are not supported |
| GET | `/api/v1/config/upgrade/status` | Read the compatibility status response only; it does not expose a supported upgrade workflow |
| GET, POST | `/api/admin/logs/level` | Read / set runtime log level |
| GET | `/api/admin/logs/files`, `/api/admin/logs/view` | List / view log files |

### aether-alarm (port 6007)

Routes from `services/alarm/src/routes.rs`.

| Method | Path | Purpose |
|--------|------|---------|
| GET | `/`, `/health` | Service info / health |
| GET, POST | `/alarmApi/rules` | List / create alarm rules |
| GET | `/alarmApi/rules/channel/{channel_id}` | Rules for one channel |
| GET, PUT, DELETE | `/alarmApi/rules/{id}` | Alarm rule detail / update / delete |
| PATCH | `/alarmApi/rules/{id}/enable`, `/disable` | Enable / disable a rule |
| GET | `/alarmApi/alerts` | List active alerts |
| GET | `/alarmApi/alerts/{id}` | Alert detail |
| PATCH | `/alarmApi/alerts/{id}/resolve` | Resolve an alert |
| GET | `/alarmApi/alert-events` | Alert event history |
| GET | `/alarmApi/alert-events/export` | Export events as CSV |
| GET | `/alarmApi/alert-statistics` | Alert statistics |
| GET | `/alarmApi/monitor/status` | Monitor loop status |
| POST | `/alarmApi/monitor/check-rule/{id}` | Manually evaluate one rule |
| POST | `/alarmApi/call-data` | Fetch data on behalf of a rule check |
| GET, POST | `/api/admin/logs/level` | Read / set runtime log level |
| GET | `/api/admin/logs/files`, `/api/admin/logs/view` | List / view log files |

### aether-uplink (port 6006)

Routes from `services/uplink/src/routes.rs`.

| Method | Path | Purpose |
|--------|------|---------|
| GET | `/`, `/ping`, `/netApi/health` | Service info / liveness / health |
| POST | `/netApi/alarm/broadcast` | Forward an alarm to the cloud |
| GET | `/netApi/alarm/config` | Alarm forwarding configuration |
| GET, POST | `/netApi/mqtt/config` | Read / update the MQTT link configuration |
| GET | `/netApi/mqtt/status` | MQTT connection status |
| POST | `/netApi/mqtt/disconnect`, `/netApi/mqtt/reconnect` | Drop / re-establish the MQTT link |
| POST | `/netApi/certificate/upload` | Upload a TLS certificate |
| GET | `/netApi/certificate/info` | Certificate details |
| DELETE | `/netApi/certificate/{cert_type}` | Delete a certificate |
| POST | `/netApi/inst-sync` | Push instance definitions to the cloud |
| GET, POST | `/api/admin/logs/level` | Read / set runtime log level |
| GET | `/api/admin/logs/files`, `/api/admin/logs/view` | List / view log files |

## WebSocket

The gateway exposes one WebSocket endpoint, `GET /ws`
(`services/api/src/main.rs`, handlers in
`services/api/src/ws.rs`). The JWT middleware covers it; pass the
token as `?token=...` since browsers cannot set headers on the upgrade
request.

After connecting, a client sends JSON messages:

- `{"type": "subscribe", "data": {"source": "...", "channels": [...],
  "data_types": ["T"], "interval": 1000}}` — subscribe to value updates. The
  hub answers with `subscribe_ack`. Special sources: `rule` (streams a rule's
  latest execution result from local SQLite `rule_history`) and
  `homepage` (streams the configured homepage points).
- `{"type": "unsubscribe", ...}` — clear the subscription.
- `{"type": "ping"}` — answered with `pong`.

Delivery is event-assisted from the SHM PointWatch plane. A point-change hint
wakes affected subscriptions, which read authoritative values from SHM and
send a `data_batch`; a periodic `run_data_push` pass reconciles subscriptions
and provides recovery if an event is dropped. Redis is not in this path.

Control commands over the WebSocket are rejected: writes must go through
automation's `POST /api/instances/{id}/action` or io's channel APIs.

## Related pages

- [Configuration Reference](configuration.md) — the config that defines channels, instances, and rules
- [System Architecture](../concepts/architecture.md) — service topology and ports
- [Getting Started](../guides/getting-started.md) — bring the stack up and call the API
- [Connect Devices](../guides/connect-devices.md) — channel and point setup end to end
