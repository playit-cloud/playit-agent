#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" &>/dev/null && pwd)"
REPO_DIR="$(cd -- "${SCRIPT_DIR}/.." &>/dev/null && pwd)"
cd "${REPO_DIR}"

cargo publish --package=playit-agent-proto
cargo publish --package=playit-api-client
cargo publish --package=playit-ipc
cargo publish --package=playit-service-manager
cargo publish --package=playit-agent-core
