SCRIPT_DIR=$( cd -- "$( dirname -- "${BASH_SOURCE[0]}" )" &> /dev/null && pwd )

VERSION="$1"
PIN="$2"

if [ -z "$VERSION" ]; then
  echo "missing verison"
  exit 1;
fi

if [ -z "$PIN" ]; then
  echo "missing pin"
  exit 1;
fi

SIGN_DIR="${SCRIPT_DIR}/../target/win-sign"
rm -r "${SIGN_DIR}"; mkdir -p "$SIGN_DIR"

curl -L -o "${SIGN_DIR}/playit-windows-x86.msi" https://github.com/playit-cloud/playit-agent/releases/download/v${VERSION}/playit-windows-x86.msi
curl -L -o "${SIGN_DIR}/playit-windows-x86_64.msi" https://github.com/playit-cloud/playit-agent/releases/download/v${VERSION}/playit-windows-x86_64.msi

curl -L -o "${SIGN_DIR}/playit-windows-x86.exe" https://github.com/playit-cloud/playit-agent/releases/download/v${VERSION}/playit-windows-x86.exe
curl -L -o "${SIGN_DIR}/playit-windows-x86_64.exe" https://github.com/playit-cloud/playit-agent/releases/download/v${VERSION}/playit-windows-x86_64.exe

jsign --storetype YUBIKEY --tsaurl http://ts.ssl.com --storepass "${PIN}" --tsmode RFC3161 "${SIGN_DIR}/playit-windows-x86.msi" \
 && mv "${SIGN_DIR}/playit-windows-x86.msi" "${SIGN_DIR}/playit-windows-x86-signed.msi"

jsign --storetype YUBIKEY --tsaurl http://ts.ssl.com --storepass "${PIN}" --tsmode RFC3161 "${SIGN_DIR}/playit-windows-x86_64.msi" \
 && mv "${SIGN_DIR}/playit-windows-x86_64.msi" "${SIGN_DIR}/playit-windows-x86_64-signed.msi"

jsign --storetype YUBIKEY --tsaurl http://ts.ssl.com --storepass "${PIN}" --tsmode RFC3161 "${SIGN_DIR}/playit-windows-x86.exe" \
 && mv "${SIGN_DIR}/playit-windows-x86.exe" "${SIGN_DIR}/playit-windows-x86-signed.exe"

jsign --storetype YUBIKEY --tsaurl http://ts.ssl.com --storepass "${PIN}" --tsmode RFC3161 "${SIGN_DIR}/playit-windows-x86_64.exe" \
 && mv "${SIGN_DIR}/playit-windows-x86_64.exe" "${SIGN_DIR}/playit-windows-x86_64-signed.exe"
