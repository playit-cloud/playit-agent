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
    # Missing CRT PDBs affect debugging, not the release executable. GPUI precompiles its
    # shaders with fxc.exe only when built on Windows; with debug assertions on, it compiles
    # them at startup with the d3dcompiler_47.dll that ships with Windows 10 and later.
    cargo xwin build --locked --release --target "$target" --target-dir /build/cargo \
      --config "target.$target.rustflags=['-C', 'link-arg=/IGNORE:4099']" \
      --config 'profile.release.package.gpui.debug-assertions=true' \
      --package playit-cli --package playitd --package playitd-tray --package playit-gui
    mkdir -p "/build/stage/$target/release"
    log "Staging executables for $target"
    for program in playit-cli playitd playitd-tray playit-gui; do
      cp "/build/cargo/$target/release/$program.exe" "/build/stage/$target/release/$program.exe"
    done
    ;;
  package)
    shift
    exec xvfb-run -a bash /work/build-scripts/windows/package-msi.sh "$@"
    ;;
  *) echo 'Expected build or package' >&2; exit 2 ;;
esac
