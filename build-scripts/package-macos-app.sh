SCRIPT_DIR=$( cd -- "$( dirname -- "${BASH_SOURCE[0]}" )" &> /dev/null && pwd )

INTEL_PATH="$1"
ARM_PATH="$2"

TEMP_DIR_NAME="playit-macos"
APP_DIR="${SCRIPT_DIR}/${TEMP_DIR_NAME}/playit.app"

# shellcheck disable=SC2115
rm -rf "${SCRIPT_DIR}/${TEMP_DIR_NAME}" 2> /dev/null || true
mkdir -p "${APP_DIR}/Contents"
mkdir "${APP_DIR}/Contents/MacOS"
mkdir "${APP_DIR}/Contents/Resources"

cat <<EOF > "${APP_DIR}/Contents/Info.p1list"
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple Computer//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
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
EOF

cp "${SCRIPT_DIR}/../assets/playit.icns" "${APP_DIR}/Contents/Resources"
cp "${ARM_PATH}" "${APP_DIR}/Contents/MacOS/agent-arm"
cp "${INTEL_PATH}" "${APP_DIR}/Contents/MacOS/agent-intel"

cat <<EOF > "${APP_DIR}/Contents/MacOS/playit"
#!/usr/bin/env bash

SCRIPT_DIR=$( cd -- "$( dirname -- "${BASH_SOURCE[0]}" )" &> /dev/null && pwd )

mkdir -p ~/.config/playit
mkdir -p ~/Library/Logs/playit

cd ~/.config/playit
CONFIG_PATH=$(pwd)

cd ~/Library/Logs/playit
LOGS_PATH=$(pwd)

if [[ $(uname -p) == 'arm' ]]; then
  osascript -e "tell app \"Terminal\"
    do script \" ${SCRIPT_DIR}/agent-arm --secret_path=${CONFIG_PATH}/playit.toml\"
  end tell"
else
  osascript -e "tell app \"Terminal\"
    do script \" ${SCRIPT_DIR}/agent-intel --secret_path=${CONFIG_PATH}/playit.toml\"
  end tell"
fi
EOF

chmod +x "${APP_DIR}/Contents/MacOS/agent-arm"
chmod +x "${APP_DIR}/Contents/MacOS/agent-intel"
chmod +x "${APP_DIR}/Contents/MacOS/playit"

# build install DMG

ln -s /Applications "${SCRIPT_DIR}/${TEMP_DIR_NAME}/Applications"

# shellcheck disable=SC2164
cd "${SCRIPT_DIR}"

rm playit-tmp.dmg 2> /dev/null || true
rm "${SCRIPT_DIR}/../target/mac/playit.dmg" 2> /dev/null || true

hdiutil create playit-tmp.dmg -ov -volname "Playit Install" -fs HFS+ -srcfolder "${TEMP_DIR_NAME}"
hdiutil convert playit-tmp.dmg -format UDZO -o "${SCRIPT_DIR}/../target/mac/playit.dmg"
rm playit-tmp.dmg
