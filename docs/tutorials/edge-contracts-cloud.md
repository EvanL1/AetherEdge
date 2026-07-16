# Connect AetherEdge to AetherCloud through AetherContracts

This tutorial proves the current cross-repository integration path without
claiming production CloudLink readiness. It starts with a safe local runtime,
verifies the shared contract release, and then runs the available product
evidence.

## 1. Select the compatible baseline

Use AetherEdge `v0.5.0`, AetherContracts `v0.1.0-alpha.3`, and an AetherCloud
revision that consumes the same complete contract lock. Confirm the exact
combination in the [version matrix](../compatibility/version-matrix.md).

Do not follow `main`, `latest`, a version range, or a sibling checkout for
contract behavior.

## 2. Start AetherEdge safely

Clone `EvanL1/AetherEdge`, then run the hardware-free SDK composition:

```bash
cargo run -p aether-example-minimal-gateway
```

This composition commissions no device and requires no Broker or cloud
service. For a supervised runtime installation, follow the
[Getting Started guide](../guides/getting-started.md).

## 3. Verify the public contract authority

In an AetherContracts `v0.1.0-alpha.3` checkout:

```bash
pnpm test:tck
```

Then inspect each product's committed `aether-contracts.lock.json`. Both locks
must name the same release tag, tag object, commit, bundle digest, manifest
digest, safety policy, exact imports, and empty pending-import set.

The verifier proves release distribution integrity. It does not prove a
production codec, authentication system, Broker deployment, or durable cloud
store.

## 4. Execute the edge contract evidence

In AetherEdge, run the focused transport-neutral codec tests:

```bash
cargo test -p aether-cloudlink
```

The test path validates strict input, canonical digests, replay behavior,
session fencing, and current telemetry mapping without contacting a Broker.

## 5. Execute the cloud contract evidence

In AetherCloud, run the default repository check:

```bash
pnpm check
```

The default path validates the strict TypeScript codec, application bridge,
memory and PostgreSQL adapter contracts, and documentation without requiring a
database, device, Broker, or cloud account.

An opt-in local dual-process Broker harness is available as development
evidence:

```bash
pnpm test:cloudlink-alpha-harness
```

MQTT PUBACK proves only Broker transport acceptance. AetherEdge may remove a
spooled record only after the exact Cloud application acknowledgement is
validated. The current alpha acknowledgement remains unsigned, and the full
production crash-durable gate has not passed.

## 6. Preserve the authority boundary

- Do not expose a direct point, register, SHM, or physical-control operation
  through CloudLink.
- Do not treat a reported capability as cloud authorization.
- Do not equate desired, reported, and applied state.
- Do not remove the legacy path until joint authentication, durability,
  conformance, rollback, and support-window gates pass.

The result of this tutorial is reproducible alpha integration evidence, not a
production commissioning receipt.
