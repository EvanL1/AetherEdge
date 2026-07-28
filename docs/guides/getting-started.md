---
title: Getting Started
description: Install a safe-empty runtime, establish operator access, verify health, and choose the next commissioning step
updated: 2026-07-26
---

# Getting Started

This guide takes an operator or source developer to the same first milestone: a
healthy, uncommissioned AetherEdge runtime with no device, rule, or domain
solution silently enabled.

If you want a ready-made energy-management product rather than an
industry-neutral runtime, start with
[AetherEMS](https://github.com/EvanL1/AetherEMS). The AetherEdge golden path is:

```text
safe-empty install -> operator identity -> disabled device channel
  -> point mapping -> read-only verification -> reviewed behavior
  -> explicit commissioning -> audit and operation
```

See [AetherEdge User Journeys](../overview/user-journeys.md) for the operator,
solution-builder, application, and AI variants.

## Install from a release

This is the normal path for a Linux edge operator. Download the matching `.run`
package and checksum from
[GitHub Releases](https://github.com/EvanL1/AetherEdge/releases), then verify and
run the fresh-install package:

```bash
sha256sum -c AetherEdge-<arch>-<version>.run.sha256
chmod +x AetherEdge-<arch>-<version>.run
sudo ./AetherEdge-<arch>-<version>.run
```

The installer creates the six-service runtime, `aether` CLI, private bootstrap
credentials, embedded database, and safe-empty configuration. It does not
commission a site. After installation, continue at
[Start and verify](#start-and-verify); do not repeat the source-checkout setup
below.

## Source checkout prerequisites

- **Rust** — the toolchain is pinned to `1.90.0` by `rust-toolchain.toml`;
  rustup installs it automatically on first build. The pin also declares the
  `aarch64-unknown-linux-musl` cross-compilation target used for edge builds.
- **Docker Engine and Docker Compose** — required for the container
  composition. Redis and PostgreSQL are not prerequisites.

## Prepare a source checkout

Use this path for AetherEdge development, SDK evaluation, or a manual Compose
installation—not as the normal operator installation flow.

Build the `aether` CLI:

```bash
cargo build --release -p aether
```

Install the binary onto your PATH — `cp target/release/aether /usr/local/bin/`
or `cargo install --path tools/aether` — so this and every other guide can
invoke it as bare `aether`.

The repository ships a fail-safe empty configuration in `config.template/`.
For a manual source deployment, copy it only into a new site directory, review
it, validate it, then apply it while runtime owners are stopped:

```bash
test ! -e data/config
mkdir -p data
cp -R config.template data/config
aether init
aether sync --dry-run
aether sync --confirmed
```

`sync --dry-run` is read-only. The confirmed apply atomically writes the
reviewed offline desired state but does not start a service, enable a device or
rule, or install a domain Pack. Packaged installers own safe-empty activation
and refuse non-empty fresh-install targets.

The CLI resolves each path independently in this order: command-line flag,
`AETHER_CONFIG_PATH`/`AETHER_DATA_PATH`, `/etc/aether/install.yaml`, then the
current checkout's `data/config/` and `data/`. Installed packages write the
context file automatically. Without that context, Aether never adopts an old
installation directory merely because it exists.

For a fresh manual Compose deployment, create a private environment file and
fill both first-start secrets before validating the composition. Packaged
installers do this automatically; repository setup deliberately keeps secrets
out of configuration templates.

```bash
cp .env.example .env
chmod 600 .env

random_hex_32() {
  if command -v openssl >/dev/null 2>&1; then
    openssl rand -hex 32
  else
    od -An -N32 -tx1 /dev/urandom | tr -d ' \n'
  fi
}
export JWT_SECRET_KEY="$(random_hex_32)"
export AETHER_BOOTSTRAP_ADMIN_PASSWORD="$(random_hex_32)"

env_tmp="$(mktemp ./.env.tmp.XXXXXX)"
chmod 600 "$env_tmp"
awk '
  /^JWT_SECRET_KEY=/ {
    print "JWT_SECRET_KEY=" ENVIRON["JWT_SECRET_KEY"]; next
  }
  /^AETHER_BOOTSTRAP_ADMIN_PASSWORD=/ {
    print "AETHER_BOOTSTRAP_ADMIN_PASSWORD=" ENVIRON["AETHER_BOOTSTRAP_ADMIN_PASSWORD"]; next
  }
  { print }
' .env > "$env_tmp"
mv "$env_tmp" .env

JWT_SECRET_KEY="$JWT_SECRET_KEY" \
  AETHER_BOOTSTRAP_ADMIN_PASSWORD="$AETHER_BOOTSTRAP_ADMIN_PASSWORD" \
  docker compose config --quiet
unset JWT_SECRET_KEY AETHER_BOOTSTRAP_ADMIN_PASSWORD
```

Keep `JWT_SECRET_KEY` stable. Sign in as `admin` with the generated bootstrap
value, change the password immediately, then remove
`AETHER_BOOTSTRAP_ADMIN_PASSWORD` from `.env`. Public registration stays off
because the example sets `AETHER_ALLOW_PUBLIC_REGISTRATION=false`.

## Start and verify

```bash
docker compose up -d
docker compose ps
curl --fail http://127.0.0.1:6005/health
curl --fail http://127.0.0.1:6001/health
```

The compose file references pre-built images; on a machine without
`aetherems:latest`, build the installer image or load a release archive as
described in [Deployment](deployment.md). Docker Compose or systemd owns the
six process states. The API health endpoint proves the remote boundary, while IO health verifies
the acquisition owner and authoritative live-state plane.

With everything healthy, these ports are listening (see
[System Architecture](../concepts/architecture.md) for what each service
does). Only the authenticated API gateway is remotely exposed by the packaged
composition; the other five process APIs listen on `127.0.0.1`:

| Service | Port |
|---------|------|
| aether-io | 6001 |
| aether-automation | 6002 |
| aether-history | 6004 |
| aether-api | 6005 |
| aether-uplink | 6006 |
| aether-alarm | 6007 |

AetherEdge intentionally exposes no bundled Web UI. Product consoles such as
AetherEMS are deployed independently and enter through `aether-api`.

## Get an operator token

The CLI data plane and MCP speak only to the authenticated API gateway on
port 6005, so every `aether` data command needs an access token.
Log in as the bootstrap admin and export the token for the shell session —
the login API expects the hex MD5 digest of the password, not the plaintext:

```bash
# The bootstrap value was unset from the shell above; read it back from .env
bootstrap_password="$(grep '^AETHER_BOOTSTRAP_ADMIN_PASSWORD=' .env | cut -d= -f2-)"
digest="$(printf '%s' "$bootstrap_password" | md5sum | cut -d' ' -f1)"
export AETHER_ACCESS_TOKEN="$(curl -s http://localhost:6005/api/v1/auth/login \
  -H 'content-type: application/json' \
  -d "{\"username\":\"admin\",\"password\":\"$digest\"}" | jq -r '.data.access_token')"
unset bootstrap_password digest
```

Tokens expire after 30 minutes by default; rerun the login when a command
reports 401. Day-to-day operation should use a dedicated account instead of
the bootstrap admin — see the auth endpoints in the
[HTTP API reference](../reference/http-api.md).

## Confirm the safe-empty state

The default template deliberately contains no device channel or instance, so
these commands should initially return empty collections:

```bash
# 1. The communication channels aether-io is polling
aether channels list

# 2. The device instances aether-automation is serving
aether models instances list

# 3. Confirm that no control rule was activated implicitly
aether rules list
```

Every command accepts `--json` for structured output, which is the mode AI
agents and scripts should use. Data starts flowing only after an explicit
commissioning step adds and enables a channel; continue with Connect Devices.

## Next steps

Your first production milestone should be a read-only acquisition path. Connect
one disabled channel, map it, verify quality and freshness, and only then review
rules or control.

- [AetherEdge User Journeys](../overview/user-journeys.md) — the complete safe lifecycle and role-specific paths
- [Connect Devices](connect-devices.md) — add a real channel and map its
  points to instances
- [Writing Rules](writing-rules.md) — automate control with the rule engine
- [AI Assistants](ai-assistants.md) — drive Aether from an AI agent
- [Deployment](deployment.md) — Docker Compose details and the edge installer
