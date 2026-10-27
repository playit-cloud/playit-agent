#!/usr/bin/env bash
set +x
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
REPO_DIR="$(cd -- "$SCRIPT_DIR/.." && pwd)"

usage() {
  cat <<'HELP'
Usage: ./windows-build.sh [--skip-sign] [--arch all|x86_64|x86]

Build Windows executables and MSI installers entirely on Linux.
Signing with the locally connected YubiKey is enabled by default.
--skip-sign (or --no-sign) builds unsigned artifacts for testing.

Environment:
  PLAYIT_SIGN_PIN_FILE     File containing the YubiKey PIN; otherwise prompt privately
  PLAYIT_SIGN_CERTIFICATE  Certificate chain (default: build-scripts/digicert.pem)
  PLAYIT_SIGN_ALIAS        Certificate alias, when the token has multiple certificates
  PLAYIT_TIMESTAMP_URL     RFC3161 server (default: http://timestamp.digicert.com)
  CARGO_BUILD_JOBS          Compiler concurrency (default: 2)

Outputs: target/windows-release/<version>/signed/ or unsigned/
Requires Docker. Signed builds also require Java, osslsigncode, and Yubico YKCS11.
HELP
}

skip_sign=false
architectures=all
while [[ $# -gt 0 ]]; do
  case "$1" in
    --skip-sign|--no-sign) skip_sign=true; shift ;;
    --arch)
      [[ $# -ge 2 ]] || { echo '--arch requires a value' >&2; exit 2; }
      architectures="$2"; shift 2 ;;
    -h|--help) usage; exit 0 ;;
    *) echo "Unknown argument: $1" >&2; usage >&2; exit 2 ;;
  esac
done
case "$architectures" in all|x86_64|x86) ;; *) echo 'Use --arch all, x86_64, or x86' >&2; exit 2 ;; esac
[[ "$(uname -s)" = Linux && "$(uname -m)" = x86_64 ]] || {
  echo 'This pipeline requires an x86_64 Linux host' >&2; exit 1;
}
command -v docker >/dev/null || { echo 'Install Docker before building' >&2; exit 1; }
docker info >/dev/null 2>&1 || { echo 'Docker must be running and accessible to this user' >&2; exit 1; }

version="$(awk '/^\[workspace.package\]/{p=1;next} p && /^\[/{exit} p && /^version = /{gsub(/"/, "", $3);print $3;exit}' "$REPO_DIR/Cargo.toml")"
[[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || { echo 'Expected a three-part workspace release version' >&2; exit 1; }
mode=signed
if "$skip_sign"; then mode=unsigned; fi

certificate="${PLAYIT_SIGN_CERTIFICATE:-$SCRIPT_DIR/digicert.pem}"
sign_args=()
if ! "$skip_sign"; then
  for tool in java osslsigncode; do
    command -v "$tool" >/dev/null || { echo "Install $tool, or use --skip-sign for testing" >&2; exit 1; }
  done
  [[ -r "$certificate" ]] || { echo "Cannot read certificate chain: $certificate" >&2; exit 1; }
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
  sign_args=(--storetype YUBIKEY --keystore PIV --storepass "$password" --certfile "$certificate"
    --alg SHA-256 --tsaurl "${PLAYIT_TIMESTAMP_URL:-http://timestamp.digicert.com}" --tsmode RFC3161
    --name playit --url https://playit.gg --replace)
  if [[ -n "${PLAYIT_SIGN_ALIAS:-}" ]]; then sign_args+=(--alias "$PLAYIT_SIGN_ALIAS"); fi
fi
trap 'unset PLAYIT_SIGN_PIN' EXIT

build_root="$REPO_DIR/target/windows-build"
work="$build_root/work"
cache="$build_root/cache"
output="$REPO_DIR/target/windows-release/$version/$mode"
uid="${SUDO_UID:-$(id -u)}"
gid="${SUDO_GID:-$(id -g)}"
mkdir -p "$work" "$cache" "$output"
touch "$build_root/.lock"
if [[ "$EUID" = 0 ]]; then
  chown "$uid:$gid" "$build_root" "$build_root/.lock" "$work" "$cache" \
    "$REPO_DIR/target/windows-release" "$REPO_DIR/target/windows-release/$version" "$output"
fi
exec 9> "$build_root/.lock"
flock -n 9 || { echo 'Another Windows build is using this checkout' >&2; exit 1; }
rm -rf "$work/stage" "$work/msi"

image=playit-windows-builder:rust1.99.1-xwin0.23.1
docker build --tag "$image" "$SCRIPT_DIR/windows"
container() {
  docker run --rm --init --platform linux/amd64 --user "$uid:$gid" \
    --mount "type=bind,src=$REPO_DIR,dst=/work,readonly" \
    --mount "type=bind,src=$work,dst=/build" \
    --mount "type=bind,src=$cache,dst=/cache" \
    -e "CARGO_BUILD_JOBS=${CARGO_BUILD_JOBS:-2}" \
    "$image" bash /work/build-scripts/windows/build-container.sh "$@"
}
sign() {
  if ! "$skip_sign"; then
    java -jar "$SCRIPT_DIR/jsign-5.0.jar" "${sign_args[@]}" "$1"
    osslsigncode verify -in "$1"
  fi
}

suffixes=(x86_64 x86)
if [[ "$architectures" != all ]]; then suffixes=("$architectures"); fi
for suffix in "${suffixes[@]}"; do
  case "$suffix" in
    x86_64) target=x86_64-pc-windows-msvc; arch=x64 ;;
    x86) target=i686-pc-windows-msvc; arch=x86 ;;
  esac
  echo "Building $version for $suffix ($mode)"
  container build "$target"
  # Sign only staged copies. Cargo's unsigned build cache remains reusable.
  for program in playit-cli playitd playitd-tray; do
    sign "$work/stage/$target/release/$program.exe"
  done
  container package "$target" "$arch" "$suffix" "$version"
  sign "$work/msi/playit-windows-$suffix.msi"
done

# Publish only after every requested architecture has built and passed verification.
rm -f "$output"/playit-windows-*.exe "$output"/playit-windows-*.msi \
  "$output"/playitd-windows-*.exe "$output"/playitd-tray-windows-*.exe
for suffix in "${suffixes[@]}"; do
  case "$suffix" in x86_64) target=x86_64-pc-windows-msvc ;; x86) target=i686-pc-windows-msvc ;; esac
  cp "$work/stage/$target/release/playit-cli.exe" "$output/playit-windows-$suffix.exe"
  cp "$work/stage/$target/release/playitd.exe" "$output/playitd-windows-$suffix.exe"
  cp "$work/stage/$target/release/playitd-tray.exe" "$output/playitd-tray-windows-$suffix.exe"
  cp "$work/msi/playit-windows-$suffix.msi" "$output/playit-windows-$suffix.msi"
done
if ! "$skip_sign"; then
  for file in "$output"/*.exe "$output"/*.msi; do
    cp "$file" "${file%.*}-signed.${file##*.}"
  done
fi
(cd "$output" && sha256sum -- *.exe *.msi > SHA256SUMS)
if [[ "$EUID" = 0 ]]; then chown "$uid:$gid" "$output"/*; fi
echo "Windows artifacts: $output"
