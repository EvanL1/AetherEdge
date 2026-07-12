# AetherEMS Dockerfile for multi-architecture builds
# Uses pre-compiled binaries from cargo-zigbuild for fast builds
# No compilation happens in Docker - just packaging the pre-built binaries

# Build argument for target triple (set by build script)
ARG TARGET_TRIPLE=aarch64-unknown-linux-musl
ARG RUNTIME_MANIFEST_PATH=config.template/runtime-manifest.json

FROM alpine:3.19

# Install only essential runtime dependencies
RUN apk add --no-cache \
    ca-certificates \
    tzdata \
    docker-cli

# Set working directory
WORKDIR /app

# Preserve the repository license in every published runtime image.
COPY LICENSE /usr/share/licenses/aetherems/LICENSE
COPY NOTICE /usr/share/licenses/aetherems/NOTICE
LABEL org.opencontainers.image.licenses="MIT OR Apache-2.0"

# Copy pre-compiled binaries (built with cargo-zigbuild)
# These are already built by the build script before Docker runs
ARG TARGET_TRIPLE
COPY target/${TARGET_TRIPLE}/release/aether-io         /usr/local/bin/aether-io
COPY target/${TARGET_TRIPLE}/release/aether-automation /usr/local/bin/aether-automation
COPY target/${TARGET_TRIPLE}/release/aether-alarm      /usr/local/bin/aether-alarm
COPY target/${TARGET_TRIPLE}/release/aether-api        /usr/local/bin/aether-api
COPY target/${TARGET_TRIPLE}/release/aether-history    /usr/local/bin/aether-history
COPY target/${TARGET_TRIPLE}/release/aether-uplink     /usr/local/bin/aether-uplink

# Make binaries executable
RUN chmod +x /usr/local/bin/*

# Copy default configuration from template
# This provides a working default configuration out-of-the-box
COPY config.template/ /app/config/
ARG RUNTIME_MANIFEST_PATH
COPY ${RUNTIME_MANIFEST_PATH} /app/config/runtime-manifest.json

# Create all necessary directories with proper permissions
RUN mkdir -p data logs && \
    mkdir -p logs/channels logs/models && \
    mkdir -p logs/aether-io logs/aether-automation logs/aether-alarm \
      logs/aether-api logs/aether-history logs/aether-uplink && \
    chmod -R 775 config data logs

# Default environment variables
ENV RUST_LOG=info

# Health check
HEALTHCHECK --interval=30s --timeout=3s --start-period=5s --retries=3 \
    CMD ["/bin/sh", "-c", "kill -0 1"]

# Default to aether-io, but can be overridden in docker-compose
CMD ["aether-io"]
