#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")/.."

# Construct retired identifiers so this guard does not contain the very tokens
# it rejects. Frontend sources and immutable historical records are outside the
# edge-kernel rename boundary.
suffix="srv"
legacy_names=(
  "com${suffix}"
  "mod${suffix}"
  "his${suffix}"
  "alarm${suffix}"
  "net${suffix}"
  "api""gateway"
)

failed=0
for legacy_name in "${legacy_names[@]}"; do
  if rg --hidden --ignore-case --files \
      -g '!.git/**' \
      -g '!target/**' \
      -g '!apps/**' \
      -g '!CHANGELOG.md' \
      -g '!docs/plans/**' \
      -g '!docs/superpowers/**' \
      | rg --ignore-case --fixed-strings "/${legacy_name}"; then
    echo "retired service name remains in a path: ${legacy_name}" >&2
    failed=1
  fi

  if rg --hidden --ignore-case --fixed-strings --line-number "${legacy_name}" \
      -g '!.git/**' \
      -g '!target/**' \
      -g '!apps/**' \
      -g '!CHANGELOG.md' \
      -g '!docs/plans/**' \
      -g '!docs/superpowers/**' \
      .; then
    echo "retired service name remains in file content: ${legacy_name}" >&2
    failed=1
  fi
done

roles=(io automation history alarm api uplink)
for role in "${roles[@]}"; do
  canonical="aether-${role}"
  manifest="services/${role}/Cargo.toml"
  unit="scripts/systemd/${canonical}.service"

  if [[ ! -f "${manifest}" ]] || ! rg --quiet --fixed-strings "name = \"${canonical}\"" "${manifest}"; then
    echo "canonical Cargo package/binary name missing: ${canonical}" >&2
    failed=1
  fi

  if ! rg --quiet "^  ${canonical}:$" docker-compose.yml; then
    echo "canonical Compose service missing: ${canonical}" >&2
    failed=1
  fi
  if rg --quiet "^  ${role}:$" docker-compose.yml; then
    echo "short Compose service leaked into public runtime: ${role}" >&2
    failed=1
  fi

  if [[ ! -f "${unit}" ]] \
      || ! rg --quiet --fixed-strings "ExecStart=/opt/aether/bin/${canonical}" "${unit}"; then
    echo "canonical systemd unit or executable missing: ${canonical}" >&2
    failed=1
  fi
done

exit "${failed}"
