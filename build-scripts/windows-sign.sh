#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
REPO_DIR="$(cd -- "$SCRIPT_DIR/.." && pwd)"

usage() {
  cat <<'HELP'
Usage: build-scripts/windows-sign.sh [--skip-sign] [--from DIR] <version>

Download the unsigned Windows programs for <version> from its GitHub release, sign them
with the locally connected YubiKey, package them into MSIs, and sign the MSIs. The MSIs
are built with WiX 3 under Wine in Docker, from the wix/ sources at tag v<version>, the
same way `cargo wix` builds them in CI. Nothing is uploaded.

Options:
  --skip-sign   Package without signing, to test the MSI build without the YubiKey
  --from DIR    Use the release assets in DIR instead of downloading them

Environment:
  PLAYIT_SIGN_PIN_FILE     File containing the YubiKey PIN; otherwise prompt privately
  PLAYIT_SIGN_CERTIFICATE  Certificate chain (default: build-scripts/digicert.pem)
  PLAYIT_SIGN_ALIAS        Certificate alias, when the token has multiple certificates
  PLAYIT_TIMESTAMP_URL     RFC3161 server (default: http://timestamp.digicert.com)

Outputs: target/windows-sign/<version>/
Requires Docker and git. Signing also requires Java, osslsigncode, yubico-piv-tool,
Yubico YKCS11, and pcscd.
HELP
}

skip_sign=false
from=''
version=''
while [[ $# -gt 0 ]]; do
  case "$1" in
    --skip-sign|--no-sign) skip_sign=true; shift ;;
    --from)
      [[ $# -ge 2 ]] || { echo '--from requires a directory' >&2; exit 2; }
      from="$(cd -- "$2" && pwd)"; shift 2 ;;
    -h|--help) usage; exit 0 ;;
    -*) echo "Unknown option: $1" >&2; usage >&2; exit 2 ;;
    *) [[ -z "$version" ]] || { usage >&2; exit 2; }; version="${1#v}"; shift ;;
  esac
done
[[ -n "$version" ]] || { usage >&2; exit 2; }
[[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || { echo "Expected a version like 1.2.3, got: $version" >&2; exit 2; }

tools=(curl docker git)
if ! "$skip_sign"; then tools+=(java osslsigncode yubico-piv-tool); fi
for tool in "${tools[@]}"; do
  command -v "$tool" >/dev/null || { echo "Install $tool first" >&2; exit 1; }
done
certificate="${PLAYIT_SIGN_CERTIFICATE:-$SCRIPT_DIR/digicert.pem}"
if ! "$skip_sign"; then
  [[ -r "$certificate" ]] || { echo "Cannot read certificate chain: $certificate" >&2; exit 1; }
fi
docker_cmd=(docker)
if ! docker info >/dev/null 2>&1; then
  if command -v sudo >/dev/null && sudo -n docker info >/dev/null 2>&1; then
    docker_cmd=(sudo -n docker)
  else
    echo 'Docker must be running and accessible to this user' >&2
    exit 1
  fi
fi

# Release asset name prefix -> file name the installer expects (see wix/main.wxs).
programs=(playit:playit-cli playitd:playitd playitd-tray:playitd-tray playit-gui:playit-gui)
suffixes=(x86_64 x86)
triple_for() { case "$1" in x86_64) echo x86_64-pc-windows-msvc ;; x86) echo i686-pc-windows-msvc ;; esac; }
wix_arch_for() { case "$1" in x86_64) echo x64 ;; x86) echo x86 ;; esac; }

output="$REPO_DIR/target/windows-sign/$version"
work="$output/work"
cache="$REPO_DIR/target/windows-sign/cache"
rm -rf "$output"
mkdir -p "$work/unsigned" "$work/src" "$work/stage" "$cache"

# Package from the release's own installer sources, not whatever is checked out now.
tag="v$version"
if ! git -C "$REPO_DIR" rev-parse -q --verify "refs/tags/$tag" >/dev/null; then
  echo "Fetching tag $tag"
  git -C "$REPO_DIR" fetch -q origin "refs/tags/$tag:refs/tags/$tag"
