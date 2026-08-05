#!/usr/bin/env bash
set -euo pipefail

# This legacy entry point should be run on Apple Silicon.
SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" &>/dev/null && pwd)"
REPO_DIR="$(cd -- "${SCRIPT_DIR}/.." &>/dev/null && pwd)"
MACOS_APP_SCRIPT="${SCRIPT_DIR}/macos-app.sh"

if [[ ! -f "${MACOS_APP_SCRIPT}" ]]; then
  echo "macOS packaging requires ${MACOS_APP_SCRIPT}, which is maintained separately." >&2
  exit 1
fi

VERSION="$(
  awk '
    $0 == "[workspace.package]" { in_workspace_package = 1; next }
    in_workspace_package && /^\[/ { exit }
    in_workspace_package && /^version[[:space:]]*=/ {
      gsub(/"/, "", $3)
      print $3
      exit
    }
  ' "${REPO_DIR}/Cargo.toml"
)"

bash "${MACOS_APP_SCRIPT}"
mkdir -p "${REPO_DIR}/build-deploy"
cp "${SCRIPT_DIR}/out/playit.dmg" "${REPO_DIR}/build-deploy/playit-${VERSION}.dmg"
cp "${REPO_DIR}/target/release/playit-cli" \
  "${REPO_DIR}/build-deploy/playit-${VERSION}-apple-m1"
cp "${REPO_DIR}/target/x86_64-apple-darwin/release/playit-cli" \
  "${REPO_DIR}/build-deploy/playit-${VERSION}-apple-intel"
