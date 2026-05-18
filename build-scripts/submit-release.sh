#!/usr/bin/env bash
set -euo pipefail

API_KEY="${1:?missing API key}"
SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" &>/dev/null && pwd)"

curl --fail-with-body -X POST \
  --header "authorization: Api-Key ${API_KEY}" \
  --header "content-type: application/json" \
  --data-binary "@${SCRIPT_DIR}/../agent-schema-release.json" \
  "https://api.playit.gg/release/agent_version"