fi
git -C "$REPO_DIR" archive "$tag" packages/playit-cli/wix | tar -x -C "$work/src"

base_url="https://github.com/playit-cloud/playit-agent/releases/download/$tag"
for suffix in "${suffixes[@]}"; do
  for entry in "${programs[@]}"; do
    asset="${entry%%:*}-windows-$suffix.exe"
    if [[ -n "$from" ]]; then
      cp "$from/$asset" "$work/unsigned/$asset"
    else
      echo "Downloading $asset"
      curl -fsSL --retry 3 -o "$work/unsigned/$asset" "$base_url/$asset" \
        || { echo "Release $tag has no $asset. Releases built before CI uploaded every program lack it; use --from." >&2; exit 1; }
    fi
  done
done

sign() { :; }
if ! "$skip_sign"; then
  # jsign reports a missing YubiKey as "token only has 0 slots"; check it ourselves first.
  # Everything is downloaded before asking for the PIN, so a missing asset fails early too.
  if ! piv_status="$(yubico-piv-tool -a status 2>&1)"; then
    cat >&2 <<HELP
Cannot reach the YubiKey through PC/SC:
$piv_status

Check that:
  - the YubiKey is plugged in (ykman list or lsusb should show it)
  - pcscd is running: sudo systemctl start pcscd.socket
  - GnuPG is not holding the card: gpgconf --kill scdaemon
HELP
    exit 1
  fi

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

  sign() {
    echo "Signing ${1##*/} (touch the YubiKey if requested)"
    java -jar "$SCRIPT_DIR/jsign-5.0.jar" "${sign_args[@]}" "$1"
    osslsigncode verify -in "$1" >/dev/null
  }
fi

image=playit-windows-msi:wix3.14.1
echo 'Preparing the MSI packaging image'
"${docker_cmd[@]}" build -q --tag "$image" "$SCRIPT_DIR/windows" >/dev/null
uid="${SUDO_UID:-$(id -u)}"
gid="${SUDO_GID:-$(id -g)}"

name_suffix=-signed
if "$skip_sign"; then name_suffix=''; fi
published=()
for suffix in "${suffixes[@]}"; do
  triple="$(triple_for "$suffix")"
  bin="$work/stage/$triple/release"
  mkdir -p "$bin"
  # Sign the programs first so the MSI packs the signed copies.
  for entry in "${programs[@]}"; do
    asset="${entry%%:*}-windows-$suffix.exe"
    cp "$work/unsigned/$asset" "$bin/${entry#*:}.exe"
    sign "$bin/${entry#*:}.exe"
  done

  echo "Packaging playit-windows-$suffix.msi"
  "${docker_cmd[@]}" run --rm --init --user "$uid:$gid" \
    --mount "type=bind,src=$work/src,dst=/src,readonly" \
    --mount "type=bind,src=$SCRIPT_DIR/windows,dst=/scripts,readonly" \
    --mount "type=bind,src=$work,dst=/build" \
    --mount "type=bind,src=$cache,dst=/cache" \
    "$image" xvfb-run -a bash /scripts/package-msi.sh \
      "$(wix_arch_for "$suffix")" "$triple" "$suffix" "$version"
  sign "$work/msi/playit-windows-$suffix.msi"

  for entry in "${programs[@]}"; do
    file="${entry%%:*}-windows-$suffix$name_suffix.exe"
    cp "$bin/${entry#*:}.exe" "$output/$file"
    published+=("$file")
  done
  file="playit-windows-$suffix$name_suffix.msi"
  cp "$work/msi/playit-windows-$suffix.msi" "$output/$file"
  chmod 644 "$output/$file"
  published+=("$file")
done

(cd "$output" && sha256sum -- "${published[@]}" > SHA256SUMS)
echo "Windows artifacts: $output"
