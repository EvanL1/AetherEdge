# aether-cloudlink-mqtt

Broker-neutral MQTT v3.1.1/QoS 1 binding for the experimental CloudLink edge
foundation. It validates a user-selected endpoint, TLS/authentication settings,
topic prefix and gateway namespace; publishes with `retain = false`; subscribes
only to the same gateway's session/ACK/replay topics; correlates QoS 1 PUBACK;
and reconnects independently of local edge behavior.

PUBACK is transport evidence only. The dedicated CloudLink spool is removed only
by a validated application durable ACK.

Default tests need no broker. See `docs/reference/cloudlink-mqtt-v1.md` for the
opt-in shared-broker harness and environment variables.
