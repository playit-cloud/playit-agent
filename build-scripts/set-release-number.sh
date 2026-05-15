#!/usr/bin/env bash
set -euo pipefail

VERSION="${1:?missing version}"

PACKAGES=(
  playit-agent-core
  playit-ipc
  playitd
)

# root workspace package version
sed -Ei '/^\[workspace\.package\]/,/^\[/{s/^version = "[^"]+"/version = "'"$VERSION"'"/}' Cargo.toml

# dependency versions for allowlisted internal packages
for pkg in "${PACKAGES[@]}"; do
  find packages -name Cargo.toml -type f -print0 | xargs -0 sed -Ei \
    's/('"$pkg"' = \{[^}]*version = )"[^"]+"/\1"'"$VERSION"'"/'
done
