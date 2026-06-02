#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" &>/dev/null && pwd)"

VERSION="${1:-}"

if [ -z "$VERSION" ]; then
  echo "missing version"
  exit 1
fi

DOWN_FOLDER="${SCRIPT_DIR}/../target/downloads"

rm -rf "$DOWN_FOLDER"
mkdir -p "$DOWN_FOLDER"

FILES=(
  playit-cli-linux-aarch64
  playit-cli-linux-amd64
  playit-cli-linux-armv7
  playit-cli-linux-i686

  playit-linux-aarch64
  playit-linux-amd64
  playit-linux-armv7
  playit-linux-i686

  playit-windows-x86-signed.exe
  playit-windows-x86-signed.msi
  playit-windows-x86.exe
  playit-windows-x86.msi

  playit-windows-x86_64-signed.exe
  playit-windows-x86_64-signed.msi
  playit-windows-x86_64.exe
  playit-windows-x86_64.msi

  playit_amd64.deb
  playit_arm64.deb
  playit_armhf.deb
  playit_i386.deb

  playit_x86_64.rpm
  playit_aarch64.rpm
  playit_armv7hl.rpm
  playit_i386.rpm

  playit_x86_64.apk
  playit_aarch64.apk
  playit_armv7.apk
  playit_x86.apk

  playit_x86_64.pkg.tar.zst
  playit_aarch64.pkg.tar.zst
  playit_armv7h.pkg.tar.zst
  playit_i686.pkg.tar.zst

  playit_x86_64.ipk
  playit_arm64.ipk
  playit_armhf.ipk
  playit_i386.ipk
)

BASE_URL="https://github.com/playit-cloud/playit-agent/releases/download/v${VERSION}"

for file in "${FILES[@]}"; do
  echo "Downloading $file"
  curl -fL \
    -o "${DOWN_FOLDER}/${file}" \
    "${BASE_URL}/${file}"
done
