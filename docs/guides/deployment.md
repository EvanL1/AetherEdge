---
title: Deployment
description: Run with Docker Compose or build a self-contained installer for edge devices
updated: 2026-07-11
---

# Deployment

Aether deploys as either a set of Docker containers or, for Docker-free
targets, native systemd services. There are three paths: run the Docker
Compose stack directly on a machine that can build images, package
everything into a single self-extracting Docker-based installer and ship it
to an edge device, or build a bare-metal installer that ships statically
linked binaries and systemd units instead.

## Docker Compose

```bash
cp .env.example .env    # then edit: AETHER_BASE_PATH, HOST_UID/HOST_GID, RUST_LOG, ...

docker compose up -d
docker compose ps
```

The default Compose application starts only the six Rust services, all with
`network_mode: host`. The web client, Redis, and TimescaleDB start only when
their explicit `frontend`, `redis`, and `postgres-storage` profiles are
selected:

| Container | Image | Role |
|-----------|-------|------|
| aether-redis | redis:8-alpine | Optional non-authoritative state mirror infrastructure (`redis` profile) |
| aether-timescaledb | timescale/timescaledb:2.25.2-pg17 | Optional PostgreSQL history backend (`postgres-storage` profile) |
| aether-io | aetherems:latest | Communication service (privileged, mounts `/dev` for field buses) |
| aether-automation | aetherems:latest | Model service and rule engine |
| aether-history | aetherems:latest | SHM sampler with embedded SQLite history by default |
| aether-api | aetherems:latest | REST API, WebSocket, JWT auth |
| aether-uplink | aetherems:latest | MQTT cloud uplink, TLS certificates |
| aether-alarm | aetherems:latest | Alarm rules and notifications |
| aether-apps | aether-apps:latest | Vue.js web UI |

Start the optional browser client with
`docker compose --profile frontend up -d`; it is not part of the edge-kernel
acceptance path.

The six Rust services share one `aetherems:latest` image, each started with
its own command. The `aether-apps:latest` image must be pre-built or loaded
(`docker load < apps.tar.gz`) — the compose file does not build it.

Host networking does not make the unauthenticated process APIs public: IO,
automation, history, uplink, and alarm bind only to `127.0.0.1`. Remote clients
must enter through `aether-api` on port 6005, where JWT and role checks apply.
The optional Redis and TimescaleDB listeners are also loopback-only.

Two mount classes matter for the runtime:

- **Shared memory and local event sockets** — the host's `/dev/shm` is
  bind-mounted at `/shm/rtdb` in all six Rust services. The mount is
  read-write because the SHM owner writes point slots while isolated
  consumers create their own subscription bitmaps and UDS endpoints beside
  the segment. Mounting the directory also avoids Docker auto-creating a
  stale file entry.
- **Optional external stores** — no core service mounts a Redis socket, exports
  `REDIS_URL`, or waits for Redis. `docker compose --profile redis up -d`
  starts mirror infrastructure for a host that explicitly wires
  `aether-redis-bridge`. PostgreSQL history remains opt-in through
  `--profile postgres-storage` and a PostgreSQL-enabled history build. Set a
  unique non-empty `TIMESCALEDB_PASSWORD` before selecting that profile; the
  packaged extension installer generates one without printing it.

All Rust containers read the shared configuration SQLite database from
`${AETHER_BASE_PATH:-./data}/aether.db` (mounted at `/app/data/aether.db`)
and write logs to `${AETHER_LOG_PATH:-./logs}`. aether-history stores samples in
`/app/data/aether-history.db` unless a PostgreSQL-enabled build and backend
configuration are explicitly selected.

The services remain six independent processes. SHM/UDS replaces a mandatory
live-data broker; it does not collapse their restart or fault-isolation
boundaries.

## Edge installer

`scripts/build-installer.sh` produces a single self-extracting `.run` file
containing everything an offline edge device needs — Docker image archives,
the compose file, configuration templates, the `aether` CLI binary, and an
install script:

```text
./scripts/build-installer.sh [VERSION] [ARCH] [TARGET] [--services=...] [--enable-swagger]
```

- `VERSION` — version string, defaults to today's date (`YYYYMMDD`)
- `ARCH` — `arm64` (default) or `amd64`
- `TARGET` — Rust target triple; defaults to `aarch64-unknown-linux-musl`
  for arm64 and `x86_64-unknown-linux-musl` for amd64
