#!/usr/bin/env bash
set -euo pipefail
source /work/build-scripts/windows/log.sh

target="${1:?missing Rust target}"
arch="${2:?missing MSI architecture}"
suffix="${3:?missing release suffix}"
version="${4:?missing version}"
mkdir -p "/build/wix/$suffix" /build/msi

if [[ ! -f "$WINEPREFIX/system.reg" ]]; then
  log 'Initializing Wine'
  wineboot --init
  log 'Waiting for Wine initialization to finish'
  wineserver -w
fi
# Let Wine save its registry before the container or virtual display exits.
trap 'log "Waiting for Wine to finish"; wineserver -w' EXIT

log "Preparing installer paths ($suffix)"
stage="$(winepath -w /build/stage)"
bin="$(winepath -w "/build/stage/$target/release")"
object="$(winepath -w "/build/wix/$suffix/main.wixobj")"
msi="/build/msi/playit-windows-$suffix.msi"

log "Compiling installer definition ($suffix)"
wine /opt/wix/candle.exe -nologo -arch "$arch" -ext WixUtilExtension \
  "-dVersion=$version" "-dTargetTriple=$target" '-dCargoProfile=release' \
  "-dCargoTargetDir=$stage" "-dCargoTargetBinDir=$bin" \
  -out "$object" 'Z:\work\packages\playit-cli\wix\main.wxs'

# ICE validation requires Windows; Windows smoke tests validate the finished installer.
log "Linking MSI ($suffix)"
wine /opt/wix/light.exe -nologo -sval -ext WixUIExtension -ext WixUtilExtension \
  -cultures:en-US -loc 'Z:\work\packages\playit-cli\wix\en-us.wxl' \
  -b 'Z:\work' -out "$(winepath -w "$msi")" "$object"

log "Extracting MSI and verifying executable hashes ($suffix)"
extract="/build/wix/$suffix/extracted"
rm -rf "$extract"
mkdir -p "$extract"
msiextract -C "$extract" "$msi" > /dev/null
python3 /work/build-scripts/windows/verify-msi.py "/build/stage/$target/release" "$extract"
