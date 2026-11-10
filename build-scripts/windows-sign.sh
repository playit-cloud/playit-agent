#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
REPO_DIR="$(cd -- "$SCRIPT_DIR/.." && pwd)"

usage() {
  cat <<'HELP'
Usage: build-scripts/windows-sign.sh <version>

Download the unsigned Windows release assets for <version> from GitHub and sign them
with the locally connected YubiKey. Each signed file is saved with a -signed suffix,
for example playit-windows-x86_64-signed.msi. Nothing is uploaded.

Environment:
  PLAYIT_SIGN_PIN_FILE     File containing the YubiKey PIN; otherwise prompt privately
  PLAYIT_SIGN_CERTIFICATE  Certificate chain (default: build-scripts/digicert.pem)
  PLAYIT_SIGN_ALIAS        Certificate alias, when the token has multiple certificates
  PLAYIT_TIMESTAMP_URL     RFC3161 server (default: http://timestamp.digicert.com)

Outputs: target/windows-sign/<version>/
Requires curl, Java, osslsigncode, and Yubico YKCS11.
HELP
}

case "${1:-}" in
  -h|--help) usage; exit 0 ;;
  '') usage >&2; exit 2 ;;
esac
[[ $# -eq 1 ]] || { usage >&2; exit 2; }
version="${1#v}"
[[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || { echo "Expected a version like 1.2.3, got: $1" >&2; exit 2; }

for tool in curl java osslsigncode; do
  command -v "$tool" >/dev/null || { echo "Install $tool before signing" >&2; exit 1; }
done
certificate="${PLAYIT_SIGN_CERTIFICATE:-$SCRIPT_DIR/digicert.pem}"
[[ -r "$certificate" ]] || { echo "Cannot read certificate chain: $certificate" >&2; exit 1; }

assets=()
for arch in x86 x86_64; do
  assets+=("playit-windows-$arch.exe" "playitd-windows-$arch.exe" "playit-windows-$arch.msi")
done

output="$REPO_DIR/target/windows-sign/$version"
download="$output/unsigned"
rm -rf "$output"
mkdir -p "$download"

base_url="https://github.com/playit-cloud/playit-agent/releases/download/v$version"
for asset in "${assets[@]}"; do
  echo "Downloading $asset"
  curl -fsSL --retry 3 -o "$download/$asset" "$base_url/$asset"
done

# Download everything before asking for the PIN, so a missing asset fails early.
if [[ -n "${PLAYIT_SIGN_PIN_FILE:-}" ]]; then
  [[ -r "$PLAYIT_SIGN_PIN_FILE" ]] || { echo 'Cannot read PLAYIT_SIGN_PIN_FILE' >&2; exit 1; }
  password="file:$PLAYIT_SIGN_PIN_FILE"
else
  read -r -s -p 'YubiKey PIN: ' PLAYIT_SIGN_PIN < /dev/tty
  printf '\n' > /dev/tty
  [[ -n "$PLAYIT_SIGN_PIN" ]] || { echo 'The YubiKey PIN must not be empty' >&2; exit 1; }
  export PLAYIT_SIGN_PIN
  password=env:PLAYIT_SIGN_PIN
fi
trap 'unset PLAYIT_SIGN_PIN' EXIT

sign_args=(--storetype YUBIKEY --keystore PIV --storepass "$password" --certfile "$certificate"
  --alg SHA-256 --tsaurl "${PLAYIT_TIMESTAMP_URL:-http://timestamp.digicert.com}" --tsmode RFC3161
  --name playit --url https://playit.gg --replace)
if [[ -n "${PLAYIT_SIGN_ALIAS:-}" ]]; then sign_args+=(--alias "$PLAYIT_SIGN_ALIAS"); fi

signed=()
for asset in "${assets[@]}"; do
  target="$output/${asset%.*}-signed.${asset##*.}"
  cp "$download/$asset" "$target"
  echo "Signing $asset (touch the YubiKey if requested)"
  java -jar "$SCRIPT_DIR/jsign-5.0.jar" "${sign_args[@]}" "$target"
  osslsigncode verify -in "$target" >/dev/null
  signed+=("${target##*/}")
done

(cd "$output" && sha256sum -- "${signed[@]}" > SHA256SUMS)
echo "Signed Windows artifacts: $output"
