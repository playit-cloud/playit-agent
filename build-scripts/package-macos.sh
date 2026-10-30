#!/usr/bin/env bash
set -euo pipefail

# Builds universal (Apple Silicon + Intel) playit and playitd binaries and packages them
# as target/macos-release/<version>/playit-macos-universal.tar.gz. Run on a Mac.

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" &>/dev/null && pwd)"
REPO_DIR="$(cd -- "${SCRIPT_DIR}/.." &>/dev/null && pwd)"
TARGETS=(aarch64-apple-darwin x86_64-apple-darwin)

if [[ "$(uname -s)" != "Darwin" ]]; then
  echo "package-macos.sh must run on macOS" >&2
  exit 1
fi

cd "${REPO_DIR}"
VERSION="$(cargo metadata --no-deps --format-version 1 \
  | python3 -c 'import json,sys; print(next(p["version"] for p in json.load(sys.stdin)["packages"] if p["name"] == "playit-cli"))')"
OUT_DIR="${REPO_DIR}/target/macos-release/${VERSION}"
STAGE_DIR="${OUT_DIR}/playit-macos-universal"

for target in "${TARGETS[@]}"; do
  rustup target add "${target}"
  cargo build --release --target "${target}" -p playit-cli -p playitd
done

rm -rf "${STAGE_DIR}"
mkdir -p "${STAGE_DIR}"
universal() {
  local binary="$1" output="$2" slices=()
  for target in "${TARGETS[@]}"; do
    slices+=("target/${target}/release/${binary}")
  done
  lipo -create -output "${STAGE_DIR}/${output}" "${slices[@]}"
}
universal playit-cli playit
universal playitd playitd
cp LICENSE.txt "${STAGE_DIR}/"

tar -C "${OUT_DIR}" -czf "${OUT_DIR}/playit-macos-universal.tar.gz" playit-macos-universal
lipo -info "${STAGE_DIR}/playit" "${STAGE_DIR}/playitd"
echo "Wrote ${OUT_DIR}/playit-macos-universal.tar.gz"
