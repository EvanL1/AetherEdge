# Bare-Metal Linux Install (No Docker) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Install and run the full AetherEMS stack (6 Rust services + Redis + the Vue web UI) on a Docker-free Linux target (arm64/amd64) via one self-contained `.run` package, supervised by systemd.

**Architecture:** A build machine (which still uses Docker, only for cross-compiling static `redis-server`/`nginx`) produces a `.run` package containing musl-static binaries, systemd unit files, and an install script. The target machine gets zero new system dependencies. The `aether` CLI gains a `DeployMode` (`Docker` | `Systemd`) auto-detected at runtime, so `aether services`/`aether doctor` work unmodified in either environment.

**Tech Stack:** Bash (build/install scripts), systemd units, Rust (CLI mode split), Alpine/musl Docker containers for static dependency builds (build-machine only).

**Spec:** `docs/superpowers/specs/2026-07-10-baremetal-install-design.md`

**Grounding note:** every file path, function name, and line-number reference below was verified against the current `main` branch before this plan was written (services.rs, doctor.rs, main.rs, build-installer.sh, install.sh, docker-compose.yml, apps/nginx.conf, apps/Dockerfile, Dockerfile, Cargo.toml, service_ports.rs, .env.example). Two corrections were made during research versus the spec's original wording, both incorporated below:
1. `aether init` does not scaffold config from the template (confirmed twice: `main.rs:866-880` is migration-only, and `install.sh:1023-1041` only *stages* `config.template/` — the user must manually activate it, and the post-install banner says so explicitly). Task 5 replicates this stage-then-first-install-activate pattern, not a naive unconditional copy.
2. Redis connectivity is TCP (`redis://127.0.0.1:6379`, already `.env.example`'s documented default), not the Unix-socket setup Docker Compose uses internally — bare metal doesn't need socket-permission choreography, so Task 1 configures native `redis-server` for TCP-only on loopback.

---

## Task 1: `scripts/build-static-deps.sh` — static `redis-server`

**Files:**
- Create: `scripts/build-static-deps.sh`

- [ ] **Step 1: Write the script's redis-server build path**

```bash
#!/usr/bin/env bash
# Cross-compile static redis-server and nginx for bare-metal AetherEMS
# installs. Runs on the build machine (still requires Docker); targets
# get zero new system dependencies. Results are cached under
# build/cache/static-deps/<name>-<version>-<arch>/ so repeat builds are
# instant once warm.
set -euo pipefail
cd "$(dirname "$0")/.."

REDIS_VERSION="${REDIS_VERSION:-8.0.2}"
NGINX_VERSION="${NGINX_VERSION:-1.27.4}"
ARCH="${1:-arm64}"

case "$ARCH" in
  arm64) DOCKER_PLATFORM="linux/arm64" ;;
  amd64) DOCKER_PLATFORM="linux/amd64" ;;
  *) echo "Usage: $0 [arm64|amd64]" >&2; exit 1 ;;
esac

CACHE_DIR="$(pwd)/build/cache/static-deps"
REDIS_OUT="$CACHE_DIR/redis-server-$REDIS_VERSION-$ARCH"
NGINX_OUT="$CACHE_DIR/nginx-$NGINX_VERSION-$ARCH"
mkdir -p "$CACHE_DIR"

if [[ -x "$REDIS_OUT/redis-server" ]]; then
  echo "redis-server $REDIS_VERSION ($ARCH) cached at $REDIS_OUT"
else
  echo "Building static redis-server $REDIS_VERSION for $ARCH..."
  mkdir -p "$REDIS_OUT"
  docker run --rm --platform "$DOCKER_PLATFORM" \
    -v "$REDIS_OUT:/out" \
    alpine:3.19 sh -c "
      set -eu
      apk add --no-cache build-base linux-headers curl
      curl -fsSL https://download.redis.io/releases/redis-${REDIS_VERSION}.tar.gz | tar xz
      cd redis-${REDIS_VERSION}
      make -j\$(nproc) BUILD_TLS=no BUILD_STATIC=yes
      cp src/redis-server /out/redis-server
      strip /out/redis-server
    "
  echo "redis-server built: $REDIS_OUT/redis-server"
fi
```

`BUILD_STATIC=yes` links against musl statically on Alpine (musl is Alpine's libc, so a normal `make` on `alpine:3.19` already produces a musl binary; `BUILD_STATIC=yes` additionally forces static linking of any remaining shared deps). `strip` matches the workspace's own release profile convention (`strip = true` in the root `Cargo.toml`).

- [ ] **Step 2: Run it and verify the redis-server binary**

```bash
chmod +x scripts/build-static-deps.sh
./scripts/build-static-deps.sh arm64
file build/cache/static-deps/redis-server-8.0.2-arm64/redis-server
```

Expected: `file` reports an ELF executable, statically linked, ARM aarch64. (If building on an amd64 dev machine without QEMU arm64 emulation configured, this step may need `docker run --platform linux/arm64` emulation — if it fails with an exec-format error, note it as a BLOCKED finding rather than silently switching to `amd64`; QEMU binfmt setup is an environment prerequisite, not something this script should paper over.)

- [ ] **Step 3: Commit**

```bash
git add scripts/build-static-deps.sh
git commit -m "feat: add build-static-deps.sh, static redis-server cross-compile"
```

---

## Task 2: extend `build-static-deps.sh` — static `nginx`

**Files:**
- Modify: `scripts/build-static-deps.sh`

**Context:** `apps/nginx.conf` requires exactly one non-default compile flag: `--with-http_auth_request_module` (used by the `/__auth_check` internal location and every proxied API location's `auth_request` directive — 6 of 7 API location blocks). Everything else in that config (gzip, proxy, WebSocket upgrade headers) is core-module, on by default. No SSL, no `stub_status`, no `resolver` — nothing else to enable.

- [ ] **Step 1: Add the nginx build path to the same script**

Append to `scripts/build-static-deps.sh` (before any final "done" echo, if one exists — there isn't yet, so just append at the end):

```bash

if [[ -x "$NGINX_OUT/nginx" ]]; then
  echo "nginx $NGINX_VERSION ($ARCH) cached at $NGINX_OUT"
else
  echo "Building static nginx $NGINX_VERSION for $ARCH..."
  mkdir -p "$NGINX_OUT"
  docker run --rm --platform "$DOCKER_PLATFORM" \
    -v "$NGINX_OUT:/out" \
    alpine:3.19 sh -c "
      set -eu
      apk add --no-cache build-base pcre2-dev zlib-dev curl
      curl -fsSL https://nginx.org/download/nginx-${NGINX_VERSION}.tar.gz | tar xz
      cd nginx-${NGINX_VERSION}
      ./configure \
        --with-http_auth_request_module \
        --with-http_v2_module \
        --without-http_uwsgi_module \
        --without-http_scgi_module \
        --without-http_fastcgi_module \
        --without-mail_pop3_module \
        --without-mail_imap_module \
        --without-mail_smtp_module \
        --with-cc-opt='-static' \
        --with-ld-opt='-static'
      make -j\$(nproc)
      cp objs/nginx /out/nginx
      strip /out/nginx
    "
  echo "nginx built: $NGINX_OUT/nginx"
fi

echo 'redis-server and nginx ready under build/cache/static-deps/'
```

`--with-http_v2_module` is included for forward compatibility (harmless if unused; nginx.conf doesn't currently request HTTP/2). The `--without-*` flags trim mail-proxy and CGI modules AetherEMS never uses, keeping the static binary smaller — purely a size optimization, not a correctness requirement, since none of them are referenced by `apps/nginx.conf`.

- [ ] **Step 2: Run and verify**

```bash
./scripts/build-static-deps.sh arm64
build/cache/static-deps/nginx-1.27.4-arm64/nginx -V 2>&1 | grep -o 'http_auth_request_module'
```

Expected: `http_auth_request_module` printed (confirms the module compiled in).

- [ ] **Step 3: Commit**

```bash
git add scripts/build-static-deps.sh
git commit -m "feat: add static nginx build with auth_request module"
```

---

## Task 3: systemd unit templates

**Files:**
- Create: `scripts/systemd/aether-redis.service`
- Create: `scripts/systemd/aether-comsrv.service`
- Create: `scripts/systemd/aether-modsrv.service`
- Create: `scripts/systemd/aether-hissrv.service`
- Create: `scripts/systemd/aether-apigateway.service`
- Create: `scripts/systemd/aether-netsrv.service`
- Create: `scripts/systemd/aether-alarmsrv.service`
- Create: `scripts/systemd/aether-apps.service`
- Create: `scripts/systemd/aether.target`

These are packaged verbatim into the `.run` bundle and installed to `/etc/systemd/system/` by `install-baremetal.sh` (Task 5). All paths inside assume the install layout from the spec: binaries at `/opt/aether/bin/`, config/env at `/etc/aether/`.

- [ ] **Step 1: Write `aether.target`**

```ini
[Unit]
Description=AetherEMS (all services)
Wants=aether-redis.service aether-comsrv.service aether-modsrv.service aether-hissrv.service aether-apigateway.service aether-netsrv.service aether-alarmsrv.service aether-apps.service

[Install]
WantedBy=multi-user.target
```

- [ ] **Step 2: Write `aether-redis.service`**

```ini
[Unit]
Description=AetherEMS - Redis
PartOf=aether.target

[Service]
Type=simple
ExecStart=/opt/aether/bin/redis-server --port 6379 --bind 127.0.0.1 --appendonly yes --save 60 1 --dir /var/lib/aether/redis
Restart=on-failure
RestartSec=2
User=root

[Install]
WantedBy=aether.target
```

`--dir /var/lib/aether/redis` matches the spec's `/var/lib/aether/` runtime-data location; `install-baremetal.sh` creates that directory (Task 5).

- [ ] **Step 3: Write `aether-comsrv.service`**

```ini
[Unit]
Description=AetherEMS - comsrv (communication service)
PartOf=aether.target
After=aether-redis.service
Requires=aether-redis.service

[Service]
Type=simple
EnvironmentFile=/etc/aether/aether.env
ExecStart=/opt/aether/bin/comsrv
Restart=on-failure
RestartSec=2
User=root
WorkingDirectory=/opt/aether

[Install]
WantedBy=aether.target
```

- [ ] **Step 4: Write `aether-modsrv.service`** (depends on comsrv, per the SHM-creation ordering documented in `docs/concepts/architecture.md`'s Startup order section)

```ini
[Unit]
Description=AetherEMS - modsrv (model/rule service)
PartOf=aether.target
After=aether-redis.service aether-comsrv.service
Requires=aether-redis.service aether-comsrv.service

[Service]
Type=simple
EnvironmentFile=/etc/aether/aether.env
ExecStart=/opt/aether/bin/modsrv
Restart=on-failure
RestartSec=2
User=root
WorkingDirectory=/opt/aether

[Install]
WantedBy=aether.target
```

- [ ] **Step 5: Write the remaining four service units** — `aether-hissrv.service`, `aether-apigateway.service`, `aether-netsrv.service`, `aether-alarmsrv.service`. Each follows the same shape as `aether-comsrv.service` (only `After=`/`Requires=aether-redis.service`, since only comsrv/modsrv have the SHM ordering constraint), with the binary name substituted:

```ini
[Unit]
Description=AetherEMS - <hissrv|apigateway|netsrv|alarmsrv> (<one-line role>)
PartOf=aether.target
After=aether-redis.service
Requires=aether-redis.service

[Service]
Type=simple
EnvironmentFile=/etc/aether/aether.env
ExecStart=/opt/aether/bin/<binary-name>
Restart=on-failure
RestartSec=2
User=root
WorkingDirectory=/opt/aether

[Install]
WantedBy=aether.target
```

Roles (for the `Description=` line, matching `docs/concepts/architecture.md`'s Services table): hissrv "historical data service", apigateway "API gateway", netsrv "MQTT networking", alarmsrv "alarm management".

- [ ] **Step 6: Write `aether-apps.service`** (static nginx, not a Rust service)

```ini
[Unit]
Description=AetherEMS - Web UI (nginx)
PartOf=aether.target
After=aether-apigateway.service
Requires=aether-apigateway.service

[Service]
Type=forking
PIDFile=/var/lib/aether/nginx/nginx.pid
ExecStartPre=/opt/aether/bin/nginx -t -c /etc/aether/nginx.conf -p /var/lib/aether/nginx
ExecStart=/opt/aether/bin/nginx -c /etc/aether/nginx.conf -p /var/lib/aether/nginx
ExecReload=/opt/aether/bin/nginx -s reload -c /etc/aether/nginx.conf -p /var/lib/aether/nginx
ExecStop=/opt/aether/bin/nginx -s stop -c /etc/aether/nginx.conf -p /var/lib/aether/nginx
Restart=on-failure
RestartSec=2
User=root

[Install]
WantedBy=aether.target
```

`-p /var/lib/aether/nginx` sets nginx's prefix (where it writes `logs/`, `nginx.pid`, temp dirs) separately from `-c` (the actual `apps/nginx.conf`, installed unmodified at `/etc/aether/nginx.conf`) — this is standard nginx practice for running outside its compiled-in default prefix (`/usr/local/nginx` from the Alpine build), and requires no changes to `apps/nginx.conf` itself, matching the spec's "零 Rust/前端改动" decision.

- [ ] **Step 7: Verify syntax with systemd's own linter, if available on the dev machine (skip gracefully if not — macOS has no systemd)**

```bash
command -v systemd-analyze >/dev/null 2>&1 && \
  for f in scripts/systemd/*.service scripts/systemd/*.target; do
    systemd-analyze verify "$f" || echo "VERIFY FAILED: $f"
  done || echo "systemd-analyze not available on this machine (expected on macOS) — syntax will be verified on first real install (Task 5 Step 5)"
```

- [ ] **Step 8: Commit**

```bash
git add scripts/systemd/
git commit -m "feat: add systemd unit templates for bare-metal deployment"
```

---

## Task 4: `build-installer.sh --bare-metal` packaging branch

**Files:**
- Modify: `scripts/build-installer.sh`

- [ ] **Step 1: Add the `--bare-metal` flag to argument parsing**

In the `while [[ $# -gt 0 ]]; do case $1 in` block (existing flags `--services=*`, `-s|--services`, `--enable-swagger` are parsed here), add a new arm before the catch-all `*)`:

```bash
        --bare-metal)
            BARE_METAL=1
            shift
            ;;
```

And initialize the variable alongside the script's other flag defaults (near `ENABLE_SWAGGER=0`):

```bash
BARE_METAL=0
```

- [ ] **Step 2: Verify the flag parses without touching existing behavior**

```bash
bash -n scripts/build-installer.sh && echo "syntax ok"
./scripts/build-installer.sh --help 2>&1 | head -5 || true
```

(The script may not have a `--help` path; the point of this step is just confirming `bash -n` — syntax check — passes after the edit. If there's no `--help`, skip that half.)

- [ ] **Step 3: Branch the packaging steps after the shared cargo-zigbuild step**

The existing script already builds all 7 binaries (`aether` + 6 services) via `cargo zigbuild` in one architecture-agnostic step before any Docker-image work begins — this step is unchanged and shared by both paths. Find the point right after that build step and before Docker image construction begins (the `[2/5]`-style banner that starts building `aetherems:latest`), and wrap the remaining Docker-specific steps:

```bash
if [[ "$BARE_METAL" == 1 ]]; then
    echo -e "${BLUE}[2/4] Building static dependencies (redis-server, nginx)...${NC}"
    ./scripts/build-static-deps.sh "$ARCH"

    echo -e "${BLUE}[3/4] Building frontend assets...${NC}"
    (cd apps && corepack enable && corepack prepare pnpm@latest --activate && pnpm install --frozen-lockfile && pnpm run build)

    echo -e "${BLUE}[4/4] Packaging bare-metal installer...${NC}"
    BM_PKG_DIR="$BUILD_DIR/baremetal-pkg"
    rm -rf "$BM_PKG_DIR"
    mkdir -p "$BM_PKG_DIR/bin" "$BM_PKG_DIR/apps-dist" "$BM_PKG_DIR/systemd" "$BM_PKG_DIR/config.template" "$BM_PKG_DIR/script-host"

    for svc in aether comsrv modsrv hissrv apigateway netsrv alarmsrv; do
        cp "target/$TARGET/release/$svc" "$BM_PKG_DIR/bin/$svc"
    done
    cp "build/cache/static-deps/redis-server-${REDIS_VERSION:-8.0.2}-$ARCH/redis-server" "$BM_PKG_DIR/bin/redis-server"
    cp "build/cache/static-deps/redis-server-${REDIS_VERSION:-8.0.2}-$ARCH/redis-cli" "$BM_PKG_DIR/bin/redis-cli"
    cp "build/cache/static-deps/nginx-${NGINX_VERSION:-1.27.4}-$ARCH/nginx" "$BM_PKG_DIR/bin/nginx"
    cp -r apps/dist/. "$BM_PKG_DIR/apps-dist/"
    cp apps/nginx.conf "$BM_PKG_DIR/nginx.conf"
    cp scripts/systemd/*.service scripts/systemd/*.target "$BM_PKG_DIR/systemd/"
    cp scripts/install-baremetal.sh "$BM_PKG_DIR/install.sh"
    cp libs/aether-script-host/main.py "$BM_PKG_DIR/script-host/main.py"
    find config.template -type f \( -name "*.yaml" -o -name "*.yml" -o -name "*.csv" -o -name "*.json" \) | while read -r f; do
        mkdir -p "$BM_PKG_DIR/$(dirname "$f")"
        cp "$f" "$BM_PKG_DIR/$f"
    done

    chmod +x "$BM_PKG_DIR/bin/"* "$BM_PKG_DIR/install.sh"

    BM_OUTPUT_NAME="AetherEdge-baremetal-${ARCH}-${VERSION}.run"
    makeself --gzip "$BM_PKG_DIR" "$OUTPUT_DIR/$BM_OUTPUT_NAME" \
        "AetherEMS bare-metal installer ($ARCH, $VERSION)" \
        bash ./install.sh

    echo -e "${GREEN}Bare-metal installer: $OUTPUT_DIR/$BM_OUTPUT_NAME${NC}"
    exit 0
fi
```

This mirrors the existing dev-mode fork pattern (`if [[ -n "$DEV_SERVICE" ]]`) that the script already uses elsewhere for a divergent packaging path — same precedent, new branch. The `config.template` copy above uses a `while read` loop (not `find -exec cp --parents`, which is GNU-only and would break on a macOS build machine's BSD `cp`) — this is the exact portable pattern `install.sh:1023-1041` already uses for the same copy, kept consistent here. `libs/aether-script-host/main.py` is bundled unmodified (Task 5's install step places it at the deployed path `/etc/aether/script-host/main.py`, matching the search order `script_runner.rs:375` already hardcodes — do not change that path).

- [ ] **Step 4: Commit**

```bash
git add scripts/build-installer.sh
git commit -m "feat: add --bare-metal packaging branch to build-installer.sh"
```

(This commit will not build successfully end-to-end until Task 5 provides `scripts/install-baremetal.sh` — that's expected; the script references a file the next task creates. Verify with `bash -n` only at this point, not a full run.)

---

## Task 5: `scripts/install-baremetal.sh`

**Files:**
- Create: `scripts/install-baremetal.sh`

**Context — config activation semantics (the correction from this plan's research phase):** Docker's `install.sh` stages `config.template/` into the install dir but never touches live config; the post-install banner tells the user to `cp -r $INSTALL_DIR/config.template/{comsrv,modsrv} $INSTALL_DIR/data/config/` manually, specifically so upgrades never clobber a customized config. This script replicates that safety property but automates the *first-install* case (since there's no existing config to protect yet): if `/etc/aether/config/` doesn't exist, it's created from the staged template; if it already exists (an upgrade), it's left untouched.

- [ ] **Step 1: Write the script**

```bash
#!/usr/bin/env bash
# Bare-metal AetherEMS installer. Packaged by build-installer.sh --bare-metal;
# this script is what makeself runs after extracting the .run archive.
set -euo pipefail
cd "$(dirname "$0")"

INSTALL_DIR="${AETHER_INSTALL_DIR:-/opt/aether}"
CONFIG_DIR="/etc/aether"
DATA_DIR="/var/lib/aether"
SYSTEMD_DIR="/etc/systemd/system"

echo "=== AetherEMS bare-metal installer ==="

if ! command -v systemctl >/dev/null 2>&1; then
    echo "ERROR: systemctl not found. This installer requires a systemd-based" >&2
    echo "Linux distribution. See docs/guides/deployment.md for Docker Compose" >&2
    echo "as an alternative on non-systemd systems." >&2
    exit 1
fi

echo "[1/6] Installing binaries to $INSTALL_DIR/bin ..."
mkdir -p "$INSTALL_DIR/bin"
cp bin/* "$INSTALL_DIR/bin/"
chmod +x "$INSTALL_DIR/bin/"*

echo "[2/6] Installing web UI assets to $INSTALL_DIR/apps ..."
mkdir -p "$INSTALL_DIR/apps"
cp -r apps-dist/. "$INSTALL_DIR/apps/"

echo "[3/6] Installing configuration to $CONFIG_DIR ..."
mkdir -p "$CONFIG_DIR"
cp nginx.conf "$CONFIG_DIR/nginx.conf"
mkdir -p "$CONFIG_DIR/script-host"
cp script-host/main.py "$CONFIG_DIR/script-host/main.py"
if [[ ! -d "$CONFIG_DIR/config" ]]; then
    echo "  First install detected: activating config.template/ -> $CONFIG_DIR/config"
    cp -r config.template "$CONFIG_DIR/config"
else
    echo "  Existing config found at $CONFIG_DIR/config — leaving it untouched (upgrade)"
    echo "  (staged template still available at $CONFIG_DIR/config.template for reference)"
    cp -r config.template "$CONFIG_DIR/config.template"
fi

if [[ ! -f "$CONFIG_DIR/aether.env" ]]; then
    cat > "$CONFIG_DIR/aether.env" <<'EOF'
AETHER_REDIS_URL=redis://127.0.0.1:6379
AETHER_COMSRV_URL=http://127.0.0.1:6001
AETHER_MODSRV_URL=http://127.0.0.1:6002
RUST_LOG=info
EOF
    echo "  Wrote default $CONFIG_DIR/aether.env"
fi

echo "[4/6] Preparing runtime data directories under $DATA_DIR ..."
mkdir -p "$DATA_DIR/redis" "$DATA_DIR/nginx" "$DATA_DIR/logs"

echo "[5/6] Installing systemd units ..."
cp systemd/*.service systemd/*.target "$SYSTEMD_DIR/"
systemctl daemon-reload

echo "[6/6] Initializing database and starting services ..."
"$INSTALL_DIR/bin/aether" --config-path "$CONFIG_DIR/config" --db-path "$DATA_DIR/aether.db" init
"$INSTALL_DIR/bin/aether" --config-path "$CONFIG_DIR/config" --db-path "$DATA_DIR/aether.db" sync

systemctl enable --now aether.target

echo ""
echo "=== Install complete ==="
systemctl --no-pager status aether.target || true
echo ""
echo "Check full health with: aether doctor"
echo "Web UI: http://$(hostname -I 2>/dev/null | awk '{print $1}'):8080"
```

- [ ] **Step 2: Verify syntax**

```bash
bash -n scripts/install-baremetal.sh && echo "syntax ok"
```

- [ ] **Step 3: Write `/opt/aether/uninstall.sh`** as a second heredoc-generated file inside `install-baremetal.sh` — insert this block right after Step 5 ("Installing systemd units"), before Step 6:

```bash
cat > "$INSTALL_DIR/uninstall.sh" <<EOF
#!/usr/bin/env bash
set -euo pipefail
echo "Stopping AetherEMS..."
systemctl stop aether.target || true
systemctl disable aether.target || true
rm -f $SYSTEMD_DIR/aether-*.service $SYSTEMD_DIR/aether.target
systemctl daemon-reload
rm -rf "$INSTALL_DIR"
echo "AetherEMS removed. Configuration and data preserved at $CONFIG_DIR and $DATA_DIR."
echo "Delete those manually if you want a full wipe."
EOF
chmod +x "$INSTALL_DIR/uninstall.sh"
```

- [ ] **Step 4: Verify the uninstall block's syntax by re-checking the whole file**

```bash
bash -n scripts/install-baremetal.sh && echo "syntax ok"
grep -c "uninstall.sh" scripts/install-baremetal.sh   # expect 2 (the cat> line and the chmod line)
```

- [ ] **Step 5: End-to-end verification — REQUIRES A REAL LINUX TARGET MACHINE, not this dev environment**

This step cannot run on macOS (no systemd, no Linux ELF execution). Document it as the acceptance test for whoever runs this plan on/against real hardware or a Linux VM/container with systemd (e.g. a `systemd`-enabled Docker container, or an actual arm64/amd64 edge box):

```bash
# On the target machine, after building the .run with Task 4's script:
scp release/AetherEdge-baremetal-arm64-*.run root@<target>:/tmp/
ssh root@<target> 'chmod +x /tmp/AetherEdge-baremetal-*.run && /tmp/AetherEdge-baremetal-*.run'
ssh root@<target> 'systemctl status aether.target'
ssh root@<target> '/opt/aether/bin/aether doctor'
```

Expected: `systemctl status aether.target` shows all 8 units `active (running)`; `aether doctor` reports all green (this requires Task 7/8's systemd-mode doctor changes to be in place — sequence this verification after those tasks land, or expect Docker-check failures until then).

- [ ] **Step 6: Commit**

```bash
git add scripts/install-baremetal.sh
git commit -m "feat: add install-baremetal.sh with config stage/activate and uninstall"
```

---

## Task 6: `aether` CLI — `DeployMode` detection

**Files:**
- Create: `tools/aether/src/deploy_mode.rs`
- Modify: `tools/aether/src/main.rs` (register the module)

- [ ] **Step 1: Write the failing test (RED)**

Create `tools/aether/src/deploy_mode.rs`:

```rust
//! Detects whether this host is running AetherEMS via Docker Compose or
//! systemd, so `aether services`/`aether doctor` can speak the right
//! backend without a user-facing flag. See
//! docs/superpowers/specs/2026-07-10-baremetal-install-design.md.

use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DeployMode {
    Docker,
    Systemd,
}

impl DeployMode {
    /// Systemd mode requires both: the unit this installer creates exists,
    /// and `systemctl` itself is on PATH (belt-and-braces — the unit file
    /// existing without a working systemctl would be a broken half-install,
    /// and falling back to Docker semantics in that case is safer than
    /// erroring, since the Docker path degrades gracefully with its own
    /// "docker-compose.yml not found" error).
    pub(crate) fn detect() -> Self {
        Self::detect_with(
            Path::new("/etc/systemd/system/aether.target"),
            which_systemctl(),
        )
    }

    fn detect_with(unit_path: &Path, systemctl_available: bool) -> Self {
        if unit_path.exists() && systemctl_available {
            DeployMode::Systemd
        } else {
            DeployMode::Docker
        }
    }
}

fn which_systemctl() -> bool {
    std::process::Command::new("systemctl")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_docker_when_unit_file_absent() {
        let mode = DeployMode::detect_with(Path::new("/nonexistent/aether.target"), true);
        assert_eq!(mode, DeployMode::Docker);
    }

    #[test]
    fn detects_docker_when_systemctl_unavailable_even_if_unit_present() {
        // Use this crate's own Cargo.toml as a stand-in "file that exists"
        // so the test doesn't depend on writing to /etc.
        let existing_file = Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");
        let mode = DeployMode::detect_with(&existing_file, false);
        assert_eq!(mode, DeployMode::Docker);
    }

    #[test]
    fn detects_systemd_when_both_present() {
        let existing_file = Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");
        let mode = DeployMode::detect_with(&existing_file, true);
        assert_eq!(mode, DeployMode::Systemd);
    }
}
```

Register the module in `tools/aether/src/main.rs` — find the `mod mcp_docs;` line (added in the earlier docs project) and add nearby, alphabetically:

```rust
mod deploy_mode;
```

- [ ] **Step 2: Run tests, confirm they pass immediately (this module has no external dependency to stub out — it's correct on first write, so RED/GREEN collapses to one step here; verify anyway)**

```bash
cargo test -p aether deploy_mode --quiet
```

Expected: 3 passed. (Unlike the docs project's `mcp_docs.rs`, there's no separately-stubbed-then-implemented function here — `detect_with` takes its inputs as parameters specifically so it's testable without touching the filesystem it detects in production. If any test fails, the implementation above has a typo — fix it directly, there's no design ambiguity to resolve.)

- [ ] **Step 3: Commit**

```bash
git add tools/aether/src/deploy_mode.rs tools/aether/src/main.rs
git commit -m "feat: add DeployMode detection for Docker vs systemd"
```

---

## Task 7: `aether` CLI — `services.rs` systemd branch

**Files:**
- Modify: `tools/aether/src/services.rs`
- Modify: `tools/aether/src/main.rs` (thread `DeployMode` into the call site)

**Context:** `execute_docker_compose` (`services.rs:539-578`) is the single seam nearly every `ServiceCommands` arm funnels through (`Start`/`Stop`/`Restart`/`Status`/`Build`/`Pull`/`Clean`/`Refresh`). `Reload` is already deploy-mode-agnostic (pure HTTP POST to comsrv, `services.rs:412-443`) and needs no changes. `Refresh`'s "smart mode" (Docker image-ID diffing, `--force-recreate`) has no systemd equivalent — this task's design decision is that **systemd-mode `Refresh` behaves like `Restart`** (there's no image to diff against; a bare-metal upgrade replaces binaries out-of-band via re-running the `.run` installer, then the service manager just needs a restart), with a one-line stderr note when `--smart` is passed so the reduced behavior isn't silent.

- [ ] **Step 1: Write the failing test (RED)**

Add to the existing `mod tests` block in `tools/aether/src/services.rs` (near the other `build_docker_compose_args` tests):

```rust
    #[test]
    fn build_systemctl_args_maps_start_to_all_units_via_target() {
        let args = build_systemctl_args("start", &[]);
        assert_eq!(args, vec!["start", "aether.target"]);
    }

    #[test]
    fn build_systemctl_args_maps_named_services_to_unit_names() {
        let args = build_systemctl_args("restart", &["comsrv".to_string(), "modsrv".to_string()]);
        assert_eq!(args, vec!["restart", "aether-comsrv", "aether-modsrv"]);
    }

    #[test]
    fn build_systemctl_args_status_defaults_to_target() {
        let args = build_systemctl_args("status", &[]);
        assert_eq!(args, vec!["status", "aether.target"]);
    }
```

- [ ] **Step 2: Run, confirm RED**

```bash
cargo test -p aether build_systemctl_args --quiet
```

Expected: FAIL — `build_systemctl_args` not defined.

- [ ] **Step 3: Implement `build_systemctl_args` and the mode-aware dispatch (GREEN)**

Add near `build_docker_compose_args` (find that function first to match its style; it's referenced by the existing tests, so it's already in this file):

```rust
/// Maps a systemctl verb + service-name list to unit-file arguments.
/// Empty `services` means "the whole stack" (`aether.target`), matching
/// `build_docker_compose_args`'s existing "empty = all services" convention.
fn build_systemctl_args(verb: &str, services: &[String]) -> Vec<String> {
    let mut args = vec![verb.to_string()];
    if services.is_empty() {
        args.push("aether.target".to_string());
    } else {
        args.extend(services.iter().map(|s| format!("aether-{s}")));
    }
    args
}

fn execute_systemctl(args: &[String]) -> Result<()> {
    let output = Command::new("systemctl").args(args).output()?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("systemctl {} failed: {}", args.join(" "), stderr.trim());
    }
    Ok(())
}
```

Now thread `DeployMode` through `handle_command`. Change its signature:

```rust
pub async fn handle_command(cmd: ServiceCommands, mode: crate::deploy_mode::DeployMode) -> Result<()> {
```

For each arm that currently calls `execute_docker_compose`/`execute_docker_compose_str`, branch on `mode`. Example for `Start` (apply the identical pattern to `Stop`, `Restart`, `Status`, `Build`, `Pull`, `Clean`):

```rust
        ServiceCommands::Start { services } => {
            ensure_shm_file_exists()?;
            ensure_logs_dir_exists()?;
            match mode {
                crate::deploy_mode::DeployMode::Docker => {
                    let mut args = vec!["up".to_string(), "-d".to_string()];
                    args.extend(services.clone());
                    execute_docker_compose_str(&args)?;
                }
                crate::deploy_mode::DeployMode::Systemd => {
                    execute_systemctl(&build_systemctl_args("start", &services))?;
                }
            }
        },
```

For `Refresh`, add the systemd branch as a reduced-functionality restart with the documented warning:

```rust
        ServiceCommands::Refresh { services, pull: _, smart } => {
            match mode {
                crate::deploy_mode::DeployMode::Docker => {
                    // ... existing Docker refresh logic unchanged ...
                }
                crate::deploy_mode::DeployMode::Systemd => {
                    if smart {
                        eprintln!(
                            "note: --smart has no effect in systemd mode (no container images to diff); performing a plain restart"
                        );
                    }
                    execute_systemctl(&build_systemctl_args("restart", &services))?;
                }
            }
        },
```

(Leave the existing Docker-path body of every arm exactly as it is today — only wrap it in the `DeployMode::Docker` match arm and add the `DeployMode::Systemd` sibling arm alongside it. Do not refactor the Docker logic itself; that's out of scope and risks regressing tested behavior.)

`Reload` needs no `match` — it stays exactly as-is (already mode-agnostic HTTP POST).

- [ ] **Step 4: Update the call site in `main.rs`**

Find `Commands::Services { command } => { ... services::handle_command(command).await?; }` (main.rs, around line 451-459 per this plan's research). Replace the Docker-specific warning and thread the detected mode:

```rust
        Commands::Services { command } => {
            let mode = deploy_mode::DeployMode::detect();
            if host.is_some() {
                eprintln!(
                    "warning: --host is ignored for 'services' (local operation, {})",
                    if mode == deploy_mode::DeployMode::Systemd { "systemd" } else { "Docker" }
                );
            }
            services::handle_command(command, mode).await?;
        },
```

- [ ] **Step 5: Run tests, confirm GREEN**

```bash
cargo build -p aether --quiet
cargo test -p aether services:: --quiet
```

Expected: all `services::` tests pass, including the 3 new `build_systemctl_args` tests and every pre-existing `build_docker_compose_args`/`ServiceCommands` test (unchanged, still exercising the Docker path).

- [ ] **Step 6: Quality gate and commit**

```bash
cargo fmt -p aether
cargo clippy -p aether --all-targets --quiet -- -D warnings
cargo test -p aether --quiet
git add tools/aether/src/services.rs tools/aether/src/main.rs
git commit -m "feat: add systemd mode to aether services (start/stop/status/refresh)"
```

---

## Task 8: `aether` CLI — `doctor.rs` mode-conditional checks

**Files:**
- Modify: `tools/aether/src/doctor.rs`

No `main.rs` change needed for this task: `run_doctor` detects `DeployMode` internally (Step 3), so its call site (`Commands::Doctor { verbose } => doctor::run_doctor(config_path, db_path, verbose, json).await?`) keeps its existing signature and is untouched — unlike Task 7's `services::handle_command`, which needs `mode` threaded in from `main.rs` because it's also used for the `--host` warning text there.

**Context:** `run_doctor` unconditionally pushes exactly 7 checks (`doctor.rs:99-105`). Two are Docker-specific: `check_docker()` (Docker Engine reachable — irrelevant under systemd, skip entirely) and `check_redis()` (does `docker inspect`/`docker exec` against a container named `aether-redis` — needs a full reimplementation, not a parameter tweak, since bare-metal Redis is a plain OS process reachable over TCP). The other 5 checks (`check_service`, `check_database`, `check_config_files`, `check_shared_memory`) are already generic and need no changes.

- [ ] **Step 1: Write the failing test (RED)**

Add to `doctor.rs`'s existing test module (create one with `#[cfg(test)] mod tests { use super::*; ... }` if none exists yet — check first; if `doctor.rs` currently has no tests at all, that's fine, this is the first):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn systemd_mode_check_list_excludes_docker_engine_check() {
        let names = check_names_for_mode(crate::deploy_mode::DeployMode::Systemd);
        assert!(!names.contains(&"Docker Engine"), "systemd mode must not run the Docker Engine check");
        assert!(names.contains(&"Redis"), "Redis connectivity must still be checked");
    }

    #[test]
    fn docker_mode_check_list_is_unchanged() {
        let names = check_names_for_mode(crate::deploy_mode::DeployMode::Docker);
        assert!(names.contains(&"Docker Engine"));
        assert_eq!(names.len(), 7, "Docker mode must still run all 7 pre-existing checks");
    }
}
```

- [ ] **Step 2: Run, confirm RED**

```bash
cargo test -p aether doctor:: --quiet
```

Expected: FAIL — `check_names_for_mode` not defined.

- [ ] **Step 3: Implement (GREEN)**

Add a small pure function that names which checks run per mode — this is the seam the tests exercise without needing to actually run subprocess-invoking checks:

```rust
/// The check names run for a given deploy mode, in the same order
/// `run_doctor` executes them. Exists as a pure, testable seam separate
/// from the actual (async, subprocess-invoking) check functions.
fn check_names_for_mode(mode: crate::deploy_mode::DeployMode) -> Vec<&'static str> {
    let mut names = Vec::new();
    if mode == crate::deploy_mode::DeployMode::Docker {
        names.push("Docker Engine");
    }
    names.push("Redis");
    names.push("comsrv");
    names.push("modsrv");
    names.push("Database");
    names.push("Config Files");
    names.push("Shared Memory");
    names
}
```

Now add the native-Redis check function (reimplementation, not a parameter tweak — the existing `check_redis()` does `docker inspect`/`docker exec`; this is a new sibling function for the systemd path):

```rust
/// Systemd-mode Redis check: pings the native redis-server directly over
/// TCP, instead of `check_redis()`'s `docker inspect`/`docker exec` into
/// the `aether-redis` container.
async fn check_redis_native() -> CheckResult {
    let start = std::time::Instant::now();
    match Command::new("redis-cli")
        .args(["-h", "127.0.0.1", "-p", "6379", "ping"])
        .output()
    {
        Ok(output) if output.status.success() && String::from_utf8_lossy(&output.stdout).trim() == "PONG" => {
            CheckResult::ok("Redis", "Running (PONG)".to_string()).with_duration(start.elapsed())
        }
        Ok(output) => CheckResult::error(
            "Redis",
            format!("redis-cli did not return PONG: {}", String::from_utf8_lossy(&output.stdout).trim()),
            Some("Check: systemctl status aether-redis".to_string()),
        ),
        Err(e) => CheckResult::error(
            "Redis",
            format!("redis-cli not runnable: {e}"),
            Some("Ensure /opt/aether/bin is on PATH, or install redis-tools".to_string()),
        ),
    }
}
```

(If `redis-cli` isn't guaranteed to be on `PATH` on a bare-metal target — the installer ships `redis-server`, not necessarily a separate `redis-cli` binary — fall back to a raw TCP `PING\r\n` write instead of shelling out. Check whether `redis-cli` is bundled before finalizing this: if Task 1's static Redis build doesn't produce `redis-cli` alongside `redis-server`, either add it to the build (Redis's `make` already builds both from the same source tree — this is a one-line addition to Task 1's `cp` step: `cp src/redis-cli /out/redis-cli` alongside the existing `cp src/redis-server ...`) or switch this function to a plain `std::net::TcpStream` write/read of `PING\r\n` → expect `+PONG\r\n`. Prefer bundling `redis-cli` — it's already built by the same `make` invocation at zero extra cost, and keeps this check symmetric with the Docker path's use of `redis-cli` inside the container.)

Retroactively fix Task 1's script to also copy `redis-cli`:

```bash
# In scripts/build-static-deps.sh, redis-server build block, add after the redis-server cp line:
      cp src/redis-cli /out/redis-cli
      strip /out/redis-cli
```

And Task 4's packaging step, add alongside the existing `redis-server` copy:

```bash
    cp "build/cache/static-deps/redis-server-${REDIS_VERSION:-8.0.2}-$ARCH/redis-cli" "$BM_PKG_DIR/bin/redis-cli"
```

Now update `run_doctor` to use `check_names_for_mode`-consistent logic (the real, effectful version):

```rust
pub async fn run_doctor(config_path: &Path, db_path: &Path, verbose: bool, json_output: bool) -> Result<()> {
    let mode = crate::deploy_mode::DeployMode::detect();
    let mut results = Vec::new();
    if mode == crate::deploy_mode::DeployMode::Docker {
        results.push(check_docker().await);
        results.push(check_redis().await);
    } else {
        results.push(check_redis_native().await);
    }
    results.push(check_service("comsrv", COMSRV_PORT).await);
    results.push(check_service("modsrv", MODSRV_PORT).await);
    results.push(check_database(db_path).await);
    results.push(check_config_files(config_path).await);
    results.push(check_shared_memory().await);
    // ... rest of the function (has_errors, printing, bail) unchanged ...
```

(Keep `run_doctor`'s existing signature — it doesn't need a `mode` parameter from its caller, since it detects internally, matching the "auto-detect, no user-facing flag" decision from the spec. This differs from `services::handle_command`, which DOES take `mode` as a parameter because `main.rs` needs it for the `--host` warning message too — no need for that duplication in the doctor path.)

- [ ] **Step 4: Run tests, confirm GREEN**

```bash
cargo build -p aether --quiet
cargo test -p aether doctor:: --quiet
```

Expected: both new tests pass.

- [ ] **Step 5: Quality gate and commit**

```bash
cargo fmt -p aether
cargo clippy -p aether --all-targets --quiet -- -D warnings
cargo test -p aether --quiet
git add tools/aether/src/doctor.rs scripts/build-static-deps.sh scripts/build-installer.sh
git commit -m "feat: add systemd-mode doctor checks, native Redis ping, bundle redis-cli"
```

---

## Task 9: `docs/guides/deployment.md` — bare-metal section

**Files:**
- Modify: `docs/guides/deployment.md`

**Context:** this is the doc corpus's `deployment.md` (from the earlier AI-native docs project — frontmatter `title: Deployment`, convention: "Aether" in prose, bare relative links, no unquoted colons in frontmatter). Ground every command in the actual scripts this plan just created — do not describe aspirational behavior.

- [ ] **Step 1: Read the current file to match its section style**

```bash
cat docs/guides/deployment.md
```

- [ ] **Step 2: Add a new `## Bare-metal Linux (systemd)` section** after the existing `## Edge installer` section, before `## Runtime paths`:

```markdown
## Bare-metal Linux (systemd)

For targets without Docker, `build-installer.sh --bare-metal` produces a
self-contained `.run` package: statically-linked `redis-server`, `nginx`,
and all six AetherEMS services, plus systemd unit files. The target machine
needs nothing beyond a systemd-based Linux distribution — no Docker, no
package manager installs.

Build (still requires Docker on the *build* machine, only to cross-compile
the static `redis-server`/`nginx` dependencies):

```bash
./scripts/build-installer.sh --bare-metal v1.2.0 arm64
```

Ship and run on the target:

```bash
scp release/AetherEdge-baremetal-arm64-v1.2.0.run root@<device>:/tmp/
ssh root@<device> '/tmp/AetherEdge-baremetal-arm64-v1.2.0.run'
```

The installer places binaries at `/opt/aether/bin/`, configuration at
`/etc/aether/`, and runtime data (Redis persistence, nginx logs) at
`/var/lib/aether/`. Eight systemd units run under one target:

```bash
systemctl status aether.target      # everything
systemctl status aether-comsrv      # one service
journalctl -u aether-modsrv -f      # tail one service's logs
```

`aether services start/stop/restart/status/refresh` and `aether doctor`
auto-detect this mode (no flag needed) by checking for
`/etc/systemd/system/aether.target` — the same commands from
[Service management on device](#service-management-on-device) below work
unmodified in both Docker and bare-metal deployments. `aether services
refresh --smart` has no image to diff against outside Docker, so it
degrades to a plain restart with a one-line notice.

Upgrading: re-running the `.run` package overwrites binaries and restarts
`aether.target`; it never overwrites an existing `/etc/aether/config/`
directory, so local customizations survive.

Uninstalling: `/opt/aether/uninstall.sh` stops and removes the systemd
units and `/opt/aether`, but preserves `/etc/aether/` and `/var/lib/aether/`
— delete those manually for a full wipe.
```

- [ ] **Step 3: Verify**

```bash
f=docs/guides/deployment.md
grep -nE "TBD|TODO|FIXME|XXX|placeholder|coming soon" "$f" && echo "PLACEHOLDERS FOUND" || echo no-placeholders
head -5 "$f" | grep -q "^title:" && echo frontmatter-ok
```

- [ ] **Step 4: Commit**

```bash
git add docs/guides/deployment.md
git commit -m "docs: add bare-metal Linux (systemd) section to deployment guide"
```

---

## Task 10: Final verification

- [ ] **Step 1: Full workspace quality gate**

```bash
./scripts/quick-check.sh
```

Expected: all checks pass, including the new `deploy_mode`/`services`/`doctor` tests.

- [ ] **Step 2: Shell script syntax sweep**

```bash
for f in scripts/build-static-deps.sh scripts/install-baremetal.sh scripts/build-installer.sh; do
  bash -n "$f" && echo "$f: syntax ok" || echo "$f: SYNTAX ERROR"
done
```

- [ ] **Step 3: systemd unit lint (skips gracefully on macOS)**

```bash
command -v systemd-analyze >/dev/null 2>&1 && \
  for f in scripts/systemd/*.service scripts/systemd/*.target; do
    systemd-analyze verify "$f" || echo "VERIFY FAILED: $f"
  done || echo "no systemd-analyze on this machine — units were already verified during Task 3"
```

- [ ] **Step 4: Confirm Docker-mode behavior is unchanged (regression check)**

```bash
cargo test -p aether services:: doctor:: --quiet 2>&1 | grep -E "test result:"
```

Expected: all passing, same pre-existing Docker-path tests still green (this plan never modified the Docker-path logic itself, only wrapped it in a `match` arm — confirm no accidental behavior change).

- [ ] **Step 5: Explicit gap — real-device testing is out of this environment's reach**

This plan's development environment is macOS. `install-baremetal.sh` (Task 5, Step 5), the systemd units actually starting services, and the end-to-end acceptance criteria from the spec (`aether doctor` all-green on a real target, UI reachable at `:8080`) **cannot be verified from here** — they require a real or virtualized systemd Linux target (arm64 device, or an amd64 Linux VM/systemd-enabled container). Flag this explicitly to whoever executes this plan: do not mark the feature "done" based on this environment's test runs alone. Steps 1-4 above are what's verifiable pre-merge; the spec's acceptance criteria 2-4 (device-level) need a follow-up manual verification pass on real hardware before shipping.

- [ ] **Step 6: Final tally (no commit unless Steps 1-4 surfaced a fix)**

```bash
git log --oneline docs/superpowers/plans/2026-07-10-baremetal-install.md 2>/dev/null | head -1
git log --oneline -15
git diff --stat HEAD~15..HEAD 2>/dev/null | tail -3
```

If any step above found a real gap, fix it as its own small commit referencing which check caught it.

---

## Spec Coverage

| Spec requirement | Task |
|---|---|
| systemd units + `aether.target`, dependency ordering (comsrv before modsrv) | Task 3 |
| arm64 + amd64 target support | Tasks 1, 2, 4 (parametrized by `$ARCH`) |
| Static redis-server, zero target-machine dependencies | Task 1 |
| Static nginx reusing `apps/nginx.conf` verbatim | Task 2, Task 3 Step 6 |
| `build-installer.sh --bare-metal` variant, new `install-baremetal.sh` | Tasks 4, 5 |
| `aether services`/`aether doctor` systemd mode, auto-detected | Tasks 6, 7, 8 |
| Root run, `/opt/aether` + `/etc/aether` + `/var/lib/aether` layout | Tasks 3, 5 |
| `script-host/main.py` bundled at its hardcoded deployed path | Tasks 4, 5 |
| Uninstall preserves `/etc/aether` | Task 5 Step 3 |
| Docs follow-up | Task 9 |
| Docker-mode regression safety | Task 7 Step 3 (wrap, don't rewrite), Task 10 Step 4 |
| `aether init` does not scaffold config (plan correction) | Task 5 (stage/activate logic) |

## Explicitly Not Doing

(mirrors the spec's list, unchanged)
- No non-systemd distro support; no `aether` self-spawn/pidfile process mode
- No TimescaleDB/PostgreSQL bundling
- No apigateway ingress changes; no nginx.conf changes; no frontend changes
- No dedicated runtime user/permission hardening; no SELinux/AppArmor
- No changes to the existing Docker `install.sh`/compose flow

## Known Gaps for Whoever Picks This Up

- Task 8's native Redis check assumes `redis-cli` ships alongside `redis-server` from the same build (added mid-task as a retroactive fix to Task 1/4 — make sure that lands, or switch to a raw TCP ping).
- Task 1's exact `REDIS_VERSION`/`NGINX_VERSION` pins (8.0.2 / 1.27.4) should be bumped to whatever the latest stable patch is at implementation time — they're script-level variables specifically so this is a one-line change, not a design decision.
- QEMU cross-arch emulation for Task 1 (building arm64 static binaries from an amd64 dev machine, or vice versa) is an environment prerequisite this plan assumes but doesn't set up — if the build machine lacks `binfmt_misc` QEMU registration, `docker run --platform` will fail with an exec-format error; that's a one-time host setup step (`docker run --privileged --rm tonistiigi/binfmt --install all`), not something to work around in the scripts.