- `--services` / `-s` — comma-separated subset to include (service names:
  `aether-io`, `aether-automation`, `aether-history`, `aether-api`, `aether-uplink`, `aether-alarm`, `apps`,
  `redis`, `timescaledb`; group shortcut `rust` expands to all six Rust
  services). Every fresh-install package must include the Rust core; select
  extension variants as `-s rust,redis`, `-s rust,timescaledb`, or
  `-s rust,apps`. The default package contains only the Rust edge-runtime
  image; frontend and external-store images must be selected explicitly.
- `--enable-swagger` — compile the Rust services with their feature-gated
  Swagger UI enabled

```bash
# Full installer for an ARM64 edge device
./scripts/build-installer.sh

# All Rust services only, with Swagger UI
./scripts/build-installer.sh v1.2.0 arm64 -s rust --enable-swagger
```

The script cross-compiles the six services and the `aether` CLI with
`cargo zigbuild` for the target triple, builds the `aetherems` Docker image
from those binaries, saves the images with `docker save` (plus the Redis,
TimescaleDB, and frontend images when selected), and packages the result
with `makeself` into `release/AetherEdge-<arch>-<version>.run` (subset
builds via `--services` append a service-list suffix to the file name, and
`--enable-swagger` appends `-swagger`). The build host needs Docker,
`cargo-zigbuild` (auto-installed via `cargo install` if missing), and
`makeself` (auto-installed via Homebrew on macOS).

Ship and run:

```bash
scp release/AetherEdge-arm64-<version>.run root@192.168.30.21:/tmp/
ssh root@192.168.30.21 'chmod +x /tmp/AetherEdge-arm64-<version>.run && /tmp/AetherEdge-arm64-<version>.run'
```

The embedded installer supports a **fresh deployment only**. Its first step is
a read-only preflight: if it finds an Aether installation root, install context,
site configuration or database, Aether container, or Aether systemd unit, it
exits before stopping a service, loading an image, or writing a file. On an
accepted clean host it installs to `/opt/AetherEdge`, loads the bundled images
with `docker load`, activates the fail-safe template at
`/opt/AetherEdge/data/config`, records the layout in
`/etc/aether/install.yaml`, initializes a new database, and starts the six
containers with Docker Compose. The deployment is Docker-based — the installer
delivers images and compose configuration, not standalone service binaries.

In-place upgrade, rollback to an older release, and import of an old database
or installation layout are not supported in this release. To replace an
installation, first export and back up anything that must be retained, run the
deployment-specific uninstall procedure, and manually relocate or remove every
retained Aether footprint before invoking the new installer. Translating
retained data into a new release is currently an operator-managed migration
outside the installer; do not point a fresh installer at an old site directory.

`/opt/AetherEdge` is intentionally fixed for this release because packaged
service-management paths assume that composition root. The installer rejects
`AETHER_INSTALL_DIR` overrides rather than completing an installation whose
later lifecycle operations would target a different root. `AETHER_BASE_PATH`
may place a **new, empty** data/configuration tree in a dedicated child
directory on another disk, but it must be chosen before installation and is not
a migration switch. The installer rejects `/`, system roots, generic mount
roots, symlinked paths, the installation root, and any destination containing
an Aether site before any recursive permission operation. Paths are also
limited to characters that round-trip safely through Docker Compose `.env`.

An `AETHER_TIMESCALE_DATA_PATH` outside the site root and Docker's optional
`redis-data` named volume are extension-owned storage. They must also be empty
for a fresh deployment. Reusing or migrating an extension store is outside the
installer's supported workflow.

The installer generates
`AETHER_BOOTSTRAP_ADMIN_PASSWORD`, persists it only in the mode-0600 `.env`,
and never prints the value. The completion message provides a local retrieval
command. Sign in as `admin`, change the password immediately, then remove the
bootstrap variable. Anonymous registration remains disabled unless
`AETHER_ALLOW_PUBLIC_REGISTRATION=true` is explicitly set.

The API container runs as `HOST_UID:HOST_GID`. It has neither the Docker socket
nor the installation root mounted, and `/etc/systemd/network` is read-only.
Consequently host-network mutation requests fail closed. Remote runtime
upgrade is not supported; installing another release requires the explicit
fresh-deployment workflow above rather than expanding the API process's
authority.

## Bare-metal Linux (systemd)

