# Typical user journeys

Choose the shortest path that matches your role. All write paths remain governed
and deny by default.

## Evaluate a local edge runtime

1. Open the [AetherEdge overview](../aetheredge/index.md).
2. Start the safe-empty composition with no devices or external services.
3. Inspect runtime health and the machine-readable manifest.
4. Add a protocol adapter and domain Pack only when the application requires it.

## Build an edge application

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
