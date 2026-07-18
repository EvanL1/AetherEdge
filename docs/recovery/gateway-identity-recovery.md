---
title: Gateway identity recovery
description: Contain a lost or compromised Gateway credential and re-establish an experimental CloudLink session without weakening Edge authority.
---

# Gateway identity recovery

Use this runbook when a Gateway signing key, broker credential, credential
generation, or trusted Cloud key can no longer be used safely.

Managed enrollment, hardware-backed key custody, certificate issuance, and key
rotation are not yet production AetherEdge capabilities. Recovery therefore
requires the operator and the AetherCloud credential authority; static
documentation cannot mint or authorize a replacement identity.

## Contain

1. Disable the optional CloudLink and governed-control composition. Local
   acquisition, deterministic rules, and commissioned safety behavior must
   continue without Cloud.
2. Revoke the affected broker and cloud-side credential through their owning
   systems. Do not place replacement secrets in configuration files, logs,
   prompts, or an agent transcript.
3. Preserve the Gateway ID, credential ID and generation, last accepted session
   epoch, challenge-ledger evidence, CloudLink spool, audit records, and
   timestamps needed for investigation.
4. If private-key compromise is possible, do not reuse the key or merely
   restart the old session.

## Re-establish trust

1. Provision a new supervisor-managed secret reference and, when required, a
   new key and credential generation.
2. Update only the explicit identity and secret-reference settings. Never infer
   a provider or identity from the shape of a credential.
3. Verify the broker principal and topic authorization for the exact Gateway
   identity before enabling the connector.
4. Start one connector instance with exclusive ownership of its session-epoch,
   challenge-ledger, and spool files.
5. Confirm a fresh challenge and signed Gateway hello, a strictly newer
   accepted session epoch, and successful signed heartbeat or durable uplink
   evidence.
6. Keep governed control disabled until read-only telemetry, audit delivery,
   credential generation, and Edge-local policy have all been checked.

The current `session-accepted`, heartbeat acknowledgement, and application
acknowledgement still have documented authentication gaps. A successful
experimental reconnection is not production identity proof.

See the [CloudLink MQTT reference](../reference/cloudlink-mqtt-v1.md),
[configuration reference](../reference/configuration.md), and
[CloudLink spool recovery](cloudlink-spool-recovery.md).
