# AetherEdge

AetherEdge is the open-source, industry-neutral Linux edge runtime, Kernel, CLI,
and Rust SDK in the AetherIoT product family.

## Implemented today

- Six isolated runtime services for acquisition, automation, alarms, history,
  the application API, and uplink.
- Shared-memory authority for current point and health state.
- Embedded SQLite desired state, history, audit, and durable local outbox.
- The `aether` CLI, governed HTTP and MCP application boundaries, Domain Packs,
  and the `aether-edge-sdk` facade.
- Statically composed field-protocol adapters whose exact feature/runtime-ID
  matrix and implementation boundaries are documented in the
  [Protocol Adapter Reference](../reference/protocol-adapters.md).
- A signed `v0.0.1` source, runtime, installer, CLI, and SDK release.

## Experimental today

- Broker-neutral CloudLink MQTT sessions, telemetry, replay, and application
  acknowledgement spooling.
- Digest-pinned AetherContracts `v0.1.0-alpha.3` consumption and public fixture
  execution.

Experimental CloudLink evidence does not establish production authentication,
signed acknowledgement, or end-to-end crash durability.

## Stable product identifiers

The canonical repository and product name is AetherEdge. Crate names, binary
names, the `aether` CLI, `aether-edge-sdk`, configuration keys, service
identities, installer names, and protocol identifiers remain the current
stable software identifiers.

Start by choosing the matching [user journey](../overview/user-journeys.md),
then follow [Getting Started](../guides/getting-started.md) for a safe-empty
runtime, [Connect Devices](../guides/connect-devices.md) to commission a
channel, or the [Agent Quickstart](https://docs.aetheriot.ai/agent-quickstart/)
for a read-only assistant workflow.
