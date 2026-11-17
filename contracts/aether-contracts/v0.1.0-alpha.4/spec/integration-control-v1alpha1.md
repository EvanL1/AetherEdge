---
id: integration-control-v1alpha1
status: experimental-default-off
version: 0.1.0-alpha.4
normative: true
---

# Integration Control v1 alpha 1

The Integration Control extension defines one deny-by-default governed action:
`device.power.set.v1`. It is a separate extension named
`aether.cloudlink.integration-control.v1alpha1`. It does not change the
read-only `aether.cloudlink.integration.v1alpha1` topology and observation
extension, and base CloudLink 1.0 negotiation never enables control.

This release intentionally does not define a generic action, Home Assistant
domain, service name, service-data object, provider URL, token, credential
reference, script, template, or arbitrary JSON argument. A producer cannot use
the fixed action as a container for another provider operation.

## Activation and rollout

The extension is disabled by default. An action offer is permitted only when
all of the following are true:

1. The edge Runtime Manifest explicitly declares
   `aether.cloudlink.integration-control.v1alpha1`.
2. The edge consumer, persistent job ledger, local Integration connection, and
   local safety policy have been commissioned.
3. Cloud has observed that exact Runtime Manifest protocol and explicitly
   enables its producer.
4. The offer is bound to the current authenticated CloudLink session.

Rollout is edge-first. Cloud must never probe support by publishing an action.
An unsupported or disabled edge receives no action message. Disabling the
extension prevents new execution but does not erase the audit or replay ledger
for previously accepted jobs.

## Fixed intent

`IntegrationActionIntent` is a closed object with Schema
`aether.integration-control.action-intent.v1alpha1`.

The only `capability_id` is `device.power.set.v1`. Its target is exactly:

- `integration_id`;
- the accepted `snapshot_generation`;
- the stable `entity_id`;
- `point_key`, fixed to `is_on`.

Its arguments object contains only one Boolean member, `value`. The target
resolves through the complete accepted Integration topology. The generation
must match exactly, the entity must exist, the point must be a Boolean status
point, and the Home Assistant profile permits only `fan`, `light`, and
`switch` entity kinds.

The current provider source address is deliberately absent. Edge resolves it
from the stable entity identity only after checking the exact local topology
generation. A provider rename therefore cannot redirect a stale Cloud offer.

Governance metadata is fixed rather than caller-selected:

| Field | Required value |
| --- | --- |
| execution | `governed-job` |
| default authorization | `deny` |
| permission | `integration.device.control` |
| risk | `high` |
| confirmation | `required` |
| idempotency | `required` |
| expiry | `required` |
| audit | `required` |
| edge final decision | `true` |

The signed intent also carries bounded authorization and confirmation
references. These references do not override local policy. Edge verifies the
Cloud authority, explicit confirmation, commissioned Integration, target, and
local safety decision before any provider invocation.

## Action offer

`integration-action-offer.schema.json` is a closed Cloud-to-edge message. It
binds the Gateway, current session, credential generation, job identity,
issue time, mandatory expiry, intent digest, complete intent, and Cloud
authentication.

`intent_digest` is lowercase SHA-256 over RFC 8785 JCS UTF-8 bytes of the
complete intent object, encoded as `sha256:<64 lowercase hex digits>`.
`cloud_authentication` is Ed25519 over the exact ordered field projection in
`profiles/cloudlink/v1alpha1/integration-control.json`. Presence of a
well-shaped signature is not verification; the edge must validate a configured
Cloud key. Production Cloud key provisioning, rotation, revocation, and
verifier ownership remain planned, so this profile remains experimental.

The job replay identity is `(gateway_id, job_id)`. The first durably accepted
offer binds that identity to one `intent_digest`. Repeating the same identity
and digest returns the stored receipt and must not invoke Home Assistant again.
Reusing the identity with another digest fails as `DIGEST_CONFLICT`. A timeout,
crash, or ambiguous provider response becomes `unknown`; it never justifies an
automatic second physical effect.

The offer is valid only while `evaluation_time_ms < expires_at_ms`.
An expiry earlier than the issue time fails as `INVALID_EXPIRY_WINDOW`; the
deadline or any later time fails as `MESSAGE_EXPIRED`.

## Home Assistant mapping

The edge adapter maps the Boolean semantic action internally:

- `true` selects its fixed provider power-on operation;
- `false` selects its fixed provider power-off operation.

Neither operation name nor any provider request object appears on the public
wire. The caller cannot select a Home Assistant domain, service, target syntax,
or service data. Provider URL and credentials remain in the edge-local secret
store.

An edge implementation must durably record the job identity and intent digest,
then record attempted audit evidence, before invoking the provider. A failed
pre-invocation audit or local policy check prevents invocation.

## Receipts and evidence

`integration-action-receipt.schema.json` is an authenticated edge-to-Cloud
delivery using the existing CloudLink uplink envelope, replay identity,
business digest, and durable-ACK rules. Its closed payload binds the job,
receipt sequence, capability, exact target, and intent digest.

The permitted evidence stages are:

- `edge-accepted`;
- `edge-rejected`;
- `provider-accepted`;
- `provider-rejected`;
- `unknown`.

Every receipt fixes `physical_outcome` to `unknown`. The contract has no
`physical-confirmed` or `succeeded` stage. In the Home Assistant profile, a
successful provider response proves only that Home Assistant accepted the
request. It does not prove that a radio packet reached a device, that an
actuator moved, or that the desired physical state now exists. A later state
observation is separate provider evidence and still is not independent
physical confirmation.

Cloud must therefore retain `provider-accepted`, observed state, physical
confirmation, and final job outcome as separate facts. It may not turn
`provider-accepted` into physical completion or successful physical execution.

When present, `failure_code` is a nonempty uppercase contractual identifier
with a maximum of 128 characters. Longer values are wire-invalid and fail as
`FIELD_BOUND`; a consumer must not accept an unbounded producer value and then
silently truncate it to its own parser limit.

## Conformance

The normative Schemas are under `schemas/integration-control/v1alpha1/` and
the two CloudLink entry Schemas are under
`schemas/cloudlink/v1alpha1/`. The fixtures and TCK freeze the only action,
closed argument surface, secret boundary, mandatory governance metadata,
default-off activation, exact topology binding, expiry, replay conflict,
receipt evidence stages, and Home Assistant provider-acceptance boundary.

This release does not claim a production Cloud key lifecycle, a product job
ledger, a Home Assistant command adapter, end-to-end Broker evidence, or
physical confirmation.
