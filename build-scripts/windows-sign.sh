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

MSI_DIR="${SCRIPT_DIR}/../target/win-sign"
rm -r "${MSI_DIR}"; mkdir -p "$MSI_DIR"

curl -L -o "${MSI_DIR}/playit-windows-x86.msi" https://github.com/playit-cloud/playit-agent/releases/download/v${VERSION}/playit-windows-x86.msi
curl -L -o "${MSI_DIR}/playit-windows-x86_64.msi" https://github.com/playit-cloud/playit-agent/releases/download/v${VERSION}/playit-windows-x86_64.msi

jsign --storetype YUBIKEY --tsaurl http://ts.ssl.com --storepass "${PIN}" --tsmode RFC3161 "${MSI_DIR}/playit-windows-x86.msi" \
 && mv "${MSI_DIR}/playit-windows-x86.msi" "${MSI_DIR}/playit-windows-x86-signed.msi"

jsign --storetype YUBIKEY --tsaurl http://ts.ssl.com --storepass "${PIN}" --tsmode RFC3161 "${MSI_DIR}/playit-windows-x86_64.msi" \
 && mv "${MSI_DIR}/playit-windows-x86_64.msi" "${MSI_DIR}/playit-windows-x86_64-signed.msi"
