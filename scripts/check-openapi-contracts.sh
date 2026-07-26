#!/usr/bin/env bash

set -euo pipefail

# Service-local OpenAPI is feature-gated so the normal workspace test graph
# does not execute these Router/OpenAPI parity contracts. Keep the six
# service-owned documents honest whenever routes, schemas, security, or
# responses move; only aether-api owns the Swagger UI.
readonly SERVICES=(
    aether-io
    aether-automation
    aether-history
    aether-api
    aether-uplink
    aether-alarm
)

for service in "${SERVICES[@]}"; do
    # I/O and automation keep their OpenAPI contracts in library modules.
    # The remaining services are binary-only packages, for which `--lib`
    # makes Cargo fail before it can run the contract tests.
    if [[ "$service" == "aether-io" || "$service" == "aether-automation" ]]; then
        cargo test -p "$service" --features openapi --lib --bins openapi
    else
        cargo test -p "$service" --features openapi --bins openapi
    fi
done

# Compile the gateway-owned multi-document UI and its path-rewrite contracts.
cargo test -p aether-api --features swagger-ui --bins openapi

echo "Gateway Swagger/OpenAPI contracts passed"
