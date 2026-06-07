#!/usr/bin/env bash
set -euo pipefail

IMAGE="${PLAYIT_ARCH_PKGBUILD_IMAGE:-${PLAYIT_ARCH_TEST_IMAGE:-localhost/playit-arch-pkgbuild-tools:latest}}"
BASE_IMAGE="${PLAYIT_ARCH_PKGBUILD_BASE_IMAGE:-docker.io/library/archlinux:base-devel}"
CONTAINERFILE="${PLAYIT_ARCH_PKGBUILD_CONTAINERFILE:-arch/Containerfile}"
SCRIPT_PATH="${BASH_SOURCE[0]}"
REPO_DIR="$(cd -- "$(dirname -- "${SCRIPT_PATH}")/.." && pwd)"
PKGBUILD_PATH="arch/bin/PKGBUILD"

usage() {
  cat >&2 <<EOF
usage: $0 [--inside-container]

Updates sha256sums in ${PKGBUILD_PATH} with updpkgsums from pacman-contrib.

Environment:
  PLAYIT_ARCH_PKGBUILD_IMAGE          Container image to run (default: ${IMAGE})
  PLAYIT_ARCH_PKGBUILD_BASE_IMAGE     Base image used when building the image (default: ${BASE_IMAGE})
  PLAYIT_ARCH_PKGBUILD_CONTAINERFILE  Containerfile to build (default: ${CONTAINERFILE})
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

  if ! podman image exists "${IMAGE}"; then
    podman build \
      --build-arg "BASE_IMAGE=${BASE_IMAGE}" \
      --tag "${IMAGE}" \
      --file "${REPO_DIR}/${CONTAINERFILE}" \
      "${REPO_DIR}"
  fi

  exec podman run --rm \
    --volume "${REPO_DIR}:/work:Z" \
    --workdir /work \
    "${IMAGE}" \
    /bin/bash -ceu 'exec bash arch/update-bin-pkgbuild-shasum.sh --inside-container'
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

id -u builder >/dev/null 2>&1 || useradd -m builder
chown -R builder:builder "${work_dir}"

(
  cd "${work_dir}/pkg"
  sudo -u builder updpkgsums PKGBUILD
)

cp "${work_dir}/pkg/PKGBUILD" "${PKGBUILD_PATH}"
bash -n "${PKGBUILD_PATH}"

echo "Updated ${PKGBUILD_PATH} sha256sums"
