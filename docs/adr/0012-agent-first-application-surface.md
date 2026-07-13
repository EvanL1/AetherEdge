# ADR-0012: Make contracts and Agent Skills the application surface

## Status

Accepted on 2026-07-13.

## Context

AetherIot is an AI-native, industry-neutral edge kernel. The former Web Console was built for an
energy-management product and could not remain a generic Kernel interface without reintroducing
domain ownership, a frontend build chain, and a second path for configuration behavior.

A fixed generic Console also cannot know the vocabulary, units, layouts, and workflows of every
Domain Pack. AI coding agents can build fit-for-purpose clients when they receive exact runtime
contracts, current documentation, and a safe live tool surface. Guidance alone, however, cannot be
a security or data-authority boundary in an IoT system with physical side effects.

## Decision

1. AetherIot remains headless and does not publish a generic Web Console as part of the Kernel
   distribution.
2. The canonical application surface is the versioned command/query application API plus runtime
   manifests, capability metadata, OpenAPI, MCP, and agent-readable Markdown documentation.
3. A repository-owned `aether-iot` Agent Skill teaches clients how to discover those contracts and
   build SDK integrations, generated applications, and site-specific UIs. The Skill stays small and
   routes agents to version-matched online or local documentation.
4. Generated applications and reference UIs are untrusted downstream clients. They never become
   authorities for live state, desired configuration, domain truth, authorization, confirmation,
   or audit.
5. Only `aether-api` is a supported remote application boundary. Missing public capabilities are
   implemented through the application layer; clients must not expose or proxy internal process
   APIs, attach to SHM, or write storage as a workaround.
6. Applications start read-only. Commands are added explicitly and remain subject to server-side
   risk, permission, confirmation, idempotency, revision, and audit policy regardless of the UI.
7. Domain Packs or downstream products own presentation semantics and domain workflows. AetherEMS
   may maintain an optional energy reference application without making it a Kernel dependency.
8. Reference applications are verified against published contracts but do not define those
   contracts. Replacing or regenerating a reference application must not change runtime behavior.

## Consequences

### Positive

- AetherIot stays industry-neutral, external-service-free, and free of a required frontend toolchain.
- AI agents receive one documented development method and machine-verifiable runtime facts.
- Downstream products can generate the smallest interface their operators need instead of forking a
  generic Console.
- Server-side safety remains effective even when generated client code is wrong or incomplete.
- UI evolution no longer couples Kernel releases to a specific framework.

### Negative

- AetherIot does not offer an all-in-one dashboard immediately after installation.
- Public application queries must be completed before every internal capability is available to a
  remote generated client.
- Domain Packs need explicit presentation metadata or downstream requirements for high-quality UI
  generation.
- Reference applications require contract fixtures and regeneration discipline to remain useful.

## Follow-up acceptance criteria

- The public docs publish the generated-application method through HTML, Markdown twins,
  `llms.txt`, and `llms-full.txt`.
- The `aether-iot` Skill validates under the Agent Skills format and can be installed from the
  repository.
- No AetherIot release artifact contains a required browser application or frontend service.
- Remote application examples use only authenticated `aether-api` capabilities.
- Pack presentation metadata receives its own versioned schema before clients depend on it.
