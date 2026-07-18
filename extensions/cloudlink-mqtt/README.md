# aether-cloudlink-mqtt

Broker-neutral MQTT v3.1.1/QoS 1 binding for the experimental CloudLink edge
foundation. It validates a user-selected endpoint, TLS/authentication settings,
topic prefix and gateway namespace; publishes with `retain = false`; subscribes
only to the same gateway's session/ACK/replay topics; correlates QoS 1 PUBACK;
and reconnects independently of local edge behavior.

PUBACK is transport evidence only. The dedicated CloudLink spool is removed only
by a validated application durable ACK.

The adapter carries already-authenticated CloudLink bytes; it never invents a
signature. Gateway-signed composition uses the session Gateway key for
heartbeats and all durable uplinks. Persisted delivery facts keep their
original timestamps and business identity across restart, while a newer
session signs them again with its new session binding. The explicit trusted
connector test profile depends on broker-side attestation and normally omits
payload authentication.

The experimental `aether.cloudlink.integration.v1alpha1` extension adds
`up/integration/topology` and `up/integration/observations`. It remains disabled
until a compatible Cloud consumer is enabled first and the Runtime Manifest
explicitly declares the extension. Topology and observations use independent
durable streams. Topology is atomic; observation batches may split only between
complete observations. Each complete MQTT payload is limited to 256 KiB, and
both routes reuse application-level durable ACK and replay. Neither route
provides physical control.

The separate crate feature `integration-control` exposes the exact
experimental offer and receipt topic namespace plus explicitly activated
transport methods. Those routes remain absent from every baseline connection.
`aether-io` activates them only after accepting the current CloudLink session
and persisting its new epoch; reconnecting resets them to disabled until the
new session repeats that activation. Receipt PUBACK is transport evidence and
never removes the durable receipt. Signature verification, topology and local
policy, audit, deduplication, and runtime settings remain in the composition
and application layers rather than this MQTT adapter.

Default tests need no broker. See `docs/reference/cloudlink-mqtt-v1.md` for the
opt-in shared-broker harness and environment variables.
