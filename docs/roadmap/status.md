# Platform status and roadmap

Status is reported separately for implemented, experimental, and planned
capabilities. Product names do not upgrade technical readiness.

## AI-native end-user experience

**Implemented foundations:** agent-readable Markdown and indexes, an Agent
Skill, runtime capability discovery, OpenAPI, governed application commands,
AetherEdge MCP tools/resources, deterministic local rules, audit evidence, and
the public contract/conformance repositories.

**Experimental or partial:** AetherCloud's transport-neutral MCP application
interface, desired/reported/applied deployment, governed jobs, CloudLink,
telemetry persistence, and Edge/Cloud development harnesses.

**Planned:** the household or site semantic context, conversational end-user
agent, typed intent/proposal/policy contracts, intent-to-automation compiler,
historical simulation, generated confirmation experience, temporary behavior
expiry, outcome evaluation, and continuous governed adaptation. No current
release should present these as a complete product.

## AetherEdge

**Implemented:** six-service runtime, SHM live-state authority, embedded local
operation, governed commands, `aether` CLI, `aether-edge-sdk`, Pack v1, MCP and
OpenAPI foundations, signed `v0.0.1` source/runtime/CLI/SDK artifacts, and a
local Gateway enrollment client. The enrollment slice has process-level
evidence for key generation, durable pending state, a strict local HTTP Claim,
and the resulting `claimed` identity state.

The IO source tree also contains opt-in BACnet/IP, CJ/T 188,
IEC 60870-5-101, GB/T 32960, and JT/T 808 adapter slices alongside the existing
DL/T 645 implementation. COMTRADE CFG/DAT inspection and CSV normalization are
offline CLI functions rather than an IO protocol. Across these slices there is
codec and factory evidence, plus loopback TCP evidence for the two inbound
terminal servers; device-vendor
interoperability and field commissioning remain deployment-specific evidence,
not a repository-wide certification claim. See the
[Protocol Adapter Reference](../reference/protocol-adapters.md).

**Experimental:** the Uplink-owned CloudLink MQTT v1 foundation,
application-ACK-driven spool, AetherContracts alpha.3 consumption, and
real-Broker development evidence. The Edge real-Broker harness now requires a
Cloud-signed challenge and a Gateway-signed hello; legacy direct hello is
rejected. Legacy MQTT remains the runtime default.

**Planned or gated:** deployment of the matching AetherCloud production Claim
endpoint, verifiable credential issuance and activation, Cloud trust-key
delivery and rotation, production `aether-uplink` identity composition,
production CloudLink key lifecycle, signed ACK, complete joint conformance,
legacy cutover, History query ownership, and remaining application-boundary
migration. Home Assistant and SunSpec implementations are out-of-tree
downstream work, not kernel roadmap capabilities.

The local `claimed` enrollment state means only that a tested Claim server
acknowledged the submitted public-key fingerprint. It is not
`credential-active`, `cloudlink-connected`, or `online`, and no production
AetherCloud pairing is claimed.

**CloudLink alpha.4 production blocker:** the current `session-accepted`
message is unsigned and carries neither `challenge_id` nor `client_nonce`.
The Edge-only test harness sequences acceptance after a verified challenge and
requires the persisted session epoch to increase strictly, but the production
Uplink runtime does not yet compose this session path or bind disconnect to
handshake cancellation. Even those test checks cannot cryptographically exclude
a delayed acceptance from another handshake. The public wire contract must bind
an authenticated acceptance to the complete current handshake transcript before
this path can be described as production authentication.

The current AetherCloud dual harness is also blocked before that point: its
worker still composes the legacy direct-session application path and does not
inject challenge issuance or Gateway-signed session acceptance. AetherEdge
keeps strict verification enabled and times out rather than downgrading.

## AetherCloud

**Implemented foundations:** modular-monolith domain/application slices,
capability-driven providers, Plan-only OpenTofu, Gateway enrollment
domain/application foundations, partial CloudLink/telemetry persistence,
artifact/deployment/job foundations, audit and integration slices,
observability, and a transport-neutral MCP interface.

**Experimental or partial:** MQTT codec and ingress, local/AWS IoT harnesses,
PostgreSQL accepted-telemetry ACK outbox, and finite audit interfaces.

**Planned or gated:** the matching production Gateway Claim endpoint,
production identity and credential issuance, complete CloudLink durability and
mapping, production composition and workers, public job/deployment delivery,
hardened outbound integrations, and a connectable MCP server.

## AetherContracts

**Implemented, experimental:** alpha.3 specifications, closed Schemas, fixtures,
TCK, digest-pinned consumer verification, and four fixture bindings.

**Planned or gated:** production authentication key lifecycle, signed durable
ACK, complete production codecs, and a production CloudLink cutover release.

## Platform documentation

**Implemented in this migration:** shared product overview, unified navigation,
deployment topologies, user journeys, end-to-end alpha integration task, compatibility
matrix, status page, and AetherIot to AetherEdge migration guide.

**Planned:** automated
cross-repository version aggregation, release-channel status feeds, and a
future GitHub organization when an appropriate address is available.
