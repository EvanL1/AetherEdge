---
title: Deployment
description: Run with Docker Compose or build a self-contained installer for edge devices
updated: 2026-07-17
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
`network_mode: host`. Redis and TimescaleDB start only when their explicit
`redis` and `postgres-storage` profiles are selected. The base file does not
define or start a domain-specific data processor.

AetherEdge is a headless Kernel distribution. It does not build or install a
browser client. The EMS operator console, Energy Pack, and Load-Forecasting
processor belong to the independent
[AetherEMS](https://github.com/EvanL1/AetherEMS) distribution.

| Container | Image | Role |
|-----------|-------|------|
| aether-redis | redis:8-alpine | Optional non-authoritative state mirror infrastructure (`redis` profile) |
| aether-timescaledb | timescale/timescaledb:2.25.2-pg17 | Optional PostgreSQL history backend (`postgres-storage` profile) |
| aether-io | aetherems:latest | Communication service (privileged, mounts `/dev` for field buses) |
| aether-automation | aetherems:latest | Model service and rule engine |
| aether-history | aetherems:latest | SHM sampler with embedded SQLite history by default |
| aether-api | aetherems:latest | Headless REST application gateway and JWT auth |
| aether-uplink | aetherems:latest | MQTT cloud uplink, TLS certificates |
| aether-alarm | aetherems:latest | Alarm rules and notifications |

The six Rust services share one `aetherems:latest` compatibility image, each
started with its own command. The image name is retained while downstream
release consumers migrate; it does not imply that this repository owns the EMS
product or Console.

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
  starts compatibility infrastructure only; AetherEdge ships no Redis mirror
  adapter, so any consumer belongs to a downstream composition. PostgreSQL
  history remains opt-in through `--profile postgres-storage` and a
  PostgreSQL-enabled history build. Set a unique non-empty
  `TIMESCALEDB_PASSWORD` before selecting that profile; the packaged installer
  generates one without printing it.

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
./scripts/build-installer.sh [VERSION] [ARCH] [TARGET] [--services=...] [--io-features=...] [--enable-swagger]
```

- `VERSION` — version string, defaults to today's date (`YYYYMMDD`)
- `ARCH` — `arm64` (default) or `amd64`
- `TARGET` — Rust target triple; defaults to `aarch64-unknown-linux-musl`
  for arm64 and `x86_64-unknown-linux-musl` for amd64
- `--services` / `-s` — comma-separated subset to include (service names:
  `aether-io`, `aether-automation`, `aether-history`, `aether-api`, `aether-uplink`, `aether-alarm`,
  `redis`, `timescaledb`; group shortcut `rust` expands to all six Rust
  services). Every fresh-install package must include the Rust core; select
  external-service variants as `-s rust,redis`, `-s rust,timescaledb`, or
  `-s rust,redis,timescaledb`. The default package contains only the Rust
  edge-runtime image; external-store images must be selected explicitly.
- `--enable-swagger` — compile the single `aether-api` gateway Swagger UI;
  it presents the service-owned OpenAPI documents through fixed gateway paths
- `--io-features` — replace the default `aether-io` feature set with one
  explicit comma-separated selection. The builder rejects unknown features,
  expands required dependencies once, and uses that same normalized set for
  both the binary and the packaged `runtime-manifest.json`.

```bash
# Full installer for an ARM64 edge device
./scripts/build-installer.sh

# All Rust services only, with the gateway Swagger UI
./scripts/build-installer.sh v1.2.0 arm64 -s rust --enable-swagger
```

The script cross-compiles the six services and the `aether` CLI with
`cargo zigbuild` for the target triple, builds the `aetherems` Docker image
from those binaries, saves the images with `docker save` (plus the Redis and
TimescaleDB images when selected), and packages the result
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
`redis-data` named volume are external-service storage. They must also be empty
for a fresh deployment. Reusing or migrating an external store is outside the
installer's supported workflow.

The installer generates
`AETHER_BOOTSTRAP_ADMIN_PASSWORD`, persists it only in the mode-0600 `.env`,
and never prints the value. The completion message provides a local retrieval
command. Sign in as `admin`, change the password immediately, then remove the
bootstrap variable. Anonymous registration remains disabled unless
`AETHER_ALLOW_PUBLIC_REGISTRATION=true` is explicitly set.

The API container runs as `HOST_UID:HOST_GID`. It has no Docker socket,
installation-root, host-network-configuration, or SHM mount. Browser dashboard,
host-network mutation, configuration archive, process supervision, and remote
upgrade endpoints are not kernel capabilities. Installing another release uses
the explicit fresh-deployment workflow above.

## Pack-only artifact

A domain Pack is released separately from the fresh-install `.run` package. A
Pack bundle contains only `pack-artifact.json` and the declarative `pack/`
tree—never the `aether` CLI, a service binary, or a core crate. Build it from
the exact runtime manifest generated for the target Kernel composition:

```bash
./scripts/build-pack-artifact.sh \
  packs/<pack-id> \
  build/installer/runtime/runtime-manifest.json \
  release/<pack-id>.bundle
```

Copy that directory to an edge host which already has the matching Kernel,
then install it with the host's CLI:

```bash
aether packs install --artifact /tmp/<pack-id>.bundle
```

The command refuses a different Kernel version, target triple, or complete
runtime-manifest digest. It also rejects extra top-level entries, symlinks,
executables/source trees, payload tampering, unbounded files, and an
incompatible `pack.yaml`. After verification it publishes the data below the
installed data directory as `packs/<id>/<version>` and replaces `global.yaml`
atomically only after validating the complete candidate active Pack set. A
failed activation preserves the previous configuration and removes the newly
published version.

This command does not restart services or commission the Pack. Plan any
maintenance restart separately, then verify supervisor and health endpoints; enabling channels,
instances, rules, processors, or physical control remains a distinct audited
commissioning action. The repository can build and test this local format, but
does not yet claim an independently published/signed Kernel artifact, Pack
artifact, or downstream second-repository release gate.

## Bare-metal Linux (systemd)

For edge devices that cannot or should not run Docker,
`scripts/build-installer.sh --bare-metal` produces a second kind of `.run`
package: a self-contained bundle of statically linked binaries and systemd
units, with zero container runtime dependency on the target machine. It
contains the six Rust services, the `aether` CLI, and the core systemd units.
Static `redis-server`/`redis-cli` and their unit are included only when Redis
is selected. `scripts/build-static-deps.sh` uses `INCLUDE_REDIS=1` for that
optional infrastructure bundle. The core services are grouped by `aether.target`. The pinned
Redis release also pins its source-archive SHA-256 value. Overriding the version
requires its matching `REDIS_SHA256`; a cached binary is reused only with a
matching provenance marker and after its static ELF linkage and target
architecture are checked.

The bare-metal runtime root is likewise fixed at `/opt/aether`, matching the
packaged systemd units. `AETHER_INSTALL_DIR` overrides are rejected. Its
bootstrap administrator credential is stored in `/etc/aether/aether.env`
(mode 0600) with the same retrieve-change-remove lifecycle as Docker.

Build:

```bash
# Core-only package (default)
./scripts/build-installer.sh --bare-metal [VERSION] [ARCH]

# Core plus optional Redis mirror infrastructure
./scripts/build-installer.sh --bare-metal [VERSION] [ARCH] -s rust,redis
```

This follows the same `[VERSION] [ARCH] [TARGET]` positional convention as
the Docker build — `--bare-metal` is an added flag, order of the other
arguments is unchanged. It cross-compiles the same six services plus the
`aether` CLI and packages them with `makeself` into
`release/AetherEdge-baremetal-<arch>-<version>.run`. Selecting Redis adds
`-redis` to the file name. A bare-metal package must include the Rust core.
TimescaleDB is an external bare-metal service and is not bundled by this
builder.

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
| `/opt/aether/bin/` | Service binaries and `aether` CLI; Redis tools only in an explicitly selected infrastructure bundle |
| `/etc/aether/config/` | The activated configuration (from `config.template/` on first install) |
| `/etc/aether/aether.env` | Explicit config/data/database paths, `AETHER_LOG_DIR`, `RUST_LOG`, and freshly generated secrets (mode 600) |
| `/etc/aether/install.yaml` | Non-secret installed layout used by the CLI (`config_dir`, `data_dir`, runtime mode, release channel, and enabled packs) |
| `/var/lib/aether/` | Service logs (`logs/`) and optional Redis data (`redis/`) |

It also symlinks `aether` onto `/usr/local/bin` and drops a
`/etc/profile.d/aether.sh` PATH entry, installs the systemd units,
runs `aether init` and `aether sync` against `/etc/aether/config`, and
finishes with `systemctl enable --now aether.target`.
Day-to-day operation is native systemd:

```bash
systemctl status aether.target
journalctl -u aether-io -f
```

Systemd is the sole bare-metal supervisor. Use canonical unit names directly
with `systemctl` and `journalctl`; the application CLI does not wrap host
supervision. Redis is not part of the default health contract, and operators
who enable it inspect its unit independently.

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
`aether` symlink, the PATH entry, and `/opt/aether` itself. `/etc/aether` and
`/var/lib/aether` (configuration and runtime data) are left in place. Those
retained directories intentionally make a later
fresh install fail until an operator has exported, relocated, or removed them.

## Runtime paths

The shared-memory segment path is resolved in this order
(`crates/aether-dataplane/src/core/config.rs`):

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

Use the deployment's native supervisor:

```bash
# Container installation
docker compose -f /opt/AetherEdge/docker-compose.yml ps
docker compose -f /opt/AetherEdge/docker-compose.yml restart aether-io
docker compose -f /opt/AetherEdge/docker-compose.yml logs -f aether-io

# Bare-metal installation
systemctl status aether.target
systemctl restart aether-io.service
journalctl -u aether-io.service -f

curl --fail http://127.0.0.1:6005/health
```

Restarting the installed same-release composition is recovery, not a supported
release replacement path.

## Related pages

- [Getting Started](getting-started.md) — build, initialize, and verify a fresh checkout
- [Connect Devices](connect-devices.md) — add channels and map points once the stack is running
- [System Architecture](../concepts/architecture.md) — the services these containers run
