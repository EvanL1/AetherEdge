---
title: Connect Home Assistant
description: Use Home Assistant as a delegated device source and explicitly opt in to tightly governed semantic power control
updated: 2026-07-17
---

# Connect Home Assistant

The Home Assistant bridge lets AetherEdge reuse the device coverage of an
existing Home Assistant installation without sending Home Assistant
credentials to AetherCloud. It runs beside the edge runtime and converts Home
Assistant areas, devices, entities, and states into a vendor-neutral,
read-only integration projection.

> **Current status:** this is an experimental, opt-in integration for source
> builds. `aether-io` can compose it when built with the `home-assistant`
> feature and explicitly enabled through environment settings. A second,
> default-off `home-assistant-cloudlink` feature can publish the committed
> read-only projection through two durable CloudLink streams. A third
> default-off `home-assistant-integration-control` feature implements one
> experimental governed `is_on` capability for lights, switches, and fans.
> Prebuilt releases and installers do not enable these paths, and there is no
> public CLI/HTTP/MCP query surface or production OAuth/key lifecycle yet.

## Where the bridge fits

```text
devices and vendor services
            |
            v
local Home Assistant instance
            |
            | local WebSocket connection
            v
AetherEdge Home Assistant bridge
            |
            v
read-only delegated-device projection
            |
            | optional, durable and Cloud-first
            v
AetherCloud Integration consumer
```

Home Assistant remains authoritative for devices reached through this path.
The bridge does not copy Home Assistant state into AetherEdge's authoritative
shared-memory point plane, and AetherCloud never connects to Home Assistant or
stores its token. Native AetherEdge acquisition, rules, safety interlocks, and
device control continue independently if Home Assistant is unavailable.

This is an adapter, not a new device protocol. Home Assistant continues to own
its Matter, Zigbee, Bluetooth, local-network, and vendor-cloud integrations;
AetherEdge consumes the normalized result.

## Current delivery boundary

| Available in the source tree | Not shipped yet |
|---|---|
| Authenticated Home Assistant WebSocket connection | Published support commitment and version compatibility baseline |
| Opt-in source composition plus explicit default-off Compose settings and persistent paths | Prebuilt binaries with these features and installer-managed enrollment, keys, and broker ACLs |
| Process-local, in-memory read-only projection | Durable projection or public query surface |
| Restart-stable topology digest-to-generation ledger | Installer-managed state path and backup policy |
| Area, device, entity, and current-state snapshot | Public CLI, HTTP, MCP, or generated-app query surface |
| Stable registry identity plus mutable entity alias | Arbitrary Home Assistant service calls |
| Ordered `state_changed` observations | General governed capability discovery |
| Typed primary state and selected bounded attributes | OAuth authorization, refresh, revocation, and rotation |
| Explicit full resynchronization after a stream gap or registry change | Production secret-manager adapter |
| Bounded messages, collections, queues, and timeouts | Installer-managed CloudLink enrollment and broker ACL templates |
| Default-off CloudLink publication through independent topology and observation file spools | Production OAuth and published support commitment |
| MQTT reconnect, retained replay, transport publication evidence, and application-ACK removal | General release enablement of the Integration extension |
| Default-off, session-bound `device.power.set.v1` offers with persistent idempotency, audit, and receipts | General-purpose device capabilities or arbitrary service calls |
| Fixed `light`/`switch`/`fan` `turn_on` and `turn_off` mapping | Proof of physical completion; provider acceptance remains physically `unknown` |
| Mock-server, composition, and provider-conformance tests | Floors, labels, configuration entries, and complete service metadata |
| `unknown` and `unavailable` quality without fabricated values | Reverse exposure of native AetherEdge devices to Home Assistant |
| | Published Home Assistant version compatibility matrix and opt-in real-instance test |

The integration does not use `io.yaml`, and `aether sync` does not activate
it. Current source builds use explicit process-environment settings.

## Enable an opt-in source build

After completing [Getting Started](getting-started.md) for a configured source
checkout, build and start `aether-io` with the optional adapter:

