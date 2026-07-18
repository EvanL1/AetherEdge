---
title: Home Assistant bridge
description: Local Home Assistant projection with an experimental default-off governed power binding
updated: 2026-07-17
---

# Home Assistant bridge

This optional AetherEdge extension treats a commissioned local Home Assistant
instance as a delegated device provider. Home Assistant remains authoritative
for the actual state of devices reached through this path. AetherCloud does
not connect to Home Assistant and does not receive its credentials.

## Current status

The source tree currently implements:

- authenticated WebSocket transport with bounded messages, queues, and
  timeouts;
- area, device, entity, and current-state snapshots;
- stable registry identities and mutable entity aliases;
- explicit, typed point mappings with an attribute allowlist;
- ordered state observations and complete-resynchronization signaling after
  stream gaps or registry changes;
- environment-backed secret references without inline token configuration;
- deterministic mock-server and delegated-provider conformance tests.

The default feature set produces a **read-only projection**. It does not call
Home Assistant services, expose arbitrary actions, write AetherEdge shared
memory, or prove that a physical device performed an action.

The optional crate feature `integration-control` adds an experimental,
default-off executor for the single semantic capability
`device.power.set.v1`. It accepts only Boolean `is_on` targets resolved from
the current generation-fenced projection, and only for `light`, `switch`, and
`fan` entities. The adapter derives the provider domain and maps the value to
the fixed `turn_on` or `turn_off` service. Callers cannot supply a Home
Assistant domain, service, service data, URL, or credential.

The crate feature itself is a library integration seam and introduces no
environment variable. `aether-io` composes it only through the separate
`home-assistant-integration-control` feature and only when Home Assistant,
read-only CloudLink publication, and control are all explicitly enabled. The
verified Runtime Manifest must declare both Integration protocol tokens.
Activation injects signature verification, exact current-topology resolution,
local commissioning/delegation policy, confirmation, persistent audit, a
durable ledger, and the fixed executor before any offer subscription. Home
Assistant acceptance records only its correlation context; physical
completion and job success remain unknown.

`aether-io` can compose the read-only extension in an opt-in source build using
the `home-assistant` feature and process-environment settings. The read-only
CloudLink and governed-control paths are additional, independently gated
source-build features. Prebuilt releases, installers, and Compose do not
enable them. There is no YAML section, `aether` command, HTTP/MCP integration
query, production OAuth flow, broad device-command surface, or production key
rotation/revocation. These remain release gates rather than hidden
configuration.

## Connection contract

The non-secret origin must be an HTTP or HTTPS root such as
`https://homeassistant.example.lan:8123`. The extension derives
`wss://homeassistant.example.lan:8123/api/websocket`. Credentials, query
strings, fragments, and non-root paths are rejected.

The built-in local resolver accepts references such as
`env:AETHER_HOME_ASSISTANT_TOKEN`; the token itself must remain outside normal
configuration. The current resolver is suitable for development and
controlled commissioning. Production OAuth authorization, refresh, secure
persistence, revocation, and rotation are not implemented.

The source-build composition reads:

```text
AETHER_HOME_ASSISTANT_ENABLED=true
AETHER_HOME_ASSISTANT_ORIGIN=https://homeassistant.example.lan:8123
AETHER_HOME_ASSISTANT_ACCESS_TOKEN_REF=env:AETHER_HOME_ASSISTANT_TOKEN
AETHER_HOME_ASSISTANT_TOKEN=<access-token>
AETHER_GATEWAY_ID=home-edge
AETHER_HOME_ASSISTANT_INTEGRATION_ID=home-assistant-main
AETHER_HOME_ASSISTANT_GENERATION_STORE_PATH=/absolute/path/to/home-assistant-topology-generations.json
```

The integration identity is optional and defaults to `home-assistant`. The
plaintext setting `AETHER_HOME_ASSISTANT_ACCESS_TOKEN` is forbidden. Build
`aether-io` with `--features home-assistant`; enabling the integration in a
binary without that feature fails closed. The generation-store setting is a
required absolute file path. It preserves only topology digest-to-generation
reservations across restart; the read-only projection remains process-local.
An unavailable, corrupt, or already locked store fails startup.

## Synchronization contract

The connection subscribes before fetching the registries and current states.
Events received during the snapshot are buffered within a fixed bound and
processed afterward. A disconnect, queue overflow, entity removal, registry
change, unknown entity, or sequence conflict stops incremental processing and
requires a complete new snapshot. Missed state is never fabricated.

See the complete [English user guide](../../docs/guides/home-assistant.md) or
the [Simplified Chinese user guide](https://docs.aetheriot.workers.dev/guides/home-assistant/)
for security guidance, mapped attributes, and troubleshooting.
