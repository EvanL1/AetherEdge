# Build a downstream integration

1. Identify the smallest published Aether port that expresses the capability.
2. If no port fits, prove an industry-neutral need with behavior tests before
   changing the contract. Do not add vendor methods or DTOs to core crates.
3. Implement the adapter in the owning downstream repository. AetherEdge has no
   in-tree extension directory and does not vendor optional integration code.
4. Use `aether-edge-sdk` as the supported facade and the matching
   `aether-testkit` conformance suite.
5. Compose the adapter in a downstream Rust binary. Do not load scripts,
   dynamic libraries, or child-process plugins into `aether-io`.
6. Keep credentials, external clients, retry policy, and failure isolation in
   the downstream adapter. It must not write SHM or kernel storage directly.
7. Declare every query or command capability, permission, risk, confirmation,
   idempotency, and audit requirement before exposing it remotely.
8. Pin an AetherEdge release and verify the runtime capability catalog rather
   than assuming a static build has authority.