```bash
export AETHER_HOME_ASSISTANT_ENABLED=true
export AETHER_HOME_ASSISTANT_ORIGIN='https://homeassistant.example.lan:8123'
export AETHER_HOME_ASSISTANT_ACCESS_TOKEN_REF='env:AETHER_HOME_ASSISTANT_TOKEN'
export AETHER_HOME_ASSISTANT_TOKEN='<access-token>'
export AETHER_GATEWAY_ID='home-edge'
export AETHER_HOME_ASSISTANT_INTEGRATION_ID='home-assistant-main'
export AETHER_HOME_ASSISTANT_GENERATION_STORE_PATH="$PWD/data/home-assistant-topology-generations.json"

cargo run -p aether-io --features home-assistant
```

`AETHER_HOME_ASSISTANT_INTEGRATION_ID` is optional and defaults to
`home-assistant`. The other settings shown above are required when the
integration is enabled. The generation-store path must resolve to an absolute
file path; the example uses the absolute current checkout path. An absent
enable switch means disabled; the accepted explicit values are `true`, `1`,
`false`, and `0`.

The process fails closed when Home Assistant is enabled in a binary built
without the `home-assistant` feature, when a required setting is missing, or
when the origin or secret reference is invalid. The plaintext setting
`AETHER_HOME_ASSISTANT_ACCESS_TOKEN` is deliberately forbidden. Store the
token only in the environment variable named by
`AETHER_HOME_ASSISTANT_ACCESS_TOKEN_REF`.

After startup, the integration maintains an in-memory projection inside
`aether-io`. It repopulates that projection from a complete snapshot after a
restart. The local generation ledger records only the mapping from topology
digest to monotonically increasing generation; it does not persist device
state, Home Assistant credentials, or the projection. A repeated digest keeps
the same generation across restart, while a changed digest reserves the next
generation before publication.

Only one process may open a ledger at a time. Opening a corrupt, unavailable,
or already locked ledger fails startup. Keep the file on durable local
storage, do not edit it while `aether-io` is running, and include it in the
site's operational backup policy. There is no supported external read API for
the projection yet, so this opt-in mode is useful for integration development
and real-instance validation, not as an end-user automation surface.

## Optional CloudLink publication

CloudLink publication is a separate opt-in. Enabling the Home Assistant source
alone never advertises or starts it. The `aether-io` artifact must be built with
`home-assistant-cloudlink`, its verified `runtime-manifest.json` must declare
`aether.cloudlink.integration.v1alpha1`, and a compatible cloud consumer must
be enabled first.

Generate a source-build Runtime Manifest for the same artifact:

```bash
mkdir -p "$PWD/data/runtime"
cargo run -p aether-runtime-catalog --bin aether-runtime-manifest -- \
  generate \
  --output "$PWD/data/runtime/runtime-manifest.json" \
  --target aarch64-unknown-linux-musl \
  --io-features home-assistant-cloudlink
```

Then supply explicit durable paths and authenticated TLS MQTT settings:

