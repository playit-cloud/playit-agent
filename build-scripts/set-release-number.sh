#!/usr/bin/env bash
set -euo pipefail

VERSION="${1:?missing version}"

PACKAGES=(
  playit-agent-core
  playit-ipc
  playitd
)

ARCH_PKGBUILDS=(
  arch/build/PKGBUILD
  arch/bin/PKGBUILD
)

reset_arch_bin_pkgbuild_shasums() {
  local pkgbuild="arch/bin/PKGBUILD"
  local tmp

  tmp="$(mktemp)"
  awk '
    BEGIN {
      replacements["sha256sums"] = "sha256sums=(\047SKIP\047 \047SKIP\047 \047SKIP\047 \047SKIP\047 \047SKIP\047)"
      replacements["sha256sums_x86_64"] = "sha256sums_x86_64=(\047SKIP\047 \047SKIP\047)"
      replacements["sha256sums_aarch64"] = "sha256sums_aarch64=(\047SKIP\047 \047SKIP\047)"
      replacements["sha256sums_armv7h"] = "sha256sums_armv7h=(\047SKIP\047 \047SKIP\047)"
      replacements["sha256sums_i686"] = "sha256sums_i686=(\047SKIP\047 \047SKIP\047)"
    }
    skip {
      if ($0 ~ /\)/) {
        skip = 0
      }
      next
    }
    /^sha256sums(_x86_64|_aarch64|_armv7h|_i686)?=\(/ {
      name = $0
      sub(/=.*/, "", name)
      print replacements[name]
      if ($0 !~ /\)/) {
        skip = 1
      }
      next
    }
    { print }
  ' "$pkgbuild" > "$tmp"
  mv "$tmp" "$pkgbuild"
}

reset_arch_build_pkgbuild_shasums() {
  local pkgbuild="arch/build/PKGBUILD"
  local tmp

  tmp="$(mktemp)"
  awk '
    skip {
      if ($0 ~ /\)/) {
        skip = 0
      }
      next
    }
    /^sha256sums=\(/ {
      print "sha256sums=(\047SKIP\047)"
      if ($0 !~ /\)/) {
        skip = 1
      }
      next
    }
    { print }
  ' "$pkgbuild" > "$tmp"
  mv "$tmp" "$pkgbuild"
}

# root workspace package version
sed -Ei '/^\[workspace\.package\]/,/^\[/{s/^version = "[^"]+"/version = "'"$VERSION"'"/}' Cargo.toml

# dependency versions for allowlisted internal packages
for pkg in "${PACKAGES[@]}"; do
  find packages -name Cargo.toml -type f -print0 | xargs -0 sed -Ei \
    's/('"$pkg"' = \{[^}]*version = )"[^"]+"/\1"'"$VERSION"'"/'
done

# Arch package versions
for pkgbuild in "${ARCH_PKGBUILDS[@]}"; do
  sed -Ei 's/^pkgver=.*/pkgver='"$VERSION"'/' "$pkgbuild"
done
reset_arch_bin_pkgbuild_shasums
reset_arch_build_pkgbuild_shasums

IFS=. read -r MAJOR MINOR PATCH <<<"$VERSION"

sed -Ei \
  -e 's/("version_major": )[0-9]+/\1'"$MAJOR"'/' \
  -e 's/("version_minor": )[0-9]+/\1'"$MINOR"'/' \
  -e 's/("version_patch": )[0-9]+/\1'"$PATCH"'/' \
  -e 's|("release_url": "[^"]*/tag/v)[^"]+|\1'"$VERSION"'|' \
  agent-schema-release.json

cargo check --all