For edge devices that cannot or should not run Docker,
`scripts/build-installer.sh --bare-metal` produces a second kind of `.run`
package: a self-contained bundle of statically linked binaries and systemd
units, with zero container runtime dependency on the target machine. It
contains the six Rust services, the `aether` CLI, and the core systemd units.
The browser client, static `nginx`, and `aether-apps.service` are included only
when `apps` is explicitly selected. Static `redis-server`/`redis-cli` and their
unit are likewise included only when Redis is selected.
`scripts/build-static-deps.sh` uses `INCLUDE_NGINX=1` and `INCLUDE_REDIS=1`
for those extension bundles. The core services are grouped by `aether.target`.
The pinned Redis/nginx releases also pin their source-archive SHA-256 values.
Overriding either version requires its matching `REDIS_SHA256` or
`NGINX_SHA256`; a cached binary is reused only with a matching provenance
marker and after its static ELF linkage and target architecture are checked.

The bare-metal runtime root is likewise fixed at `/opt/aether`, matching the
packaged systemd units. `AETHER_INSTALL_DIR` overrides are rejected. Its
bootstrap administrator credential is stored in `/etc/aether/aether.env`
(mode 0600) with the same retrieve-change-remove lifecycle as Docker.

Build:

```bash
# Core-only package (default)
./scripts/build-installer.sh --bare-metal [VERSION] [ARCH]

# Core plus the optional browser client
./scripts/build-installer.sh --bare-metal [VERSION] [ARCH] -s rust,apps

# Core plus optional browser client and Redis mirror infrastructure
./scripts/build-installer.sh --bare-metal [VERSION] [ARCH] -s rust,apps,redis
```

This follows the same `[VERSION] [ARCH] [TARGET]` positional convention as
the Docker build — `--bare-metal` is an added flag, order of the other
arguments is unchanged. It cross-compiles the same six services plus the
`aether` CLI and packages them with `makeself` into
`release/AetherEdge-baremetal-<arch>-<version>.run`. Selecting `apps` also
builds static nginx and the frontend with `pnpm`, adding `-frontend` to the
file name; selecting Redis adds `-redis`. A bare-metal package must include
the Rust core. TimescaleDB is an external bare-metal extension and is not
bundled by this builder.

Ship and run as root — the installer refuses to proceed without
`systemctl` on PATH:

```bash
scp release/AetherEdge-baremetal-arm64-<version>.run root@192.168.30.21:/tmp/
ssh root@192.168.30.21 'chmod +x /tmp/AetherEdge-baremetal-arm64-<version>.run && /tmp/AetherEdge-baremetal-arm64-<version>.run'
```

`scripts/install-baremetal.sh` (the script the `.run` archive extracts and
runs) lays out the install as:

| Path | Contents |
|------|----------|
| `/opt/aether/bin/` | Service binaries and `aether` CLI; `nginx` and Redis tools only in explicitly selected extension bundles |
| `/etc/aether/config/` | The activated configuration (from `config.template/` on first install) |
| `/etc/aether/aether.env` | Explicit config/data/database paths, `AETHER_LOG_DIR`, `RUST_LOG`, and freshly generated secrets (mode 600) |
| `/etc/aether/install.yaml` | Non-secret installed layout used by the CLI (`config_dir`, `data_dir`, runtime mode, release channel, and enabled packs) |
| `/etc/aether/script-host/main.py` | The Python script host for aether-io custom transforms (matches the deployed-path lookup in `services/io/src/protocols/core/script_runner.rs`) |
| `/var/lib/aether/` | Service logs (`logs/`); nginx temp/log directories (`nginx/`) only with the browser client |
| `/usr/share/nginx/html/` | Optional Web UI static assets; untouched by a core-only package |

It also symlinks `aether` onto `/usr/local/bin` and drops a
`/etc/profile.d/aether.sh` PATH entry, installs the systemd units,
runs `aether init` and `aether sync` against `/etc/aether/config`, and
finishes with `systemctl enable --now aether.target`.

Day-to-day operation is native systemd:

```bash
systemctl status aether.target
journalctl -u aether-io -f
```

