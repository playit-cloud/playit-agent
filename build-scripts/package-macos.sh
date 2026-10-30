#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'HELP'
Usage: build-scripts/package-macos.sh

Builds universal (Apple Silicon + Intel) macOS release artifacts. Run on a Mac.

Outputs in target/macos-release/<version>/:
  playit-<version>.pkg              Installer for playit.app (menu bar app, CLI, and service)
  playit-macos-universal.tar.gz     playit and playitd binaries for manual installs

Signing is skipped unless these are set:
  PLAYIT_MACOS_APP_IDENTITY        "Developer ID Application: ..." for the app and binaries
  PLAYIT_MACOS_INSTALLER_IDENTITY  "Developer ID Installer: ..." for the .pkg
  PLAYIT_MACOS_NOTARY_PROFILE      notarytool keychain profile; notarizes and staples the .pkg
HELP
}

if [[ "${1:-}" == "-h" || "${1:-}" == "--help" ]]; then
  usage
  exit 0
fi

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" &>/dev/null && pwd)"
REPO_DIR="$(cd -- "${SCRIPT_DIR}/.." &>/dev/null && pwd)"
MACOS_DIR="${SCRIPT_DIR}/macos"
TARGETS=(aarch64-apple-darwin x86_64-apple-darwin)
PKG_IDENTIFIER="gg.playit.pkg"

if [[ "$(uname -s)" != "Darwin" ]]; then
  echo "package-macos.sh must run on macOS" >&2
  exit 1
fi

cd "${REPO_DIR}"
VERSION="$(cargo metadata --no-deps --format-version 1 \
  | python3 -c 'import json,sys; print(next(p["version"] for p in json.load(sys.stdin)["packages"] if p["name"] == "playit-cli"))')"
OUT_DIR="${REPO_DIR}/target/macos-release/${VERSION}"
WORK_DIR="${OUT_DIR}/work"
STAGE_DIR="${OUT_DIR}/playit-macos-universal"
APP_DIR="${WORK_DIR}/root/Applications/playit.app"

for target in "${TARGETS[@]}"; do
  rustup target add "${target}"
  cargo build --release --target "${target}" -p playit-cli -p playitd -p playitd-tray
done

rm -rf "${WORK_DIR}" "${STAGE_DIR}"
mkdir -p "${STAGE_DIR}" "${APP_DIR}/Contents/MacOS" "${APP_DIR}/Contents/Resources"

universal() {
  local binary="$1" output="$2" slices=()
  for target in "${TARGETS[@]}"; do
    slices+=("target/${target}/release/${binary}")
  done
  lipo -create -output "${output}" "${slices[@]}"
}

sign() {
  if [[ -n "${PLAYIT_MACOS_APP_IDENTITY:-}" ]]; then
    codesign --force --options runtime --timestamp --sign "${PLAYIT_MACOS_APP_IDENTITY}" "$@"
  fi
}

# Standalone binaries.
universal playit-cli "${STAGE_DIR}/playit"
universal playitd "${STAGE_DIR}/playitd"
sign "${STAGE_DIR}/playit" "${STAGE_DIR}/playitd"
cp LICENSE.txt "${STAGE_DIR}/"
tar -C "${OUT_DIR}" -czf "${OUT_DIR}/playit-macos-universal.tar.gz" playit-macos-universal

# playit.app: the menu bar app is the main executable; playit and playitd sit beside it.
universal playitd-tray "${APP_DIR}/Contents/MacOS/playitd-tray"
cp "${STAGE_DIR}/playit" "${STAGE_DIR}/playitd" "${APP_DIR}/Contents/MacOS/"
sed "s/@VERSION@/${VERSION}/g" "${MACOS_DIR}/Info.plist" > "${APP_DIR}/Contents/Info.plist"
install -m 0755 "${MACOS_DIR}/uninstall.sh" "${APP_DIR}/Contents/Resources/uninstall.sh"

ICONSET="${WORK_DIR}/AppIcon.iconset"
mkdir -p "${ICONSET}"
sips -s format png packages/playit-cli/wix/Product.ico --out "${WORK_DIR}/icon.png" >/dev/null
for size in 16 32 128 256 512; do
  sips -z "${size}" "${size}" "${WORK_DIR}/icon.png" --out "${ICONSET}/icon_${size}x${size}.png" >/dev/null
  double=$((size * 2))
  sips -z "${double}" "${double}" "${WORK_DIR}/icon.png" --out "${ICONSET}/icon_${size}x${size}@2x.png" >/dev/null
done
iconutil -c icns "${ICONSET}" -o "${APP_DIR}/Contents/Resources/AppIcon.icns"

sign "${APP_DIR}/Contents/MacOS/playit" "${APP_DIR}/Contents/MacOS/playitd"
sign "${APP_DIR}"

# Installer. The app must not be relocated to a copy the user moved elsewhere, since the
# /usr/local/bin/playit link and login items point into /Applications.
COMPONENT_PLIST="${WORK_DIR}/component.plist"
pkgbuild --analyze --root "${WORK_DIR}/root" "${COMPONENT_PLIST}"
plutil -replace 0.BundleIsRelocatable -bool NO "${COMPONENT_PLIST}"
mkdir -p "${WORK_DIR}/packages"
pkgbuild \
  --root "${WORK_DIR}/root" \
  --component-plist "${COMPONENT_PLIST}" \
  --scripts "${MACOS_DIR}/scripts" \
  --identifier "${PKG_IDENTIFIER}" \
  --version "${VERSION}" \
  --install-location / \
  "${WORK_DIR}/packages/playit-component.pkg"

sed "s/@VERSION@/${VERSION}/g" "${MACOS_DIR}/distribution.xml" > "${WORK_DIR}/distribution.xml"
PKG_PATH="${OUT_DIR}/playit-${VERSION}.pkg"
productbuild_args=(
  --distribution "${WORK_DIR}/distribution.xml"
  --package-path "${WORK_DIR}/packages"
)
if [[ -n "${PLAYIT_MACOS_INSTALLER_IDENTITY:-}" ]]; then
  productbuild_args+=(--sign "${PLAYIT_MACOS_INSTALLER_IDENTITY}" --timestamp)
fi
productbuild "${productbuild_args[@]}" "${PKG_PATH}"

if [[ -n "${PLAYIT_MACOS_NOTARY_PROFILE:-}" ]]; then
  xcrun notarytool submit "${PKG_PATH}" --keychain-profile "${PLAYIT_MACOS_NOTARY_PROFILE}" --wait
  xcrun stapler staple "${PKG_PATH}"
fi

lipo -info "${APP_DIR}/Contents/MacOS/playitd-tray" "${STAGE_DIR}/playit" "${STAGE_DIR}/playitd"
if [[ -z "${PLAYIT_MACOS_APP_IDENTITY:-}" ]]; then
  echo "warning: built unsigned artifacts; set PLAYIT_MACOS_APP_IDENTITY to sign" >&2
fi
echo "Wrote ${PKG_PATH}"
echo "Wrote ${OUT_DIR}/playit-macos-universal.tar.gz"
