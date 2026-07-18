# AetherIoT platform overview

AetherIoT is the open-source, AI-native project identity for a family of
interoperable products that turn human intent into governed, verifiable
behavior across physical spaces. It is not a fourth runtime and does not own a
separate wire protocol.

```text
AetherIoT
├── AetherEdge       deterministic edge runtime, Kernel, CLI, and SDK
├── AetherCloud      evolving agent, fusion, and governed control plane
└── AetherContracts  typed specifications, Schemas, fixtures, and TCK

AetherEMS            energy-management solution built on the platform
```

## Product boundaries

| Product | Owns | Does not own |
| --- | --- | --- |
| AetherEdge | Live point state, acquisition, deterministic rules, safety interlocks, local history, and final physical execution | Cloud placement, provider resources, or public protocol authority |
| AetherCloud | Desired placement, governed cloud jobs, tenant control-plane state, and multi-cloud coordination | Edge live-state authority or provider-native actual state |
| AetherContracts | Language-neutral protocol semantics, closed Schemas, fixtures, stable failure classes, and executable conformance evidence | Product runtime behavior, credentials, cloud durability, or deployment policy |
| AetherEMS | Energy-domain models, workflows, and solution experience | The industry-neutral platform core |

The long-term product experience starts with conversation rather than a fixed
configuration UI: a user describes an outcome, an agent discovers available
capabilities, generates a governed proposal, and commissions deterministic edge
behavior. The complete end-user agent lifecycle is planned; the current beta
provides the runtime, application, documentation, MCP, contract, and cloud-side
foundations it requires. See the [AI-native platform](ai-native-platform.md)
for the exact boundary.

Every infrastructure provider remains authoritative for the actual existence
and provider-native state of its resources. Cloud failure must not stop a
commissioned AetherEdge runtime.

## Naming rules

- Use **AetherIoT** for the project, community, website, and complete platform.
- Use **AetherEdge** for the repository and product that were formerly named
  AetherIot.
- Keep existing `aether-*` crate names, the `aether` CLI, the
  `aether-edge-sdk` package, installer names, and protocol identifiers stable.
- Preserve historical release artifacts and digest-pinned AetherContracts
  bundles byte for byte. A new display name never rewrites old evidence.

Read the [AetherIot to AetherEdge migration guide](../migration/aetheriot-to-aetheredge.md)
for repository and automation changes.

## Documentation ownership

This site is the common entry point. Each product repository remains the source
of truth for its implementation details, while AetherContracts remains the sole
authority for shared protocol behavior. The unified pages link to those sources
instead of copying normative content into a second authority.

Continue with the [AI-native platform](ai-native-platform.md),
[deployment topologies](deployment-topologies.md),
[user journeys](user-journeys.md), or the
[Edge, Contracts, and Cloud integration task](../guides/edge-contracts-cloud.md).