`aether services` and `aether doctor` auto-detect this mode — see
[CLI Reference: aether services](../reference/cli.md#aether-services) and
[aether doctor](../reference/cli.md#aether-doctor) — with no flag needed.
Detection (`tools/aether/src/deploy_mode.rs`) checks for both
`/etc/systemd/system/aether.target` and `systemctl` on PATH; if either is
missing it falls back to the Docker Compose code path. In systemd mode,
`aether services start/stop/restart/status` pass canonical service names such
as `aether-io` directly to `systemctl <verb>` (or use `aether.target` when no service is named), and
`aether services logs <service>` shells out to
`journalctl -u <service>`. `aether services build/pull/clean` all
error in this mode — there are no container images in a bare-metal install,
and the `.run` package is not an in-place upgrader. `aether services refresh
--smart` degrades to a plain `systemctl restart`, printing a note that
`--smart` has no effect, since there's no image to diff against. Redis is not
part of the default health contract; operators who enable the extension can
inspect its unit or profile independently.

None of the six Rust service units declares `Requires=aether-redis.service`.
The default target starts and keeps its SHM/SQLite work independently; an
enabled Redis mirror cannot become a service-availability dependency.

The bare-metal installer has the same fresh-only contract as the Docker
installer. Re-running a `.run` package on a host with `/opt/aether`,
`/etc/aether`, `/var/lib/aether`, installed units, or runtime data fails during
read-only preflight, before `aether.target` is stopped or files are replaced.
There is no automatic binary replacement, configuration merge, optional-unit
migration, or previous-release rollback path. Back up/export required state,
uninstall the old runtime, and manually relocate every retained footprint
before installing a new release; importing that state into the new release is
not currently supported by the installer. This does not remove the
installer's failure cleanup for a partially completed fresh installation.

Uninstall with the script the installer writes:

```bash
/opt/aether/uninstall.sh
```

It stops and disables `aether.target`, removes the systemd units, the
`aether` symlink, the PATH entry, and `/opt/aether` itself. It removes
`/usr/share/nginx/html` only when the optional frontend was installed by this
installer. `/etc/aether` and `/var/lib/aether` (configuration and runtime
data) are left in place. Those retained directories intentionally make a later
fresh install fail until an operator has exported, relocated, or removed them.

## Runtime paths

The shared-memory segment path is resolved in this order
(`libs/aether-rtdb-shm/src/core/config.rs`):

1. `AETHER_SHM_PATH` environment variable, if set
2. `/shm/rtdb/aether-rtdb.shm`, if the `/shm/rtdb` directory exists (the
   Docker mount point)
3. `/dev/shm/aether-rtdb.shm` on Linux
4. `/tmp/aether-rtdb.shm` elsewhere (macOS development)

Inside containers, `/shm/rtdb` is the host's `/dev/shm`, so both views name
the same file. Docker also places the aether-automation command socket and PointWatch
socket in this directory through `AETHER_M2C_SOCKET` and
`AETHER_AUTOMATION_POINT_WATCH_SOCKET`; native deployments keep the `/tmp`
defaults. Peripheral PointWatch socket names are derived from the resolved
SHM path, so each process binds a distinct endpoint.

Other state:

- **SQLite** — `aether.db` lives in the data directory:
  `/opt/AetherEdge/data` on an installed device, `./data` in a compose
  checkout (`AETHER_BASE_PATH`); containers see it as
  `/app/data/aether.db` (`AETHER_DB_PATH`).
- **Embedded history** — aether-history writes `aether-history.db` in the same data
  directory by default (`AETHER_HISTORY_DB_PATH`). PostgreSQL/TimescaleDB is
  an opt-in storage adapter, not a base-runtime prerequisite.
- **Configuration** — the `aether` CLI first honors flags and `AETHER_*_PATH`
  overrides, then reads `/etc/aether/install.yaml`. Without an install context,
  a source checkout uses `./data/config` and `./data`; an unregistered old
  installation directory is never adopted implicitly.
- **Logs** — `${AETHER_LOG_PATH:-./logs}` on the host, `/app/logs` in the
  containers.

## Service management on device

The installed `aether` CLI wraps Docker Compose for day-to-day operations:

```bash
aether services start      # start one or more services (or all)
aether services stop       # stop services
aether services status     # container status
aether services refresh    # recreate containers from the installed composition
aether services logs       # view service logs

aether doctor              # Docker, core services, SQLite, config files,
                           # shared memory
```

`aether services refresh` recreates containers from the composition and image
set already installed on the device. It is a same-release recovery operation,
not a supported path for replacing the installed release. See [Getting
Started](getting-started.md) for what a healthy `aether doctor` run covers.

## Related pages

- [Getting Started](getting-started.md) — build, initialize, and verify a fresh checkout
- [Connect Devices](connect-devices.md) — add channels and map points once the stack is running
- [System Architecture](../concepts/architecture.md) — the services these containers run