```bash
export AETHER_GATEWAY_ID='33333333-3333-4333-8333-333333333333'
export AETHER_HOME_ASSISTANT_CLOUDLINK_ENABLED=true
export AETHER_HOME_ASSISTANT_CLOUDLINK_ORIGIN_MODEL='gateway-signed'
export AETHER_HOME_ASSISTANT_CLOUDLINK_CLOUD_KEY_ID='cloud-session-key-1'
export AETHER_HOME_ASSISTANT_CLOUDLINK_CLOUD_PUBLIC_KEY_REF='env:AETHER_CLOUDLINK_CLOUD_PUBLIC_KEY'
export AETHER_CLOUDLINK_CLOUD_PUBLIC_KEY='<unpadded-base64url-32-byte-ed25519-public-key>'
export AETHER_HOME_ASSISTANT_CLOUDLINK_GATEWAY_KEY_ID='edge-session-key-1'
export AETHER_HOME_ASSISTANT_CLOUDLINK_GATEWAY_SIGNING_KEY_REF='env:AETHER_CLOUDLINK_GATEWAY_SIGNING_KEY'
export AETHER_CLOUDLINK_GATEWAY_SIGNING_KEY='<unpadded-base64url-32-byte-ed25519-private-seed>'
export AETHER_HOME_ASSISTANT_CLOUDLINK_CHALLENGE_LEDGER_PATH="$PWD/data/ha-challenges.json"
export AETHER_HOME_ASSISTANT_CLOUDLINK_RUNTIME_CONFIG_DIR="$PWD/data/runtime"
export AETHER_HOME_ASSISTANT_CLOUDLINK_CLOUD_EXTENSION='aether.cloudlink.integration.v1alpha1'
export AETHER_HOME_ASSISTANT_CLOUDLINK_TOPOLOGY_SPOOL_PATH="$PWD/data/ha-topology.spool"
export AETHER_HOME_ASSISTANT_CLOUDLINK_OBSERVATION_SPOOL_PATH="$PWD/data/ha-observations.spool"
export AETHER_HOME_ASSISTANT_CLOUDLINK_SPOOL_CAPACITY=4096
export AETHER_HOME_ASSISTANT_CLOUDLINK_MQTT_BROKER_HOST='broker.example.net'
export AETHER_HOME_ASSISTANT_CLOUDLINK_MQTT_BROKER_PORT=8883
export AETHER_HOME_ASSISTANT_CLOUDLINK_MQTT_CLIENT_ID='aether-edge-home'
export AETHER_HOME_ASSISTANT_CLOUDLINK_MQTT_TOPIC_PREFIX='aether'
export AETHER_HOME_ASSISTANT_CLOUDLINK_MQTT_USERNAME='edge-home'
export AETHER_HOME_ASSISTANT_CLOUDLINK_MQTT_PASSWORD_REF='env:AETHER_CLOUDLINK_MQTT_PASSWORD'
export AETHER_CLOUDLINK_MQTT_PASSWORD='<broker-password>'
export AETHER_HOME_ASSISTANT_CLOUDLINK_CREDENTIAL_ID='edge-home-connector'
export AETHER_HOME_ASSISTANT_CLOUDLINK_CREDENTIAL_GENERATION=1
export AETHER_HOME_ASSISTANT_CLOUDLINK_SESSION_EPOCH_PATH="$PWD/data/ha-session-epoch"

cargo run -p aether-io --features home-assistant-cloudlink
```

This composition uses platform TLS roots and requires broker username/password
authentication. The broker and cloud consumer must enforce an exact
per-gateway topic ACL. The cloud issues a bounded, signed session challenge;
the edge verifies it, signs the session hello, and commits the accepted
session epoch locally before offering any retained fact. Challenge, hello,
heartbeat, topology, observation, runtime-manifest, telemetry, data-loss, and
control-receipt signing all use the configured Gateway session key identity.

`trusted-connector-broker-attestation` remains available only to explicit
development and cross-repository test harnesses. It relies on authentication
outside the payload and therefore omits `message_authentication`; the runtime
does not manufacture a placeholder signature.

Topology and observations use different crash-recoverable file journals. A
successful MQTT publish acknowledgement records transport progress only. A
record remains in its journal until a strict, current-session CloudLink
application acknowledgement is received. On restart, pending records are
replayed with the same message kind, original send and expiry times, stream
identity, batch ID, business digest, and payload. A retry in one session is
byte-identical. A newer session keeps those durable facts unchanged and signs
them again with the new session ID and monotonically higher epoch. If the
process stops after
the local projection commit but before an observation journal append, startup
fetches a complete Home Assistant snapshot and resends the current state; the
topology stream treats the already-retained generation idempotently.

CloudLink startup fails before spawning the integration when the feature,
verified Runtime Manifest declaration, cloud-side confirmation, credential
reference, TLS MQTT configuration, or either absolute spool path is missing.
Cloud unavailability never stops commissioned native edge acquisition, rules,
safety interlocks, or control.

## Experimental governed power control

Governed control is a separate third opt-in. It does not become active merely
because Home Assistant or read-only CloudLink publication is enabled. The same
runtime must explicitly set all three switches:

```bash
export AETHER_HOME_ASSISTANT_ENABLED=true
export AETHER_HOME_ASSISTANT_CLOUDLINK_ENABLED=true
export AETHER_HOME_ASSISTANT_CONTROL_ENABLED=true
```

Build the binary with `home-assistant-integration-control` and generate the
Runtime Manifest with that exact IO feature. Feature normalization records both
the read-only Integration and Integration Control protocol tokens:

