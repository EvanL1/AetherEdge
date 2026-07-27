# aether-auth-jwt

Shared kernel implementation for fail-closed access-JWT verification at
application-facing service boundaries.

It accepts only HS256 access tokens issued with the configured server secret,
validates bounded claims and expiry, and maps the signed role to the fixed
Aether application permission set. It does not authorize capabilities by
itself; application policy still enforces permission, confirmation, revision,
idempotency, and audit requirements.

This is internal kernel infrastructure rather than an extension or a supported
standalone authentication product.

```bash
cargo test -p aether-auth-jwt
```
