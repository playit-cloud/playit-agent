#!/usr/bin/env bash
set -euo pipefail

export HOME=/cache/home
export WINEPREFIX=/cache/wine
mkdir -p "$HOME"

case "${1:-}" in
  build)
    target="${2:?missing Rust target}"
    cargo xwin build --locked --release --target "$target" --target-dir /build/cargo \
      --package playit-cli --package playitd --package playitd-tray
    mkdir -p "/build/stage/$target/release"
    for program in playit-cli playitd playitd-tray; do
      cp "/build/cargo/$target/release/$program.exe" "/build/stage/$target/release/$program.exe"
    done
    ;;
  package)
    shift
    exec xvfb-run -a bash /work/build-scripts/windows/package-msi.sh "$@"
    ;;
  *) echo 'Expected build or package' >&2; exit 2 ;;
esac
