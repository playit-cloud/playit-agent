#!/usr/bin/env bash
set -euo pipefail
source /work/build-scripts/windows/log.sh

export HOME=/cache/home
export WINEPREFIX=/cache/wine
mkdir -p "$HOME"

case "${1:-}" in
  build)
    target="${2:?missing Rust target}"
    log "Compiling $target; the first build may download the Microsoft SDK and CRT"
    # Missing CRT PDBs affect debugging, not the release executable.
    cargo xwin build --locked --release --target "$target" --target-dir /build/cargo \
      --config "target.$target.rustflags=['-C', 'link-arg=/IGNORE:4099']" \
      --package playit-cli --package playitd --package playitd-tray
    mkdir -p "/build/stage/$target/release"
    log "Staging executables for $target"
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
