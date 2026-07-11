# Dependency security exceptions

Security advisories are denied by default. An exception is permitted only when
the affected operation is unreachable, the boundary is enforced in code and a
removal condition is recorded here.

## RUSTSEC-2023-0071 (`rsa`)

- **Introduced by:** optional `async-opcua` support in `aether-io`.
- **Risk:** the upstream RSA implementation may leak private-key information
  through a timing side channel.
- **Runtime boundary:** `aether-io` accepts only anonymous OPC UA sessions with
  `SecurityPolicy::None` and `MessageSecurityMode::None`. Signing, encryption,
  username/password authentication and sample keypair generation are rejected
  before an OPC UA client is constructed.
- **Default exposure:** none. The `opcua` Cargo feature is disabled by default.
- **Compensating control:** use a trusted, isolated field network or a separately
  audited OPC UA bridge whenever transport security or authentication is needed.
- **Removal condition:** remove the `cargo-deny` exception and the temporary
  runtime restriction once `async-opcua` no longer resolves to an affected
  `rsa` release; then add authenticated and encrypted integration tests before
  re-enabling those modes.
- **Review owner:** maintainers; review on every `async-opcua` update and at
  least once per release.

This exception preserves protocol interoperability without claiming that the
affected cryptographic operations are safe.
