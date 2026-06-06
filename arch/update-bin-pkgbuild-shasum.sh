#!/usr/bin/env bash
set -euo pipefail

IMAGE="${PLAYIT_ARCH_TEST_IMAGE:-docker.io/library/alpine:latest}"
SCRIPT_PATH="${BASH_SOURCE[0]}"
REPO_DIR="$(cd -- "$(dirname -- "${SCRIPT_PATH}")/.." && pwd)"
PKGBUILD_PATH="arch/bin/PKGBUILD"

usage() {
  cat >&2 <<EOF
usage: $0 [--inside-container]

Updates sha256sums in ${PKGBUILD_PATH} by running makepkg --geninteg
inside an Alpine container.

Environment:
  PLAYIT_ARCH_TEST_IMAGE  Container image to use (default: ${IMAGE})
EOF
}

if [[ "${1:-}" == "--help" || "${1:-}" == "-h" ]]; then
  usage
  exit 0
fi

if [[ "${1:-}" != "--inside-container" ]]; then
  if ! command -v podman >/dev/null 2>&1; then
    echo "podman is required to update PKGBUILD checksums" >&2
    exit 1
  fi

  exec podman run --rm \
    --volume "${REPO_DIR}:/work:Z" \
    --workdir /work \
    "${IMAGE}" \
    /bin/sh -ceu '
      apk add --no-cache bash ca-certificates curl fakeroot git pacman sudo
      exec bash arch/update-bin-pkgbuild-shasum.sh --inside-container
    '
fi

require_file() {
  if [[ ! -f "$1" ]]; then
    echo "missing required file: $1" >&2
    exit 1
  fi
}

extract_sha256sums() {
  awk '
    /^sha256sums(_x86_64|_aarch64|_armv7h|_i686)?=\(/ {
      print
      if ($0 !~ /\)/) {
        in_block = 1
      }
      next
    }
    in_block {
      print
      if ($0 ~ /\)/) {
        in_block = 0
      }
    }
  ' "$1"
}

replace_sha256sums() {
  local pkgbuild="$1"
  local sums_file="$2"
  local tmp

  tmp="$(mktemp)"
  awk -v sums_file="${sums_file}" '
    BEGIN {
      while ((getline line < sums_file) > 0) {
        generated = generated line ORS
      }
      close(sums_file)
    }
    skip {
      if ($0 ~ /\)/) {
        skip = 0
      }
      next
    }
    /^sha256sums(_x86_64|_aarch64|_armv7h|_i686)?=\(/ {
      if (!inserted) {
        printf "%s", generated
        inserted = 1
      }
      if ($0 !~ /\)/) {
        skip = 1
      }
      next
    }
    { print }
  ' "${pkgbuild}" > "${tmp}"
  mv "${tmp}" "${pkgbuild}"
}

require_file "${PKGBUILD_PATH}"
require_file arch/bin/playit-bin.install

bash -n "${PKGBUILD_PATH}"

work_dir="$(mktemp -d)"
trap 'rm -rf "${work_dir}"' EXIT

mkdir -p "${work_dir}/pkg"
cp "${PKGBUILD_PATH}" arch/bin/playit-bin.install "${work_dir}/pkg/"

adduser -D builder >/dev/null
chown -R builder:builder "${work_dir}"

(
  cd "${work_dir}/pkg"
  sudo -u builder makepkg --geninteg > "${work_dir}/makepkg-geninteg.out"
)

extract_sha256sums "${work_dir}/makepkg-geninteg.out" > "${work_dir}/sha256sums.out"

if [[ ! -s "${work_dir}/sha256sums.out" ]]; then
  echo "makepkg did not produce sha256sums output" >&2
  exit 1
fi

replace_sha256sums "${PKGBUILD_PATH}" "${work_dir}/sha256sums.out"
bash -n "${PKGBUILD_PATH}"

echo "Updated ${PKGBUILD_PATH} sha256sums"
