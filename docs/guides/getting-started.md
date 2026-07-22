---
title: Getting Started
description: Build the workspace, initialize configuration, start services, and verify health
updated: 2026-07-22
---

# Getting Started

This guide takes you from a fresh clone to a running, uncommissioned Aether
system: build the `aether` CLI, apply a reviewed safe-empty setup plan, start
the services, and confirm that the runtime is healthy.

## Prerequisites

- **Rust** — the toolchain is pinned to `1.90.0` by `rust-toolchain.toml`;
  rustup installs it automatically on first build. The pin also declares the
  `aarch64-unknown-linux-musl` cross-compilation target used for edge builds.
- **Docker Engine and Docker Compose** — required for the container
  composition. `aether services start` drives Docker Compose under the hood.
  Redis and PostgreSQL are not prerequisites.

## Build and configure

Build the `aether` CLI:

```bash
cargo build --release -p aether
```

Install the binary onto your PATH — `cp target/release/aether /usr/local/bin/`
or `cargo install --path tools/aether` — so this and every other guide can
invoke it as bare `aether`.

The repository ships a fail-safe empty configuration in `config.template/`.
In a source checkout the CLI and `docker-compose.yml` both use
`./data/config` and `./data` by default. Planning is persistently read-only and
will not create either directory:

```bash
aether --json setup
```

Read `data.plan_id` from the JSON output, review the listed actions, then
explicitly apply that exact unchanged plan:

```bash
aether setup apply --plan-id <PLAN_ID>
```

Apply is accepted only for a fresh site or an exact safe subset of the four
distribution files. Before any persistent write, Aether stages the complete
configuration, runs normal validation and the full atomic sync against a
temporary SQLite database, then creates only missing files without
overwriting. It initializes `aether.db` and syncs the empty runtime, but does
not start a service, enable a device or rule, or install a domain pack. If the
site changed after planning, the plan ID is stale and apply stops without a
write. Rerunning setup on the resulting `safe_ready` site is a no-op.

Existing/custom sites are reported but never rewritten by setup. Operators
can still use `aether init` for an explicit schema migration and `aether sync`
for an explicit configuration apply; `aether sync --dry-run` validates the
same nested files without changing the installed database.

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
aether services start
aether doctor
```

`aether services start` brings up the Docker Compose stack. The compose file
references pre-built images; on a machine that does not yet have
`aetherems:latest`, produce it by running `./scripts/build-installer.sh`
(which builds the image from cross-compiled binaries) or load a prebuilt
image archive with `docker load` — see [Deployment](deployment.md).

`aether doctor` checks the required local runtime and exits nonzero if any
required component fails:

1. **Docker Engine** — the daemon is installed and running.
2. **Six core services** — IO, automation, history, API, uplink, and alarm
   answer their service-specific health routes. Optional cloud or storage
   dependencies may report degraded without becoming core failures.
3. **SQLite database** — `aether.db` exists, is initialized, and shows its
   last sync time.
4. **Config files** — `global.yaml`, `io/io.yaml`,
   `automation/automation.yaml`, and `automation/instances.yaml` are present.
5. **Shared memory** — the segment file `/dev/shm/aether-rtdb.shm` exists and
   has a readable, valid data-plane header and a fresh IO-writer heartbeat.
   Missing, stale, truncated, symlinked, or invalid SHM is an error because it
   is the authoritative live-state plane. `AETHER_SHM_PATH` overrides the
   platform default when an installation deliberately uses another location.

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
port 6005 (ADR-0021), so every `aether` data command needs an access token.
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

## First look around

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

- [Connect Devices](connect-devices.md) — add a real channel and map its
  points to instances
- [Writing Rules](writing-rules.md) — automate control with the rule engine
- [AI Assistants](ai-assistants.md) — drive Aether from an AI agent
- [Deployment](deployment.md) — Docker Compose details and the edge installer
