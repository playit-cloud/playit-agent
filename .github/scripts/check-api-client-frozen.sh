#!/usr/bin/env bash
set -euo pipefail

repository_root="$(git rev-parse --show-toplevel)"
cd "$repository_root"

actual_manifest="$(mktemp)"
trap 'rm -f "$actual_manifest"' EXIT

find packages/api_client -type f -print0 \
  | LC_ALL=C sort -z \
  | xargs -0 sha256sum > "$actual_manifest"

diff --unified .github/api-client.sha256 "$actual_manifest"
