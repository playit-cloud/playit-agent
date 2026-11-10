#!/usr/bin/env bash
set +x
set -euo pipefail

usage() {
  cat <<'HELP'
Usage: build-scripts/sign-macos.sh [--skip-notarize] [RELEASE_DIR]

Signs unsigned macOS release artifacts on Linux, then notarizes and staples the installer.
RELEASE_DIR holds the output of build-scripts/package-macos.sh from a Mac:
  playit-<version>.pkg
  playit-macos-universal.tar.gz
It defaults to target/macos-release/<workspace version>.

Signed artifacts and SHA256SUMS are written to RELEASE_DIR/signed/.

Each certificate comes from exactly one of a .p12 file, a PEM file, or a YubiKey slot:
  PLAYIT_MACOS_APP_P12, PLAYIT_MACOS_APP_P12_PASSWORD_FILE    Developer ID Application
  PLAYIT_MACOS_APP_PEM                                        (key and certificate)
  PLAYIT_MACOS_APP_SMARTCARD_SLOT                             (for example 9c)
  PLAYIT_MACOS_INSTALLER_P12, ..._P12_PASSWORD_FILE           Developer ID Installer
  PLAYIT_MACOS_INSTALLER_PEM
  PLAYIT_MACOS_INSTALLER_SMARTCARD_SLOT

Other settings:
  PLAYIT_MACOS_NOTARY_API_KEY      App Store Connect API key JSON from
                                   `rcodesign encode-app-store-connect-api-key`
  PLAYIT_MACOS_SMARTCARD_PIN_FILE  File containing the YubiKey PIN; otherwise prompt privately
  PLAYIT_MACOS_TIMESTAMP_URL       Time-stamp server (default: Apple's)

--skip-notarize signs without notarizing, for example with a test certificate.
Run build-scripts/macos/install-signing-tools.sh first. See build-scripts/macos-signing.md.
HELP
}

notarize=true
release_dir=
while [[ $# -gt 0 ]]; do
  case "$1" in
    --skip-notarize) notarize=false; shift ;;
    -h|--help) usage; exit 0 ;;
    -*) echo "Unknown argument: $1" >&2; usage >&2; exit 2 ;;
    *) [[ -z "$release_dir" ]] || { echo 'Only one release directory may be given' >&2; exit 2; }
      release_dir="$1"; shift ;;
  esac
done

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
REPO_DIR="$(cd -- "$SCRIPT_DIR/.." && pwd)"
export PATH="$REPO_DIR/target/macos-sign-tools/bin:$PATH"

log() {
  TZ=UTC printf '[macos-sign %(%H:%M:%SZ)T] %s\n' -1 "$*"
}

for tool in rcodesign mkbom xar python3; do
  command -v "$tool" >/dev/null || {
    echo "Missing $tool; run build-scripts/macos/install-signing-tools.sh (and apt-get install xar)" >&2
    exit 1
  }
done

if [[ -z "$release_dir" ]]; then
  version="$(awk '/^\[workspace.package\]/{p=1;next} p && /^\[/{exit} p && /^version = /{gsub(/"/, "", $3);print $3;exit}' "$REPO_DIR/Cargo.toml")"
  release_dir="$REPO_DIR/target/macos-release/$version"