```bash
cargo run -p aether-runtime-catalog --bin aether-runtime-manifest -- \
  generate \
  --output "$PWD/data/runtime/runtime-manifest.json" \
  --target aarch64-unknown-linux-musl \
  --io-features home-assistant-integration-control

cargo run -p aether-io --features home-assistant-integration-control
```

The verified manifest must declare both
`aether.cloudlink.integration.v1alpha1` and
`aether.cloudlink.integration-control.v1alpha1`. The edge refuses startup if
either declaration or any trust/persistence input is missing:

```bash
export AETHER_HOME_ASSISTANT_CONTROL_CLOUD_EXTENSION='aether.cloudlink.integration-control.v1alpha1'
export AETHER_HOME_ASSISTANT_CONTROL_LEDGER_PATH="$PWD/data/control/jobs-and-receipts.json"
export AETHER_HOME_ASSISTANT_CONTROL_POLICY_PATH="$PWD/data/control/policy.json"
export AETHER_HOME_ASSISTANT_CONTROL_AUDIT_PATH="$PWD/data/control/audit.jsonl"

export AETHER_HOME_ASSISTANT_CONTROL_CLOUD_KEY_ID='cloud-control-key-1'
export AETHER_HOME_ASSISTANT_CONTROL_CLOUD_PUBLIC_KEY_REF='env:AETHER_CONTROL_CLOUD_PUBLIC_KEY'
export AETHER_CONTROL_CLOUD_PUBLIC_KEY='<unpadded-base64url-32-byte-ed25519-public-key>'

export AETHER_HOME_ASSISTANT_CONTROL_PROVIDER_TIMEOUT_MS=5000
```

The optional timeout defaults to 5000 milliseconds and is bounded from 1 to
30000. Key material must be canonical unpadded Base64url. Cloud offers use the
separate configured cloud verification key. Receipt uplinks reuse the active
CloudLink Gateway session signer and must carry that exact key identity; there
is no second production receipt key. Rotation, revocation, hardware-backed
storage, and production enrollment remain planned.

The deprecated `AETHER_HOME_ASSISTANT_CONTROL_EDGE_KEY_ID` and
`AETHER_HOME_ASSISTANT_CONTROL_EDGE_SIGNING_KEY_REF` names should be omitted.
In Gateway-signed mode they are accepted only as a complete, exact alias of
the CloudLink Gateway key identity and reference. The explicit trusted
connector test harness may still use both names for an independent legacy
receipt signer so its existing cross-repository control test remains
verifiable; that key is test-only and is never represented as session
authentication.

The required local policy is a closed, deny-by-default JSON document. Every
entity must be independently commissioned, delegated, and allowed for the
confirmed subject:

```json
{
  "schema": "aether.edge.integration-control-policy.v1",
  "gateway_id": "33333333-3333-4333-8333-333333333333",
  "integration_id": "home-assistant.home",
  "permission": "integration.device.control",
  "commissioned_entities": ["entity-registry-light-bedroom"],
  "delegated_entities": ["entity-registry-light-bedroom"],
  "allowed_subjects": ["user-homeowner"]
}
```

Use a mode-0700 real directory for the policy, ledger, audit, and sibling lock
files. On Unix, existing ledger, audit, and lock files are rejected when group
or other permission bits are present; newly created sensitive files use mode
0600. Symbolic-link files and direct parent directories are rejected. On
non-Unix platforms, regular-file, non-symbolic-link, process-lock, and
exclusive-replacement checks remain active, but Unix permission bits are not
portable.

The control subscription is not part of the baseline MQTT subscriptions. It
is installed only after the current CloudLink session has been accepted and
its new session epoch has been persisted. Each offer is then bound to the
current gateway, session ID, session epoch, and credential generation;
verified with the configured cloud Ed25519 key; resolved against the exact
current topology generation; and evaluated by the local policy before a
provider call.

The public contract cannot carry a Home Assistant domain, service,
`service_data`, URL, token, or provider entity address. The edge resolves the
stable entity registry ID locally and maps only Boolean `is_on` for `light`,
`switch`, and `fan` to fixed `turn_on` or `turn_off` calls. Provider acceptance
creates a receipt but does not prove physical completion or job success; the
physical outcome remains `unknown`.

