#!/usr/bin/env bash
# Packages the playit window as playit-gui .deb, .rpm and .apk packages in target/pkg.
# deb and rpm take a glibc build; apk takes a build made on Alpine with musl linked
# dynamically (RUSTFLAGS="-C target-feature=-crt-static"), since the window loads Vulkan and
# Wayland at runtime.
set -euo pipefail

START_DIR="$(pwd)"
SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" &>/dev/null && pwd)"
REPO_DIR="$(cd -- "${SCRIPT_DIR}/.." &>/dev/null && pwd)"

if [[ $# -lt 2 ]]; then
  echo "usage: $0 <playit-gui-binary> <nfpm-arch: amd64|arm64> [deb|rpm|apk ...]" >&2
  exit 1
fi

if ! command -v nfpm >/dev/null 2>&1; then
  echo "nfpm not found. Install it with: go install github.com/goreleaser/nfpm/v2/cmd/nfpm@latest" >&2
  exit 1
fi

GUI_BIN="$1"
NFPM_ARCH="$2"
shift 2
FORMATS=("$@")
if [[ ${#FORMATS[@]} -eq 0 ]]; then
  FORMATS=(deb rpm)
fi

if [[ "${GUI_BIN}" != /* ]]; then
  GUI_BIN="${START_DIR}/${GUI_BIN}"
fi
if [[ ! -f "${GUI_BIN}" ]]; then
  echo "playit-gui binary not found: ${GUI_BIN}" >&2
  exit 1
fi

# The window renders with Vulkan through GPUI, which only targets 64-bit platforms.
case "${NFPM_ARCH}" in
  amd64) DEB_ARCH=amd64 RPM_ARCH=x86_64 APK_ARCH=x86_64 ;;
  arm64) DEB_ARCH=arm64 RPM_ARCH=aarch64 APK_ARCH=aarch64 ;;
  *)
    echo "unsupported nFPM architecture for playit-gui: ${NFPM_ARCH} (use amd64 or arm64)" >&2
    exit 1
    ;;
esac

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
export PLAYIT_GUI_BIN="${GUI_BIN}"

OUT_DIR="${REPO_DIR}/target/pkg"
mkdir -p "${OUT_DIR}"

for format in "${FORMATS[@]}"; do
  case "${format}" in
    deb) output="${OUT_DIR}/playit-gui_${DEB_ARCH}.deb" ;;
    rpm) output="${OUT_DIR}/playit-gui_${RPM_ARCH}.rpm" ;;
    apk) output="${OUT_DIR}/playit-gui_${APK_ARCH}.apk" ;;
    *)
      echo "unsupported package format: ${format} (use deb, rpm or apk)" >&2
      exit 1
      ;;
  esac
  echo "Building ${format} package: ${output}"
  (cd "${REPO_DIR}" && nfpm package --config build-scripts/nfpm-gui.yaml --packager "${format}" --target "${output}")
done
