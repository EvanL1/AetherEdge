# aether-integration-contract

Strict product-side Rust binding for the experimental AetherContracts
`aether.integration` v1alpha1 contract in the `0.1.0-alpha.4` candidate.

The crate is transport-neutral. It provides:

- closed snake_case topology and observation DTOs;
- strict JSON decoding and deterministic RFC 8785 encoding;
- topology-dependent identity, reference, quality, and value-type validation;
- lossless string encodings for `int64` and `uint64`;
- canonical decimal and unpadded Base64url validation;
- Aether Foundation binary64 safety semantics;
- explicit Home Assistant source-address and semantic point-kind projection.

The Edge binding deliberately applies a 16 MiB complete-message safety limit,
which is stricter than the contract's portable field limits. The bundled tests
pin every official Integration fixture file by the SHA-256 values in the
`0.1.0-alpha.4` fixture manifest.

This crate does not define a CloudLink wrapper or a physical-control path.

```bash
cargo test -p aether-integration-contract
```