The job ledger is opened before the offer subscription. A job is durably
claimed before the provider boundary, and the same `(gateway_id, job_id,
intent_digest)` never invokes Home Assistant twice. An interrupted claim is
converted to an `unknown` receipt after restart without retrying the provider.
Receipts retain their message kind, original send time, stream position, batch
ID, business digest, and payload across disconnect and restart. A newer
session signs those immutable facts again with the same Gateway session key.
MQTT PUBACK never removes them; only an exact current-session CloudLink
application durable ACK can do so.

Gateway heartbeats and every durable Gateway uplink sign the frozen 13-field
RFC 8785 projection. Missing delivery fields are represented as JSON `null`.
`session-accepted`, heartbeat acknowledgements, and durable application
acknowledgements remain unsigned because the current alpha profile defines no
signing projection for them. The decoder rejects a heartbeat acknowledgement
that adds `message_authentication`. The unsigned acceptance and
acknowledgement boundary remains a documented blocker to a
production-readiness claim.

## Connection values

The connection accepts a Home Assistant **origin**, not a full API URL.

| Setting | Example | Rule |
|---|---|---|
| Origin | `https://homeassistant.example.lan:8123` | HTTP or HTTPS root only; no username, password, path, query, or fragment |
| Derived WebSocket endpoint | `wss://homeassistant.example.lan:8123/api/websocket` | Derived automatically from the origin |
| Token reference | `env:AETHER_HOME_ASSISTANT_TOKEN` | Reference only; never place the token itself in configuration |
| Topology generation ledger | `/var/lib/aether/home-assistant-topology-generations.json` | Required absolute local file path; must be writable and exclusively lockable by `aether-io` |

An origin beginning with `http://` becomes
`ws://<host>/api/websocket`. Use it only on an isolated, trusted local
commissioning network. For normal operation, terminate TLS correctly and use
an `https://` origin so the WebSocket connection uses `wss://`.

The current local resolver accepts only `env:` references. Variable names
must start with an uppercase letter or underscore and may contain only
uppercase letters, digits, and underscores:

```bash
export AETHER_HOME_ASSISTANT_TOKEN='<access-token>'
```

Give this variable only to the process that hosts the bridge. Do not commit it
to YAML, an environment template, a shell script, a container image, logs, or
an agent prompt. Production deployments should inject it from a protected
process environment or secret store and restrict access to the process
identity.

## Authentication boundary

