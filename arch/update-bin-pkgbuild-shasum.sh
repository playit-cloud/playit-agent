#!/usr/bin/env bash
set -euo pipefail

IMAGE="${PLAYIT_ARCH_TEST_IMAGE:-docker.io/library/archlinux:base-devel}"
SCRIPT_PATH="${BASH_SOURCE[0]}"
REPO_DIR="$(cd -- "$(dirname -- "${SCRIPT_PATH}")/.." && pwd)"
PKGBUILD_PATH="arch/bin/PKGBUILD"

usage() {
  cat >&2 <<EOF
usage: $0 [--inside-container]

Updates sha256sums in ${PKGBUILD_PATH} with updpkgsums from pacman-contrib.

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
    /bin/bash -ceu '
      pacman -Sy --noconfirm --needed ca-certificates pacman-contrib sudo
      exec bash arch/update-bin-pkgbuild-shasum.sh --inside-container
    '
fi

require_file() {
  if [[ ! -f "$1" ]]; then
    echo "missing required file: $1" >&2
    exit 1
  fi
}

require_file "${PKGBUILD_PATH}"
require_file arch/bin/playit-bin.install

bash -n "${PKGBUILD_PATH}"

work_dir="$(mktemp -d)"
trap 'rm -rf "${work_dir}"' EXIT

mkdir -p "${work_dir}/pkg"
cp "${PKGBUILD_PATH}" arch/bin/playit-bin.install "${work_dir}/pkg/"

useradd -m builder
chown -R builder:builder "${work_dir}"

(
  cd "${work_dir}/pkg"
  sudo -u builder updpkgsums PKGBUILD
)

cp "${work_dir}/pkg/PKGBUILD" "${PKGBUILD_PATH}"
bash -n "${PKGBUILD_PATH}"

echo "Updated ${PKGBUILD_PATH} sha256sums"
