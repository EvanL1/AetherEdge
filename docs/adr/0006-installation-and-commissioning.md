# ADR-0006: Separate installation from IoT commissioning

## Status

Accepted on 2026-07-11. The installation-context, fail-safe default,
read-only setup plan, explicit plan apply, fresh-install refusal, health-check,
and release-integrity clauses are implemented. In-place runtime upgrade and
legacy installation/database import are explicitly unsupported. Commissioning
plans, application-level configuration snapshots, and hosted documentation are
follow-up work governed by the same boundaries.

## Context

Aether had the pieces of an installable edge runtime, but Docker, systemd, and
the CLI inferred different configuration and data paths. A successful
installer invocation could therefore be followed by a CLI invocation that
read a different layout. The distribution template also contained enabled
energy channels and a control rule, which made an uncommissioned generic IoT
installation unsafe and industry-specific.

AI-native onboarding should eventually be as simple as installing Aether,
connecting an agent skill, and asking the agent to get started. Unlike a cloud
database, however, an IoT gateway can produce physical side effects. Software
installation and device commissioning must therefore remain separate safety
boundaries.

## Decision

1. Installation creates a non-secret `/etc/aether/install.yaml` descriptor
   containing runtime mode, configuration directory, data directory, runtime
   directory, release channel, and enabled domain packs.
2. The CLI resolves configuration and data paths independently with this
   precedence: explicit flag, path environment variable, install context,
   the current working directory's `data/config` and `data` paths. Relative
   explicit/environment paths are converted to absolute paths immediately, so
   a repository checkout and its default Compose mounts share one site tree.
3. A fresh distribution is uncommissioned: it has no channel, instance, or
   enabled control rule and cannot contact field equipment. Industry examples
   belong to optional packs and ship disabled.
4. First-run onboarding uses `aether setup`: the default is a persistent
   read-only plan, and apply requires the unchanged SHA-256 plan ID. Setup may
   create only missing, byte-exact safe distribution files and new local
   SQLite state; it refuses custom partial, unrecognized, and commissioned
   sites without writing. `aether init` remains an explicit developer/schema
   tool, not an installer path for importing an existing database.
5. Configuration is validated in full and global, IO, and automation changes
   are applied in one SQLite transaction. `--force` may replace managed rows
   but never bypasses validation. A failed apply leaves the previous database
   state intact.
6. `aether doctor` is the local acceptance gate for all six core services, the
   real nested configuration tree, SQLite, and SHM. Redis and PostgreSQL are
   never implicit health requirements.
7. Downloaded release artifacts are verified before extraction. Offline fresh
   installation must not depend on an Internet connection. Shipping a
   version-matched offline documentation subset remains a follow-up criterion.
8. AI onboarding starts with the same structured `aether setup` plan used by
   humans. A setup apply never starts services, installs a pack, or enables a
   channel, instance, rule, or physical control capability. Future device
   commissioning plans remain explicit and audited.
9. Repository Markdown remains the documentation source of truth. A hosted
   static mirror may improve discovery, search, and version navigation, but it
   cannot become an edge-runtime dependency and must retain version-matched
   offline resources.
10. Setup plan IDs bind the absolute configuration/data paths, setup plan
    schema, CLI build version, core schema version, and expected hashes of all
    embedded safe files. SQLite inspection uses a stable private DB+WAL
    snapshot, and setup takes an immediate writer transaction that rejects any
    commissioned channel, instance, or rule before syncing the safe baseline.
11. The authenticated API gateway is the only remotely exposed Rust API in
    packaged compositions. Internal process APIs and optional database
    extensions bind to host loopback. `AETHER_BASE_PATH` may select only a new,
    empty site root; it is not a populated-site migration switch. Dangerous
    recursive filesystem roots, symlinked paths, non-empty Aether site roots,
    and Compose-unsafe path strings are rejected before host mutation.
12. Packaged Docker and bare-metal installers are fresh-install-only. A
    read-only preflight rejects any existing Aether installation root, install
    context, site configuration/database, container, or systemd unit before it
    stops a service, loads an image, or writes a file. The installer performs no
    in-place upgrade, rollback to an older release, legacy-layout conversion,
    or old-database import. Replacing a release requires an explicit backup and
    deployment-specific uninstall followed by operator-managed removal or
    relocation of every retained footprint.

## Consequences

### Positive

- Docker, systemd, the CLI, and future setup agents share one installed layout.
- Installing Aether cannot accidentally open a fieldbus or run an energy rule.
- Configuration readers never observe a partially applied multi-domain sync.
- An existing or ambiguous site cannot be mistaken for a clean install target;
  refusal occurs before the installer mutates runtime state.
- The standalone runtime stays industry-neutral and external-service-free.
- Online documentation can evolve independently from deterministic edge
  operation.

### Negative

- Replacing an installed release requires export/backup, complete uninstall,
  and operator-managed data migration; there is no automatic installer upgrade
  or legacy database import.
- Energy examples moved out of the default template, so operators must select
  and commission the pack explicitly.
- A complete Neon-style experience still requires a commissioning planner,
  pack loader, versioned application-configuration snapshots and rollback,
  and deeper agent integration.

## Follow-up acceptance criteria

- Device commissioning is plan-based, idempotent, deny-by-default, and uses
  the same confirmation and audit policy for CLI, HTTP, and AI interfaces.
- Configuration rollback restores a named, versioned snapshot and verifies all
  six services afterward.
- AI, CLI, and HTTP configuration writes converge on the same application
  capability and audit path.
- Hosted documentation exposes versioned pages, `llms.txt`, and raw Markdown,
  while every release carries the matching offline subset.
