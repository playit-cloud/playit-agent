#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" &>/dev/null && pwd)"
REPO_DIR="$(cd -- "${SCRIPT_DIR}/.." &>/dev/null && pwd)"
cd "${REPO_DIR}"

cargo build --release --all --target=x86_64-pc-windows-msvc
cargo wix --target x86_64-pc-windows-msvc --package playit-cli --nocapture \
  --output=target/wix/x86_64-pc-windows-msvc.msi
