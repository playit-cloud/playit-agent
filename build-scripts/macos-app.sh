SCRIPT_DIR=$( cd -- "$( dirname -- "${BASH_SOURCE[0]}" )" &> /dev/null && pwd )

# build app
cd "${SCRIPT_DIR}/../"

# for building on M1 Mac
cargo build --release --bin=agent
cargo build --target=x86_64-apple-darwin --release --bin=agent

TEMP_DIR_NAME="playit-macos"
APP_DIR="${SCRIPT_DIR}/${TEMP_DIR_NAME}/playit.app"

rm -rf "${SCRIPT_DIR}/${TEMP_DIR_NAME}"
mkdir -p "${APP_DIR}/Contents"
mkdir "${APP_DIR}/Contents/MacOS"
mkdir "${APP_DIR}/Contents/Resources"

echo '<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple Computer//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
' > "${APP_DIR}/Contents/Info.plist"

echo "
  <dict>
    <key>CFBundleExecutable</key>
    <string>playit</string>
    <key>CFBundleIconFile</key>
    <string>playit.icns</string>
    <key>LSArchitecturePriority</key>
    <array>
      <string>arm64</string>
      <string>x86_64</string>
    </array>
  </dict>
</plist>
" >> "${APP_DIR}/Contents/Info.plist"

cp "${SCRIPT_DIR}/../assets/playit.icns" "${APP_DIR}/Contents/Resources"

cp "${SCRIPT_DIR}/../target/release/agent" "${APP_DIR}/Contents/MacOS/agent-m1"
cp "${SCRIPT_DIR}/../target/x86_64-apple-darwin/release/agent" "${APP_DIR}/Contents/MacOS/agent-intel"
cp "${SCRIPT_DIR}/macos-run.sh" "${APP_DIR}/Contents/MacOS/playit"

chmod +x "${APP_DIR}/Contents/MacOS/agent-m1"
chmod +x "${APP_DIR}/Contents/MacOS/agent-intel"
chmod +x "${APP_DIR}/Contents/MacOS/playit"

# build install DMG

ln -s /Applications "${SCRIPT_DIR}/${TEMP_DIR_NAME}/Applications"

cd "${SCRIPT_DIR}"

rm playit-tmp.dmg
rm out/playit.dmg

hdiutil create playit-tmp.dmg -ov -volname "Playit Install" -fs HFS+ -srcfolder "${TEMP_DIR_NAME}"
hdiutil convert playit-tmp.dmg -format UDZO -o out/playit.dmg

rm playit-tmp.dmg
