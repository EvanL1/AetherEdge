# AetherEdge

AetherEdge is the open-source, industry-neutral Linux edge runtime, Kernel, CLI,
and Rust SDK formerly published from the AetherIot repository name.

## Implemented today

- Six isolated runtime services for acquisition, automation, alarms, history,
  the application API, and uplink.
- Shared-memory authority for current point and health state.
- Embedded SQLite desired state, history, audit, and durable local outbox.
- The `aether` CLI, governed HTTP and MCP application boundaries, Domain Packs,
  and the `aether-edge-sdk` facade.
- A signed `v0.5.0` source, runtime, installer, and CLI release.

## Experimental today

- Broker-neutral CloudLink MQTT v1 sessions, telemetry, replay, and application
  acknowledgement spooling.
- Digest-pinned AetherContracts `v0.1.0-alpha.3` consumption and public fixture
  execution.

Experimental CloudLink evidence does not establish production authentication,
signed acknowledgement, or end-to-end crash durability. Legacy MQTT remains
the compatibility default.

## Stable compatibility names

The repository display name changes to AetherEdge. Existing crate names,
binary names, the `aether` CLI, `aether-edge-sdk`, configuration keys, service
identities, installer names, and protocol identifiers do not change in this
migration.

Start with the [Agent Quickstart](https://docs.aetheriot.workers.dev/en/agent-quickstart/),
[Getting Started](../guides/getting-started.md), or the
[migration guide](../migration/aetheriot-to-aetheredge.md).
