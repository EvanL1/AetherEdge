# aether-cloudlink-mqtt

Uplink-owned, broker-neutral MQTT v3.1.1/QoS 1 binding for the experimental
CloudLink edge foundation. It validates a user-selected endpoint, TLS/authentication settings,
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

This package is private implementation below `services/uplink`; it is not a
kernel extension or an IO dependency. Production session composition remains
gated by ADR-0017.

Default tests need no broker. See `docs/reference/cloudlink-mqtt-v1.md` for the
opt-in shared-broker harness and environment variables.
