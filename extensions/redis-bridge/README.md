# aether-redis-bridge

Optional, non-authoritative Redis mirror for Aether point state.

This extension implements the `StateMirror` capability. It is not enabled by
`aether-sdk`, is not part of the default edge runtime, and must never be used
as the control loop or live-state source of truth. Deploy it only when an
external integration explicitly needs a Redis-shaped projection.

```bash
cargo test -p aether-redis-bridge
```

Licensed under either MIT or Apache-2.0, at your option.
