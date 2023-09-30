SCRIPT_DIR=$( cd -- "$( dirname -- "${BASH_SOURCE[0]}" )" &> /dev/null && pwd )

set -e

VERSION=$2
DOWNLOAD_URL="https://github.com/playit-cloud/playit-agent/releases/download/v${VERSION}"
TMP="${SCRIPT_DIR}/../target/github"

rm -rf "${TMP}"
mkdir -p "${TMP}"

curl -o "${TMP}/playit-linux-aarch64" "${DOWNLOAD_URL}/playit-linux-aarch64"
curl -o "${TMP}/playit-linux-amd64" "${DOWNLOAD_URL}/playit-linux-amd64"
curl -o "${TMP}/playit-linux-armv7" "${DOWNLOAD_URL}/playit-linux-armv7"
curl -o "${TMP}/playit-linux-i686" "${DOWNLOAD_URL}/playit-linux-i686"
curl -o "${TMP}/playit-linux-mips" "${DOWNLOAD_URL}/playit-linux-mips"
curl -o "${TMP}/playit-linux-mipsel" "${DOWNLOAD_URL}/playit-linux-mipsel"

bash "${SCRIPT_DIR}/package-linux-deb.sh" "${TMP}/playit-linux-amd64" amd64
bash "${SCRIPT_DIR}/package-linux-deb.sh" "${TMP}/playit-linux-arm64" arm64
bash "${SCRIPT_DIR}/package-linux-deb.sh" "${TMP}/playit-linux-armv7" armhf
bash "${SCRIPT_DIR}/package-linux-deb.sh" "${TMP}/playit-linux-i686" i386
bash "${SCRIPT_DIR}/package-linux-deb.sh" "${TMP}/playit-linux-mipsel" mipsel
bash "${SCRIPT_DIR}/package-linux-deb.sh" "${TMP}/playit-linux-mips" mips

cp target/deb/playit_amd64.deb "${SCRIPT_DIR}/../../ppa/data/playit_${VERSION}_amd64.deb"
cp target/deb/playit_arm64.deb "${SCRIPT_DIR}/../../ppa/data/playit_${VERSION}_arm64.deb"
cp target/deb/playit_armhf.deb "${SCRIPT_DIR}/../../ppa/data/playit_${VERSION}_armhf.deb"
cp target/deb/playit_i386.deb "${SCRIPT_DIR}/../../ppa/data/playit_${VERSION}_i386.deb"
cp target/deb/playit_mipsel.deb "${SCRIPT_DIR}/../../ppa/data/playit_${VERSION}_mipsel.deb"
cp target/deb/playit_mips.deb "${SCRIPT_DIR}/../../ppa/data/playit_${VERSION}_mips.deb"
