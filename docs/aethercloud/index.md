# AetherCloud

AetherCloud is the AI-native, industry-neutral, multi-cloud IoT fusion and
control plane for AetherEdge runtimes and cloud-side workloads. It is a separate
product, not a hosted copy of the edge runtime.

## Implemented foundations

- Provider-neutral discovery and governed infrastructure planning through
  capability-driven adapters.
- A Plan-only OpenTofu infrastructure engine with lock and process safety
  evidence.
- Gateway identity and enrollment, CloudLink session and runtime-manifest
  application foundations.
- Partial PostgreSQL persistence for gateway and accepted telemetry facts,
  including durable acknowledgement outbox evidence.
- Partial artifact, deployment, governed job, audit, integration, observability,
  and transport-neutral MCP application slices.

## Still planned for production

Production identity and credential lifecycle, public CloudLink composition,
complete crash durability, multi-sample mapping, production database
composition, workers, hardened outbound delivery, and a connectable MCP server
remain planned or gated.

AetherCloud owns desired placement. A provider owns its actual resources.
AetherEdge remains authoritative for live point state and final physical
execution.

Read the [AetherCloud repository](https://github.com/EvanL1/AetherCloud), the
[platform overview](../overview/platform.md), and the
[status page](../roadmap/status.md).
