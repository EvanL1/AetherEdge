# aether-cloudlink

Transport-neutral implementation of the experimental, digest-pinned public
AetherContracts CloudLink subset. It provides strict closed JSON decoding, RFC
8785 business digests, session/version/epoch validation, stable delivery
envelopes, Runtime Manifest checksum reuse, and truthful `PointSample` mapping.

This crate contains no MQTT client and no device-control message. The matching
AetherCloud codec consumes the same imported fixtures, while three public
behavior artifacts and all production interoperability gates remain open. See
the [CloudLink MQTT reference](../../docs/reference/cloudlink-mqtt-v1.md) for
current behavior and production limits.

## Gateway-signed uplinks

Gateway-signed sessions use one Ed25519 Gateway key for the signed hello,
heartbeat, and every durable Runtime Manifest, telemetry, or data-loss uplink.
The signature covers the exact 13-field RFC 8785 projection frozen by
AetherContracts.

Durable records persist their original send and expiry times, message kind,
stream identity, batch ID, business digest, and payload. Retries in one session
reuse the exact bytes. After restart, a newer session keeps those facts
unchanged and signs them again with its new session ID and higher epoch.
Trusted-connector test mode relies on external broker attestation and omits
payload authentication.

`session-accepted`, heartbeat ACK, and durable ACK remain unsigned in the
current alpha profile. A heartbeat ACK that adds `message_authentication` is
rejected; the missing Cloud-to-Edge signing projections remain an explicit
production blocker.

```bash
cargo test -p aether-cloudlink
```
