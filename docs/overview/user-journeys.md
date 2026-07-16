# Typical user journeys

Choose the shortest path that matches your role. All write paths remain governed
and deny by default.

## Describe a physical-space outcome to an agent

The complete end-user conversation lifecycle remains product direction. The
available beta foundation supports a narrower, explicit workflow:

1. Connect an agent to the documentation and the default read-only MCP surface.
2. Ask it to inspect the runtime, active Pack, live capabilities, and current
   state before proposing a change.
3. Review which parts of the request map to implemented queries or governed
   commands and which parts are still unavailable.
4. Enable a bounded write session only for a specific task, preserve its
   confirmation and audit evidence, and return to read-only afterward.
5. Ask the agent to explain the observed outcome or reverse the versioned
   change rather than editing SHM, SQLite, or internal services.

Future releases will add typed intent/proposal contracts, simulation, temporary
behavior with expiry, and continuous outcome evaluation. See the
[AI-native platform](ai-native-platform.md) for that target lifecycle.

## Evaluate a local edge runtime

1. Open the [AetherEdge overview](../aetheredge/index.md).
2. Start the safe-empty composition with no devices or external services.
3. Inspect runtime health and the machine-readable manifest.
4. Add a protocol adapter and domain Pack only when the application requires it.

## Build an agent-generated edge application

1. Generate clients from the running AetherEdge OpenAPI contract.
2. Start read-only and preserve quality, freshness, topology generation, and
   revision fields.
3. Use the authenticated application boundary; never write SHM or SQLite
   directly.
4. Add governed commands only with explicit permission, confirmation,
   idempotency, and audit behavior.

## Connect an edge fleet to cloud

1. Select a tested combination in the
   [compatibility matrix](../compatibility/version-matrix.md).
2. Verify the digest-pinned AetherContracts consumer lock in both products.
3. Follow the [Edge to Contracts to Cloud tutorial](../tutorials/edge-contracts-cloud.md).
4. Keep CloudLink experimental and the legacy path available until every
   published release gate passes.

## Implement an independent client or runtime

1. Read the [AetherContracts overview](../aethercontracts/index.md).
2. Implement the normative specification and closed Schemas.
3. Execute the public fixtures and black-box TCK.
4. Report conformance evidence without claiming product deployment or
   production authentication.

## Adopt AetherEMS

Use AetherEMS when the desired outcome is an energy-management solution rather
than a general-purpose edge platform. AetherEMS supplies energy semantics and
workflows while the platform products keep their industry-neutral boundaries.
