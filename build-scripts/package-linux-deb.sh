#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" &>/dev/null && pwd)"

if [[ $# -ne 2 && $# -ne 3 ]]; then
  echo "usage: $0 <playit-cli-binary> [playitd-binary] <deb-arch>" >&2
  exit 1
fi

if [[ $# -eq 3 ]]; then
  CLI_BIN="$1"
  DAEMON_BIN="$2"
  DEB_ARCH="$3"
else
  CLI_BIN="$1"
  DAEMON_BIN=""
  DEB_ARCH="$2"
fi

case "${DEB_ARCH}" in
  amd64)
    NFPM_ARCH=amd64
    ;;
  arm64)
    NFPM_ARCH=arm64
    ;;
  armhf)
    NFPM_ARCH=arm7
    ;;
  i386)
    NFPM_ARCH=386
    ;;
  *)
    echo "unsupported Debian architecture: ${DEB_ARCH}" >&2
    echo "supported Debian architectures: amd64 arm64 armhf i386" >&2
    exit 1
    ;;
esac

if [[ -n "${DAEMON_BIN}" ]]; then
  "${SCRIPT_DIR}/package-linux-nfpm.sh" "${CLI_BIN}" "${DAEMON_BIN}" "${NFPM_ARCH}" deb
else
  "${SCRIPT_DIR}/package-linux-nfpm.sh" "${CLI_BIN}" "${NFPM_ARCH}" deb
fi
