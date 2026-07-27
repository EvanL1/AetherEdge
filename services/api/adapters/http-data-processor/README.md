# Aether HTTP Data Processor

Optional, bounded HTTP implementation of the `DataProcessor` port. It sends a
complete request frame to `/v1/process`; it exposes no callback into Aether,
SHM, history, or configuration.

Plain HTTP is accepted only for a local processor on localhost or a loopback
address. Remote routes require HTTPS. Request and response sizes and both
connection and request timeouts are mandatory configuration.

## Composition

```rust
use std::time::Duration;

use aether_http_data_processor::{
    BearerSecret, HttpDataProcessor, HttpDataProcessorConfig,
};
use aether_ports::{DataBoundary, DataProcessorDescriptor};

# fn build(
#     descriptor: DataProcessorDescriptor,
#     deployment_token: String,
# ) -> Result<HttpDataProcessor, aether_ports::PortError> {
let config = HttpDataProcessorConfig::new(
    "https://processor.example.net",
    descriptor,
    Duration::from_secs(2),
    Duration::from_secs(10),
    4 * 1024 * 1024,
)?
.with_bearer_secret(BearerSecret::new(deployment_token)?);
let processor = HttpDataProcessor::new(config)?;
# assert_eq!(processor.descriptor().data_boundary(), DataBoundary::Remote);
# Ok(processor)
# }
```

Only the composition root selects the origin and optional secret. The adapter
derives the fixed versioned routes:

- `POST /v1/process`
- `GET /v1/health`

The request and successful processing response use
`application/vnd.aether.data-processing+json;version=1`. Health accepts a
small JSON response containing `status`, `processor`, `version`, and
`contract`, and verifies those identity fields against the configured
descriptor.

## Hard boundaries

- Request JSON is encoded by `aether-data-processing` and checked against the
  descriptor's `max_frame_samples` and `max_request_bytes` before any network
  call.
- Responses are bounded while streaming. `Content-Length` is an early guard,
  not the authority for the limit.
- Redirects and ambient proxy discovery are disabled.
- Remote routes require HTTPS. Local HTTP requires an explicit `Local`
  descriptor and a loopback or `localhost` origin.
- URL credentials, query strings, fragments, non-origin paths, zero limits,
  and zero timeouts fail configuration with `Permanent`.
- Bearer tokens have no environment-loading API, are marked sensitive in HTTP
  headers, and are redacted from all adapter `Debug` output.
- Remote response bodies, URLs, and transport internals are never copied into
  port errors.

Every 4xx or 5xx response must use the same versioned media type and the
closed `aether.data-processing.error.v1` envelope. The body is size-bounded
before decoding. Unknown fields, explicit nulls, malformed JSON, a mismatched
HTTP status/category, an invalid or mismatched request ID, and inconsistent
retry metadata fail closed as `InvalidData`.

Validated failures map to stable `PortErrorKind` values: deadline errors to
`Timeout`, a validated HTTP conflict response to `Conflict`, retryable
capacity/unavailability to `Unavailable`, invalid frames to `InvalidData`,
request/resource rejection
to `Rejected`, and non-retryable authorization, lookup, internal, or
unavailable failures to `Permanent`. Only the validated stable code, category,
retryability, retry delay, and request ID enter the port diagnostic; the
processor's free-form message and details are never copied. A response with
`status: unavailable` is still a valid `ProcessingResult`, not a transport
error.

The `Conflict` mapping does not create request-replay semantics. The public
`data_processing.process` operation is non-idempotent and has no built-in
de-duplication or request-ID reuse guarantee.

## Verification

```bash
cargo test -p aether-http-data-processor
cargo clippy -p aether-http-data-processor --all-targets -- -D warnings
cargo fmt --package aether-http-data-processor -- --check
```
