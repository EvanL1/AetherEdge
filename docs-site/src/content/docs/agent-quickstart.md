---
title: Agent Quickstart
description: Install the AetherEdge Skill, start a safe-empty runtime, and connect an AI agent from zero.
---

This page is written for an AI agent driving a shell, not a human reading
prose. Each step states the command and the exact signal that means "this
step succeeded, move on."

## 1. Install the AetherEdge Skill

From the application repository where the agent will work:

```bash
npx skills add EvanL1/AetherEdge -s aether-iot
```

Restart the coding assistant if it does not reload project Skills automatically.

**Success criterion:** the assistant lists `aether-iot` as an available Skill.

## 2. Install the `aether` CLI

Building from a source checkout is the most direct development path:

```bash
cargo build --release -p aether
sudo cp target/release/aether /usr/local/bin/aether
```

See [Getting Started](/guides/getting-started/) for prerequisites if the
build fails.

Once a tagged release exists, a prebuilt binary is faster — download from
GitHub Releases, verify its checksum, and extract it. Pick the asset matching
your platform:

| Platform | Asset |
|---|---|
| Linux arm64 | `aether-linux-aarch64.tar.gz` |
| Linux x86_64 | `aether-linux-x86_64.tar.gz` |
| macOS arm64 | `aether-darwin-aarch64.tar.gz` |
| Windows x86_64 | `aether-windows-x86_64.zip` |

```bash
REPO="EvanL1/AetherEdge"
ASSET="aether-linux-x86_64.tar.gz"   # substitute your platform's asset name

TAG=$(curl -fsSL "https://api.github.com/repos/$REPO/releases/latest" \
  | grep -m1 '"tag_name"' | cut -d '"' -f4)

curl -fsSLO "https://github.com/$REPO/releases/download/$TAG/$ASSET"
curl -fsSLO "https://github.com/$REPO/releases/download/$TAG/$ASSET.sha256"
shasum -a 256 -c "$ASSET.sha256"

tar xzf "$ASSET"
chmod +x aether
sudo mv aether /usr/local/bin/aether
```

**Success criterion:** `aether --version` prints a version string and exits 0.

## 3. Plan and apply the first-run configuration

```bash
aether --json setup
```

Read `data.plan_id` from the JSON output. Then, without modifying anything
about the site in between, apply that exact plan:

```bash
aether setup apply --plan-id <PLAN_ID>
```

**Success criterion:** the apply command's JSON envelope has
`"success": true` and exit code 0. This never starts a service or enables a
device — it only creates the safe-empty configuration and local SQLite state.

## 4. Start the services

Aether's default deployment is Docker Compose. Generate the two required
first-start secrets, then bring the stack up:

```bash
cp .env.example .env
chmod 600 .env

export JWT_SECRET_KEY="$(openssl rand -hex 32)"
export AETHER_BOOTSTRAP_ADMIN_PASSWORD="$(openssl rand -hex 32)"
sed -i.bak \
  -e "s/^JWT_SECRET_KEY=.*/JWT_SECRET_KEY=${JWT_SECRET_KEY}/" \
  -e "s/^AETHER_BOOTSTRAP_ADMIN_PASSWORD=.*/AETHER_BOOTSTRAP_ADMIN_PASSWORD=${AETHER_BOOTSTRAP_ADMIN_PASSWORD}/" \
  .env && rm .env.bak
unset JWT_SECRET_KEY AETHER_BOOTSTRAP_ADMIN_PASSWORD

aether services start
```

**Success criterion:** `aether --json services status` reports all requested
services as running. See [Deployment](/guides/deployment/) if the
the compatibility `aetherems:latest` runtime image doesn't exist yet on this
machine — it needs to be built or loaded before `services start` can succeed.
The retained image name does not make the EMS product part of this repository.

## 5. Verify health

```bash
aether --json doctor
```

**Success criterion:** the envelope is `{"success": true, ...}` and the
process exits 0. `doctor` checks the Docker engine, all six core services'
health routes, the SQLite database, the four required config files, and the
shared-memory segment — a `false`/non-zero result means one of those failed;
read the JSON `error` field for which one.

## 6. Connect an MCP client

```bash
claude mcp add aether -- aether mcp
```

For a session that needs to issue writes (device control, rule changes) —
read [Safe Operations for Applications and Agents](/guides/safe-operations/)
before doing this against real hardware:

```bash
claude mcp add aether -- aether mcp --allow-write
```

**Success criterion:** the client's `tools/list` response includes
`channels_list`. The default server exposes only read tools. `--allow-write`
registers the current governed write allowlist, but the flag is not command
confirmation: every invocation still needs `confirmed: true`, the signed
`AETHER_ACCESS_TOKEN` is sent as a Bearer credential, and the bridge adds a
request ID. Never automatically retry an incomplete write response.
For a channel mutation, preserve `request_id`, `resulting_revision`, and
`reconciliation_required`; successful desired-state commit can still report a
degraded runtime projection.
See [Connect AI Assistants](/guides/ai-assistants/) for Claude Desktop config
and pointing at a remote installation.

Now ask the assistant:

```text
Get started with AetherEdge. Inspect the runtime in read-only mode and explain
which application capabilities are available before proposing any changes.
```
