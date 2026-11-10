#!/usr/bin/env bash
# Runs inside the packaging container. Builds the MSI the way `cargo wix` does on the
# Windows CI runners: the same main.wxs, en-us.wxl, WiX extensions, and variables.
set -euo pipefail

arch="${1:?missing MSI architecture (x64 or x86)}"
triple="${2:?missing Rust target triple}"
suffix="${3:?missing release suffix}"
version="${4:?missing version}"

export HOME=/cache/home WINEPREFIX=/cache/wine
mkdir -p "$HOME" "/build/wix/$suffix" /build/msi

if [[ ! -f "$WINEPREFIX/system.reg" ]]; then
  echo 'Initializing Wine'
  wineboot --init
  wineserver -w
fi
# Let Wine save its registry before the container exits.
trap 'wineserver -w' EXIT

# main.wxs reads the programs from $(CargoTargetDir)\$(TargetTriple)\$(CargoProfile).
bin="/build/stage/$triple/release"
object="$(winepath -w "/build/wix/$suffix/main.wixobj")"
msi="/build/msi/playit-windows-$suffix.msi"

echo "Compiling installer definition ($suffix)"
wine /opt/wix/candle.exe -nologo -arch "$arch" -ext WixUtilExtension \
  "-dVersion=$version" "-dTargetTriple=$triple" '-dCargoProfile=release' \
  "-dCargoTargetDir=$(winepath -w /build/stage)" "-dCargoTargetBinDir=$(winepath -w "$bin")" \
  -out "$object" "$(winepath -w /src/packages/playit-cli/wix/main.wxs)"

# ICE validation needs Windows Installer, which Wine does not provide.
echo "Linking MSI ($suffix)"
wine /opt/wix/light.exe -nologo -sval -ext WixUIExtension -ext WixUtilExtension \
  -cultures:en-US -loc "$(winepath -w /src/packages/playit-cli/wix/en-us.wxl)" \
  -b "$(winepath -w /src)" -out "$(winepath -w "$msi")" "$object"

echo "Checking the MSI contains the signed programs unchanged ($suffix)"
extract="/build/wix/$suffix/extracted"
rm -rf "$extract"
mkdir -p "$extract"
msiextract -C "$extract" "$msi" > /dev/null
python3 /scripts/verify-msi.py "$bin" "$extract"