fi
release_dir="$(cd -- "$release_dir" && pwd)"
pkgs=("$release_dir"/playit-*.pkg)
[[ ${#pkgs[@]} = 1 && -f "${pkgs[0]}" ]] || { echo "Expected one playit-<version>.pkg in $release_dir" >&2; exit 1; }
pkg="${pkgs[0]}"
tarball="$release_dir/playit-macos-universal.tar.gz"
[[ -f "$tarball" ]] || { echo "Missing $tarball" >&2; exit 1; }

# Fills the named array with rcodesign arguments for the APP or INSTALLER certificate.
uses_smartcard=false
identity_args() {
  local kind="$1" p12 pem slot password_file
  local -n args="$2"
  p12="PLAYIT_MACOS_${kind}_P12"; p12="${!p12:-}"
  pem="PLAYIT_MACOS_${kind}_PEM"; pem="${!pem:-}"
  slot="PLAYIT_MACOS_${kind}_SMARTCARD_SLOT"; slot="${!slot:-}"
  password_file="PLAYIT_MACOS_${kind}_P12_PASSWORD_FILE"; password_file="${!password_file:-}"
  local count=0
  for value in "$p12" "$pem" "$slot"; do [[ -z "$value" ]] || count=$((count + 1)); done
  [[ "$count" = 1 ]] || {
    echo "Set exactly one of PLAYIT_MACOS_${kind}_P12, _PEM, or _SMARTCARD_SLOT" >&2
    exit 1
  }
  if [[ -n "$p12" ]]; then
    [[ -r "$p12" ]] || { echo "Cannot read $p12" >&2; exit 1; }
    args=(--p12-file "$p12")
    if [[ -n "$password_file" ]]; then
      [[ -r "$password_file" ]] || { echo "Cannot read $password_file" >&2; exit 1; }
      args+=(--p12-password-file "$password_file")
    fi
  elif [[ -n "$pem" ]]; then
    [[ -r "$pem" ]] || { echo "Cannot read $pem" >&2; exit 1; }
    args=(--pem-file "$pem")
  else
    args=(--smartcard-slot "$slot" --smartcard-pin-env PLAYIT_MACOS_SMARTCARD_PIN)
    uses_smartcard=true
  fi
  args+=(--timestamp-url "${PLAYIT_MACOS_TIMESTAMP_URL:-http://timestamp.apple.com/ts01}")
}
app_identity=()
installer_identity=()
identity_args APP app_identity
identity_args INSTALLER installer_identity
if "$notarize"; then
  app_identity+=(--for-notarization)
  [[ -n "${PLAYIT_MACOS_NOTARY_API_KEY:-}" && -r "${PLAYIT_MACOS_NOTARY_API_KEY}" ]] || {
    echo 'Set PLAYIT_MACOS_NOTARY_API_KEY to a readable API key JSON, or use --skip-notarize' >&2
    exit 1
  }
fi

trap 'unset PLAYIT_MACOS_SMARTCARD_PIN' EXIT
if "$uses_smartcard"; then
  if [[ -n "${PLAYIT_MACOS_SMARTCARD_PIN_FILE:-}" ]]; then
    [[ -r "$PLAYIT_MACOS_SMARTCARD_PIN_FILE" ]] || { echo 'Cannot read PLAYIT_MACOS_SMARTCARD_PIN_FILE' >&2; exit 1; }
    PLAYIT_MACOS_SMARTCARD_PIN="$(< "$PLAYIT_MACOS_SMARTCARD_PIN_FILE")"
  else
    read -r -s -p 'YubiKey PIN: ' PLAYIT_MACOS_SMARTCARD_PIN < /dev/tty
    printf '\n' > /dev/tty
  fi
  [[ -n "$PLAYIT_MACOS_SMARTCARD_PIN" ]] || { echo 'The YubiKey PIN must not be empty' >&2; exit 1; }
  export PLAYIT_MACOS_SMARTCARD_PIN
fi

work="$release_dir/linux-sign-work"
output="$release_dir/signed"
rm -rf "$work"
mkdir -p "$work/flat" "$work/tar" "$output"

log "Expanding ${pkg##*/}"
xar -xf "$pkg" -C "$work/flat"
components=()
for dir in "$work/flat"/*.pkg; do
  [[ -f "$dir/Payload" ]] && components+=("$dir")
done
[[ ${#components[@]} = 1 ]] || { echo 'Expected one component package with a Payload' >&2; exit 1; }
component="${components[0]}"
python3 -I "$SCRIPT_DIR/macos/pkg-payload.py" extract "$component" "$work/root"

app="$work/root/Applications/playit.app"
tray="$app/Contents/Helpers/playit-tray.app"
tar -xzf "$tarball" -C "$work/tar"
standalone="$work/tar/playit-macos-universal"
# The tarball reuses the app's signed binaries, so notarizing the installer also
# registers the standalone binaries' code hashes with Apple.
for binary in playit playitd; do
  cmp -s "$standalone/$binary" "$app/Contents/MacOS/$binary" || {
    echo "$binary differs between the .pkg and the tarball; package both from one build" >&2
    exit 1
  }
done

# Sign inside out, matching package-macos.sh: nested code before the bundles that contain it.
sign_code() {
  local target="${*: -1}"
  log "Signing ${target#"$work/root/"}"
  rcodesign sign "${app_identity[@]}" --code-signature-flags runtime "$@" >/dev/null
}
sign_code "$app/Contents/MacOS/playit"
sign_code "$app/Contents/MacOS/playitd"
sign_code --shallow "$tray"
sign_code --shallow "$app"

log 'Rebuilding the installer'
cp "$app/Contents/MacOS/playit" "$app/Contents/MacOS/playitd" "$standalone/"
tar --owner=0 --group=0 --numeric-owner -C "$work/tar" -czf "$work/playit-macos-universal.tar.gz" playit-macos-universal
install_kbytes="$(python3 -I "$SCRIPT_DIR/macos/pkg-payload.py" pack "$component" "$work/root")"
# productbuild copies the component's size into the Distribution's pkg-ref.
sed -i -E "s/(<pkg-ref [^>]*installKBytes=\")[0-9]+\"/\1$install_kbytes\"/" "$work/flat/Distribution"
(
  cd "$work/flat"
  entries=(Distribution)
  for entry in *; do [[ "$entry" = Distribution ]] || entries+=("$entry"); done
  xar --compression none -cf "$work/unsigned.pkg" "${entries[@]}"
)
signed_pkg="$work/${pkg##*/}"
log "Signing ${pkg##*/}"
rcodesign sign "${installer_identity[@]}" "$work/unsigned.pkg" "$signed_pkg" >/dev/null

if "$notarize"; then
  log 'Notarizing and stapling the installer (this usually takes a few minutes)'
  rcodesign notary-submit --api-key-file "$PLAYIT_MACOS_NOTARY_API_KEY" --staple "$signed_pkg"
fi

log 'Copying signed artifacts'
rm -f "$output"/*.pkg "$output"/*.tar.gz "$output/SHA256SUMS"
cp "$signed_pkg" "$work/playit-macos-universal.tar.gz" "$output/"
(cd "$output" && sha256sum -- *.pkg *.tar.gz > SHA256SUMS)
rm -rf "$work"
if ! "$notarize"; then
  echo 'warning: the installer is not notarized; Gatekeeper will block it on other Macs' >&2
fi
log "Signed artifacts: $output"
