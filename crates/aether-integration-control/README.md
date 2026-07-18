# aether-integration-control

Experimental, default-off AetherEdge binding for the frozen AetherContracts
`integration-control` v1alpha1 candidate in release `0.1.0-alpha.4`.

The crate provides:

- strict, closed action-offer and terminal-receipt codecs;
- RFC 8785 intent digests and exact signature projections;
- current-session, credential-generation, expiry, and topology-generation
  fences;
- edge-final commissioning, delegation, permission, confirmation, and audit
  ports;
- persistent same-job/same-digest deduplication with no automatic provider
  retry after timeout, crash, or unknown outcome;
- durable terminal-receipt replay through exact CloudLink ACK identities;
- one semantic capability, `device.power.set.v1`, targeting the Boolean
  `is_on` point of `light`, `switch`, or `fan` entities.

Provider adapters receive a closed semantic action. They cannot receive a
caller-selected provider domain, service, service data, URL, or credential.
Provider acceptance records correlation evidence only: physical completion and
job success remain unknown.

The crate has no runtime environment switch. `IntegrationControlConfig`
defaults to disabled, and activation requires an explicit composition with a
cloud-signature verifier, local authority, audit sink, persistent ledger, and
provider executor. The optional `integration-control` features in the Home
Assistant, CloudLink MQTT, and local-store extensions expose those adapter
seams. `aether-io` joins them only through its separately compiled,
default-off `home-assistant-integration-control` feature and three explicit
runtime switches.

```bash
cargo test -p aether-integration-control
cargo test -p aether-home-assistant-bridge --features integration-control
cargo test -p aether-store-local --features integration-control
```
