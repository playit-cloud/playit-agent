#!/usr/bin/env bash
# Builds playit and packages it as a .deb, the same way the release workflow does:
# playit-cli and playitd are built with cross (static musl on amd64/arm64), and
# playit-gui is built with the host toolchain and included on amd64/arm64.
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" &>/dev/null && pwd)"
REPO_DIR="$(cd -- "${SCRIPT_DIR}/.." &>/dev/null && pwd)"

usage() {
  cat >&2 <<EOF
usage: $0 [--no-gui] [--native] [deb-arch]

Builds playit-cli, playitd and playit-gui, then writes target/pkg/playit_<deb-arch>.deb.

  deb-arch   amd64, arm64, armhf or i386 (default: this machine's architecture)
  --no-gui   leave out the desktop window
  --native   build playit-cli and playitd with cargo for this machine (glibc)
             instead of with cross; deb-arch must match this machine

Set PLAYIT_GUI_GNU_BIN to package a playit-gui binary you already built instead of
building one. The desktop window is only packaged on amd64 and arm64, and is built
for this machine, so a different deb-arch needs --no-gui or PLAYIT_GUI_GNU_BIN.

Needs nfpm (go install github.com/goreleaser/nfpm/v2/cmd/nfpm@latest), and cross
(cargo install cross) unless --native is passed. Building playit-gui needs
libxkbcommon-dev libxkbcommon-x11-dev libxcb1-dev pkg-config.
EOF
}

host_deb_arch() {
  case "$(uname -m)" in
    x86_64) echo amd64 ;;
    aarch64 | arm64) echo arm64 ;;
    armv7l | armv7*) echo armhf ;;
    i386 | i686) echo i386 ;;
    *) echo "unknown" ;;
  esac
}

WITH_GUI=1
NATIVE=0
DEB_ARCH=""

while [[ $# -gt 0 ]]; do
  case "$1" in
    --no-gui) WITH_GUI=0 ;;
    --native) NATIVE=1 ;;
    -h | --help)
      usage
      exit 0
      ;;
    -*)
      echo "unknown option: $1" >&2
      usage
      exit 1
      ;;
    *)
      if [[ -n "${DEB_ARCH}" ]]; then
        usage
        exit 1
      fi
      DEB_ARCH="$1"
      ;;
  esac
  shift
done

HOST_ARCH="$(host_deb_arch)"
DEB_ARCH="${DEB_ARCH:-${HOST_ARCH}}"

# Targets match the build_linux matrix in .github/workflows/build-release.yml.
case "${DEB_ARCH}" in
  amd64)
    NFPM_ARCH=amd64
    TARGET=x86_64-unknown-linux-musl
    GUI_SUPPORTED=1
    ;;
  arm64)
    NFPM_ARCH=arm64
    TARGET=aarch64-unknown-linux-musl
    GUI_SUPPORTED=1
    ;;
  armhf)
    NFPM_ARCH=arm7
    TARGET=armv7-unknown-linux-gnueabihf
    GUI_SUPPORTED=0
    ;;
  i386)
    NFPM_ARCH=386
    TARGET=i686-unknown-linux-gnu
    GUI_SUPPORTED=0
    ;;
  *)
    echo "unsupported Debian architecture: ${DEB_ARCH}" >&2
    echo "supported Debian architectures: amd64 arm64 armhf i386" >&2
    exit 1
    ;;
esac

if [[ "${GUI_SUPPORTED}" -eq 0 ]]; then
  WITH_GUI=0
fi

# Check every requirement before spending time on a build.
if ! command -v nfpm >/dev/null 2>&1; then
  echo "nfpm not found. Install it with: go install github.com/goreleaser/nfpm/v2/cmd/nfpm@latest" >&2
  exit 1
fi

if [[ "${NATIVE}" -eq 1 ]]; then
  if [[ "${DEB_ARCH}" != "${HOST_ARCH}" ]]; then
    echo "--native builds for this machine (${HOST_ARCH}), not ${DEB_ARCH}" >&2
    exit 1
  fi
elif ! command -v cross >/dev/null 2>&1; then
  echo "cross not found. Install it with: cargo install cross" >&2
  echo "or pass --native to build playit-cli and playitd with cargo for this machine" >&2
  exit 1
fi

if [[ "${WITH_GUI}" -eq 1 && -z "${PLAYIT_GUI_GNU_BIN:-}" && "${DEB_ARCH}" != "${HOST_ARCH}" ]]; then
  echo "playit-gui is built for this machine (${HOST_ARCH}), not ${DEB_ARCH}." >&2
  echo "Pass --no-gui, or set PLAYIT_GUI_GNU_BIN to a playit-gui built for ${DEB_ARCH}." >&2
  exit 1
fi

# The build runs from the repo root, so resolve a relative path from the caller first.
if [[ -n "${PLAYIT_GUI_GNU_BIN:-}" ]]; then
  PLAYIT_GUI_GNU_BIN="$(realpath -m -- "${PLAYIT_GUI_GNU_BIN}")"
fi

cd "${REPO_DIR}"

if [[ "${NATIVE}" -eq 1 ]]; then
  cargo build --release --locked --package playit-cli --package playitd
  BIN_DIR="target/release"
else
  cross build --release --locked --target "${TARGET}" --package playit-cli --package playitd
  BIN_DIR="target/${TARGET}/release"
fi

if [[ "${WITH_GUI}" -eq 1 ]]; then
  if [[ -z "${PLAYIT_GUI_GNU_BIN:-}" ]]; then
    # CI builds this on ubuntu-22.04 to keep the glibc requirement low. A build on a
    # newer distro may not start on older ones.
    cargo build --release --locked --package playit-gui
    PLAYIT_GUI_GNU_BIN="${REPO_DIR}/target/release/playit-gui"
  fi
  export PLAYIT_GUI_GNU_BIN
else
  unset PLAYIT_GUI_GNU_BIN
fi

"${SCRIPT_DIR}/package-linux-nfpm.sh" \
  "${BIN_DIR}/playit-cli" \
  "${BIN_DIR}/playitd" \
  "${NFPM_ARCH}" \
  deb

echo "built target/pkg/playit_${DEB_ARCH}.deb"
