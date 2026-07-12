#!/usr/bin/env bash

set -euo pipefail

# Swagger UI is feature-gated, so the normal workspace test graph does not
# execute these Router/OpenAPI parity contracts. Keep the six service-owned
# documents honest whenever their routes, schemas, security, or responses move.
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
        cargo test -p "$service" --features swagger-ui --lib --bins openapi
    else
        cargo test -p "$service" --features swagger-ui --bins openapi
    fi
done

echo "Swagger/OpenAPI contracts passed"
