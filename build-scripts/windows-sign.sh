# Install library file with https://developers.yubico.com/yubico-piv-tool/YKCS11/Supported_applications/pkcs11tool.html

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

java -jar "${SCRIPT_DIR}/jsign-5.0.jar" --keystore /usr/local/lib/libykcs11.so --storetype YUBIKEY --tsaurl http://ts.ssl.com --storepass "${PIN}" --tsmode RFC3161 "${SIGN_DIR}/playit-windows-x86.msi" \
 && mv "${SIGN_DIR}/playit-windows-x86.msi" "${SIGN_DIR}/playit-windows-x86-signed.msi"

java -jar "${SCRIPT_DIR}/jsign-5.0.jar" --keystore /usr/local/lib/libykcs11.so --storetype YUBIKEY --tsaurl http://ts.ssl.com --storepass "${PIN}" --tsmode RFC3161 "${SIGN_DIR}/playit-windows-x86_64.msi" \
 && mv "${SIGN_DIR}/playit-windows-x86_64.msi" "${SIGN_DIR}/playit-windows-x86_64-signed.msi"

java -jar "${SCRIPT_DIR}/jsign-5.0.jar" --keystore /usr/local/lib/libykcs11.so --storetype YUBIKEY --tsaurl http://ts.ssl.com --storepass "${PIN}" --tsmode RFC3161 "${SIGN_DIR}/playit-windows-x86.exe" \
 && mv "${SIGN_DIR}/playit-windows-x86.exe" "${SIGN_DIR}/playit-windows-x86-signed.exe"

java -jar "${SCRIPT_DIR}/jsign-5.0.jar" --keystore /usr/local/lib/libykcs11.so --storetype YUBIKEY --tsaurl http://ts.ssl.com --storepass "${PIN}" --tsmode RFC3161 "${SIGN_DIR}/playit-windows-x86_64.exe" \
 && mv "${SIGN_DIR}/playit-windows-x86_64.exe" "${SIGN_DIR}/playit-windows-x86_64-signed.exe"
