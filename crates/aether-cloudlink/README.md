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

```bash
cargo test -p aether-cloudlink
```
