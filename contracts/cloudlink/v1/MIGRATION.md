# Candidate reconciliation and remaining work

Public AetherContracts alpha.3 replaces the incompatible repository-local experimental
vocabularies previously called the AetherIot candidate and
`1.0-cloud.1`. The public contract selects the AetherIot durable delivery,
digest, cursor, replay, and data-loss model; it adopts the AetherCloud session
proof requirement, canonical UUID identity, and 256-sample batch limit.

The freeze does not make either repository production-complete. AetherCloud
still needs durable production session, manifest, telemetry, receipt, replay,
and data-loss persistence plus a policy-owned mapping from a CloudLink batch
position to any internal record indexing. AetherIot still needs production
credential proof generation and runtime composition of the experimental
CloudLink path. The opt-in Mosquitto dual harness and fault matrix now execute
these exact files. Both sides still need production origin verification/key
lifecycle and Cloud PostgreSQL crash-durable ACK/outbox persistence.

The Cloud retention class and optional Thing Model reference are Cloud policy
and enrichment concerns. They are not mandatory Edge facts and are not
invented in the wire payload. Legacy MQTT topics remain deprecated and
isolated; no legacy write or call topic is mapped to CloudLink v1.
