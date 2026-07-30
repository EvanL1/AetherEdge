---
title: Gateway identity recovery
description: Contain a lost or compromised Gateway credential and re-establish an experimental CloudLink session without weakening Edge authority.
---

# Gateway identity recovery

Use this runbook when a Gateway signing key, broker credential, credential
generation, or trusted Cloud key can no longer be used safely.

AetherEdge now implements a local file-backed enrollment client and a
`claimed` public-key-binding state. The matching production AetherCloud Claim
endpoint, active credential issuance, hardware-backed key custody, and key
rotation are not yet production capabilities. Recovery therefore still
requires the operator and the AetherCloud credential authority; static
documentation cannot mint or authorize a replacement identity.

The current Linux baseline stores identity material under
`<data-directory>/uplink/identity` with a `0700` directory and `0600` files.
That is ordinary file protection, not TPM, Secure Enclave, system-keyring, HSM,
or hardware-backed custody.

## Contain

1. Disable the optional CloudLink and governed-control composition. Local
   acquisition, deterministic rules, and commissioned safety behavior must
   continue without Cloud.
2. Revoke the affected broker and cloud-side credential through their owning
   systems. Do not place replacement secrets in configuration files, logs,
   visible or untrusted prompts, or an agent transcript. An Enrollment Token
   may enter only through `aether cloud enroll`'s hidden terminal prompt or an
   explicit `--token-stdin` secret pipe; a private key must never enter any
   prompt.
3. Preserve the Gateway ID, credential ID and generation, last accepted session
   epoch, challenge-ledger evidence, CloudLink spool, audit records, and
   timestamps needed for investigation.
4. If private-key compromise is possible, do not reuse the key or merely
   restart the old session.

The enrollment store deliberately refuses to overwrite a different or already
claimed identity. Do not work around that guard by deleting or editing files
before the Cloud-side identity and credential have been revoked and the
investigation evidence has been preserved.

## Re-establish trust

1. Revoke the Cloud-side public-key binding and every derived credential. Agree
   on the replacement Gateway identity and generation before changing local
   state.
2. Preserve a protected forensic copy of the old identity metadata and audit
   evidence. Never copy the private seed into a ticket, log, shell history, or
   agent transcript.
3. Re-establish the local identity only through an approved recovery procedure
   and the real enrollment application path. The current CLI intentionally has
   no force-reset or force-overwrite option.
4. Provision a new supervisor-managed CloudLink secret reference and, when
   required, a new key and credential generation.
5. Update only the explicit identity and secret-reference settings. Never infer
   a provider or identity from the shape of a credential.
6. Verify the broker principal and topic authorization for the exact Gateway
   identity before enabling the connector.
7. Start one connector instance with exclusive ownership of its session-epoch,
   challenge-ledger, and spool files.
8. Confirm a fresh challenge and signed Gateway hello, a strictly newer
   accepted session epoch, and successful signed heartbeat or durable uplink
   evidence.
9. Keep governed control disabled until read-only telemetry, audit delivery,
   credential generation, and Edge-local policy have all been checked.

The current `session-accepted`, heartbeat acknowledgement, and application
acknowledgement still have documented authentication gaps. A successful
experimental reconnection is not production identity proof.

See the [CloudLink MQTT reference](../reference/cloudlink-mqtt-v1.md),
[Gateway enrollment reference](../reference/gateway-enrollment.md),
[configuration reference](../reference/configuration.md), and
[CloudLink spool recovery](cloudlink-spool-recovery.md).
