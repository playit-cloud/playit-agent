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

IFS=. read -r MAJOR MINOR PATCH <<<"$VERSION"

sed -Ei \
  -e 's/("version_major": )[0-9]+/\1'"$MAJOR"'/' \
  -e 's/("version_minor": )[0-9]+/\1'"$MINOR"'/' \
  -e 's/("version_patch": )[0-9]+/\1'"$PATCH"'/' \
  -e 's|("release_url": "[^"]*/tag/v)[^"]+|\1'"$VERSION"'|' \
  agent-schema-release.json
