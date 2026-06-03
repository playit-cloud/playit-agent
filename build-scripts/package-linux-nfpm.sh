#!/usr/bin/env bash
set -euo pipefail

START_DIR="$(pwd)"
SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" &>/dev/null && pwd)"
REPO_DIR="$(cd -- "${SCRIPT_DIR}/.." &>/dev/null && pwd)"

usage() {
  echo "usage: $0 <playit-cli-binary> [playitd-binary] <nfpm-arch> [format ...]" >&2
}

if [[ $# -lt 2 ]]; then
  usage
  exit 1
fi

resolve_input_path() {
  local input_path="$1"
  local full_path

  if [[ "$1" = /* ]]; then
    full_path="${input_path}"
  else
    full_path="${START_DIR}/${input_path}"
  fi

  local dir
  local base
  dir="$(dirname "${full_path}")"
  base="$(basename "${full_path}")"

  if [[ -d "${dir}" ]]; then
    printf '%s/%s\n' "$(cd "${dir}" && pwd -P)" "${base}"
  else
    printf '%s\n' "${full_path}"
  fi
}

if ! command -v nfpm >/dev/null 2>&1; then
  echo "nfpm not found. Install it with: go install github.com/goreleaser/nfpm/v2/cmd/nfpm@latest" >&2
  exit 1
fi

CLI_SRC_PATH="$1"

if [[ $# -ge 3 && "$2" != "amd64" && "$2" != "arm64" && "$2" != "arm7" && "$2" != "386" ]]; then
  DAEMON_SRC_PATH="$2"
  NFPM_ARCH="$3"
  shift 3
else
  DAEMON_SRC_PATH="$(dirname "${CLI_SRC_PATH}")/playitd"
  NFPM_ARCH="$2"
  shift 2
fi

FORMATS=("$@")
if [[ ${#FORMATS[@]} -eq 0 ]]; then
  FORMATS=(deb rpm apk archlinux)
fi

case "${NFPM_ARCH}" in
  amd64)
    DEB_ARCH=amd64
    RPM_ARCH=x86_64
    APK_ARCH=x86_64
    ARCHLINUX_ARCH=x86_64
    ;;
  arm64)
    DEB_ARCH=arm64
    RPM_ARCH=aarch64
    APK_ARCH=aarch64
    ARCHLINUX_ARCH=aarch64
    ;;
  arm7)
    DEB_ARCH=armhf
    RPM_ARCH=armv7hl
    APK_ARCH=armv7
    ARCHLINUX_ARCH=armv7h
    ;;
  386)
    DEB_ARCH=i386
    RPM_ARCH=i386
    APK_ARCH=x86
    ARCHLINUX_ARCH=i686
    ;;
  *)
    echo "unsupported nFPM architecture: ${NFPM_ARCH}" >&2
    echo "supported architectures: amd64 arm64 arm7 386" >&2
    exit 1
    ;;
esac

CLI_BIN="$(resolve_input_path "${CLI_SRC_PATH}")"
DAEMON_BIN="$(resolve_input_path "${DAEMON_SRC_PATH}")"

if [[ ! -f "${CLI_BIN}" ]]; then
  echo "playit CLI binary not found: ${CLI_BIN}" >&2
  exit 1
fi

if [[ ! -f "${DAEMON_BIN}" ]]; then
  echo "playit daemon binary not found: ${DAEMON_BIN}" >&2
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

if [[ -z "${VERSION}" ]]; then
  echo "failed to read workspace package version from ${REPO_DIR}/Cargo.toml" >&2
  exit 1
fi

export PLAYIT_VERSION="${PLAYIT_VERSION:-${VERSION}}"
export PLAYIT_PACKAGE_RELEASE="${PLAYIT_PACKAGE_RELEASE:-1}"
export PLAYIT_NFPM_ARCH="${NFPM_ARCH}"
export PLAYIT_CLI_BIN="${CLI_BIN}"
export PLAYITD_BIN="${DAEMON_BIN}"

OUT_DIR="${REPO_DIR}/target/pkg"
mkdir -p "${OUT_DIR}"
rm -f "${OUT_DIR}"/playit_*.ipk

for format in "${FORMATS[@]}"; do
  case "${format}" in
    deb)
      output="${OUT_DIR}/playit_${DEB_ARCH}.deb"
      config="${REPO_DIR}/build-scripts/nfpm.yaml"
      ;;
    rpm)
      output="${OUT_DIR}/playit_${RPM_ARCH}.rpm"
      config="${REPO_DIR}/build-scripts/nfpm.yaml"
      ;;
    apk)
      output="${OUT_DIR}/playit_${APK_ARCH}.apk"
      config="${REPO_DIR}/build-scripts/nfpm-apk.yaml"
      ;;
    archlinux)
      output="${OUT_DIR}/playit_${ARCHLINUX_ARCH}.pkg.tar.zst"
      config="${REPO_DIR}/build-scripts/nfpm.yaml"
      ;;
    *)
      echo "unsupported nFPM package format: ${format}" >&2
      echo "supported formats: deb rpm apk archlinux" >&2
      exit 1
      ;;
  esac

  echo "Building ${format} package: ${output}"
  (
    cd "${REPO_DIR}"
    nfpm package --config "${config}" --packager "${format}" --target "${output}"
  )
done