Home Assistant authenticates WebSocket clients with an access token. Its
[authentication documentation](https://developers.home-assistant.io/docs/auth_api/)
supports an OAuth-based authorization flow with short-lived access tokens and
refresh tokens, and also documents manually created long-lived access tokens.

The current AetherEdge bridge resolves one access token and does not implement
authorization redirects, refresh-token storage, automatic refresh, or
revocation. Therefore:

- use a manually created long-lived token only for local development and
  controlled commissioning;
- create a dedicated Home Assistant user where practical and grant only the
  access required to read the registries and state stream;
- treat the token as a high-value secret, rotate it after suspected exposure,
  and remove it from Home Assistant when the bridge is decommissioned;
- do not expose an unauthenticated or plaintext Home Assistant endpoint to an
  untrusted network;
- do not call the current token resolver a production OAuth integration.

A production release requires an operator-facing authorization flow,
refresh-token lifecycle, secure persistence, revocation, and compatibility
tests before the manual-token path can be replaced.

## First snapshot

On a new connection, the bridge performs these operations in order:

1. open the Home Assistant WebSocket endpoint and complete authentication;
2. enable bounded message coalescing and subscribe to events;
3. fetch the area, device, and entity registries;
4. fetch the current state collection;
5. publish one internally consistent topology-and-state snapshot;
6. apply buffered and subsequent state changes in order.

Subscribing before the snapshot prevents changes during registry reads from
being silently missed. Events received while snapshot commands are in flight
remain bounded and are processed after the snapshot.

The `light`, `switch`, and `fan` domains expose their primary on/off state as
the stable Boolean `is_on` point. Every other domain keeps the primary `state`
point. This is an explicit public mapping: no compatibility alias silently
duplicates the same Home Assistant state. Known domains receive a more precise
type, while unknown domains remain bounded strings instead of being discarded.
Only selected attributes are projected:

| Home Assistant domain | Additional projected attributes |
|---|---|
| `light` | `brightness`, `color_temp_kelvin` |
| `climate` | `current_temperature`, `temperature`, `current_humidity`, `hvac_action` |
| `cover` | `current_position`, `current_tilt_position` |
| `fan` | `percentage`, `preset_mode` |
| `vacuum` | `battery_level` |
| `media_player` | `volume_level`, `is_volume_muted` |
| `event` | `event_type` |

Other attributes are not forwarded merely because Home Assistant returned
them. This keeps secrets, large payloads, and high-cardinality vendor data out
of the projection by default. Media content such as the current title is also
private by default and is not included without a future explicit privacy and
egress policy.

If Home Assistant returns an invalid value for one of these declared points,
the bridge emits that point with `unknown` quality and no value. Other valid
points in the same snapshot or event continue normally, and a later valid
event can restore `good` quality. Explicit provider `unknown` and
`unavailable` states keep their existing meanings. This tolerance applies only
to point values: identity conflicts, topology changes, stream gaps, and queue
overflow still fail closed and require a complete resynchronization.

## Disconnection and full resynchronization

Home Assistant does not provide a durable event cursor for this connection.
After a disconnect, queue overflow, entity removal, registry update, unknown
entity, sequence gap, or topology conflict, incremental processing must stop.
The caller must obtain a complete new snapshot before accepting more
observations.

The bridge never invents missed states and never repairs a gap by applying a
later event to an older topology. Until a full snapshot succeeds, consumers
must treat the projection as stale. A Home Assistant outage does not stop
commissioned native AetherEdge behavior, but delegated Home Assistant state
cannot be considered current during the outage.

## Troubleshooting

| Symptom | Check |
|---|---|
| Origin is rejected immediately | Supply only an `http://` or `https://` root; remove `/api`, `/api/websocket`, credentials, query strings, and fragments |
| Secret reference is rejected | Use `env:VARIABLE_NAME` with a portable uppercase variable name |
| Credential is unavailable | Confirm the environment variable exists in the bridge process, not only in the login shell |
| Authentication is rejected | Recreate or rotate the token, confirm the Home Assistant user is active, and verify the entire token was copied |
| Registry request is unauthorized | Verify the dedicated user can read area, device, and entity registry data; do not bypass the error by embedding an administrator token in source |
| TLS connection fails | Use a certificate trusted by the edge host and ensure the hostname in the origin matches it |
| State updates stop after reconnect | Trigger a complete snapshot; do not resume from the previous in-memory sequence |
| A new entity or area does not appear | A registry change requires a complete snapshot rather than an incremental state update |
| An attribute is missing | Only the documented attribute allowlist is projected in the current slice |
| Startup says the feature is not compiled | Rebuild `aether-io` with `--features home-assistant`; prebuilt releases do not currently include it |
| CloudLink startup rejects the Runtime Manifest | Build the same artifact with `home-assistant-cloudlink` and generate a manifest that explicitly lists that feature |
| CloudLink records remain on disk | Confirm the cloud consumer sends an application durable ACK; MQTT PUBACK is deliberately insufficient |
| CloudLink session never establishes | Verify TLS trust, broker credentials, per-gateway ACLs, and that the cloud consumer sends challenge and acceptance messages |
| Control refuses startup | Confirm all three enable switches, both Runtime Manifest protocol tokens, both Ed25519 key references, and distinct absolute ledger, policy, and audit paths |
| A control offer is rejected | Check the current session fence, signature key ID, expiry, exact topology generation, commissioned/delegated entity lists, and confirmed subject |
| A receipt is resent after MQTT PUBACK | Expected; only an exact current-session CloudLink application durable ACK removes it |
| No `aether` command can enable or query the bridge | Expected for the current opt-in source build; activation uses process settings and the projection has no public query surface yet |

For the upstream wire protocol, see the official
[Home Assistant WebSocket API](https://developers.home-assistant.io/docs/api/websocket/).
For AetherEdge command safety, see [Safe Operations](safe-operations.md).
