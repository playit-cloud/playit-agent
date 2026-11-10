#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'HELP'
Usage: build-scripts/macos/install-signing-tools.sh [--smartcard]

Installs the tools sign-macos.sh needs on Linux into target/macos-sign-tools/bin:
  rcodesign  apple-codesign 0.29.0, which signs, notarizes, and staples
  mkbom      bomutils, which writes the installer's bill of materials

--smartcard builds rcodesign from source with YubiKey support. This needs Rust and
libpcsclite-dev. Without it, the prebuilt rcodesign is downloaded and checksummed;
that build signs only with .p12 or PEM files.

xar must be installed separately (Debian/Ubuntu: apt-get install xar).
HELP
}

smartcard=false
case "${1:-}" in
  "") ;;
  --smartcard) smartcard=true ;;
  -h|--help) usage; exit 0 ;;
  *) echo "Unknown argument: $1" >&2; usage >&2; exit 2 ;;
esac

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
REPO_DIR="$(cd -- "$SCRIPT_DIR/../.." && pwd)"
TOOLS_DIR="$REPO_DIR/target/macos-sign-tools"
RCODESIGN_VERSION=0.29.0
BOMUTILS_COMMIT=c247002e2f49878bab32dd0fcf805810ae0992cb

[[ "$(uname -s)" = Linux ]] || { echo 'These tools are for Linux; on macOS use package-macos.sh' >&2; exit 1; }
mkdir -p "$TOOLS_DIR/bin" "$TOOLS_DIR/src"

if "$smartcard"; then
  echo "Building rcodesign $RCODESIGN_VERSION with smartcard support"
  cargo install apple-codesign --version "$RCODESIGN_VERSION" --locked --features smartcard \
    --root "$TOOLS_DIR" --force
else
  case "$(uname -m)" in
    x86_64) triple=x86_64-unknown-linux-musl
      sha256=dbe85cedd8ee4217b64e9a0e4c2aef92ab8bcaaa41f20bde99781ff02e600002 ;;
    aarch64) triple=aarch64-unknown-linux-musl
      sha256=4af92c87ddf52f5f2d1258a3b4e56c7dcb8f1b2468df744976c5f139e031961f ;;
    *) echo "No prebuilt rcodesign for $(uname -m); use --smartcard to build it" >&2; exit 1 ;;
  esac
  name="apple-codesign-$RCODESIGN_VERSION-$triple"
  echo "Downloading rcodesign $RCODESIGN_VERSION ($triple)"
  curl -fsSL -o "$TOOLS_DIR/src/$name.tar.gz" \
    "https://github.com/indygreg/apple-platform-rs/releases/download/apple-codesign%2F$RCODESIGN_VERSION/$name.tar.gz"
  echo "$sha256  $TOOLS_DIR/src/$name.tar.gz" | sha256sum --check --quiet
  tar -xzf "$TOOLS_DIR/src/$name.tar.gz" -C "$TOOLS_DIR/src"
  install -m 0755 "$TOOLS_DIR/src/$name/rcodesign" "$TOOLS_DIR/bin/rcodesign"
fi

echo "Building bomutils $BOMUTILS_COMMIT"
bomutils="$TOOLS_DIR/src/bomutils"
if [[ ! -d "$bomutils/.git" ]]; then
  git clone --quiet https://github.com/hogliux/bomutils "$bomutils"
fi
git -C "$bomutils" fetch --quiet origin "$BOMUTILS_COMMIT"
git -C "$bomutils" checkout --quiet --detach "$BOMUTILS_COMMIT"
make -C "$bomutils" --quiet >/dev/null
install -m 0755 "$bomutils/build/bin/mkbom" "$bomutils/build/bin/lsbom" "$TOOLS_DIR/bin/"

"$TOOLS_DIR/bin/rcodesign" --version
if ! command -v xar >/dev/null; then
  echo 'warning: xar is not installed; install it before signing (apt-get install xar)' >&2
fi
echo "Installed signing tools in $TOOLS_DIR/bin"
